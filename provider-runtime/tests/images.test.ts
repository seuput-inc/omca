import { expect, test } from 'bun:test';
import { AuthStorage } from '@oh-my-pi/pi-ai';
import type { FetchImpl } from '@oh-my-pi/pi-catalog/types';
import { generateManagedImage, imageCapabilities } from '../src/images';

const png = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/aXcAAAAASUVORK5CYII=';
const signal = () => new AbortController().signal;

async function storageWithAccounts() {
  const storage = await AuthStorage.create(':memory:');
  await storage.credentials.reload();
  await storage.credentials.upsert('openai', { type: 'api_key', key: 'explicit-key', source: 'login' });
  await storage.credentials.upsert('openai-codex', { type: 'oauth', access: 'explicit-token', refresh: 'refresh', expires: Date.now() + 3600000 });
  return storage;
}

test('capabilities require supported stored selected accounts and never probe the network', async () => {
  const storage = await AuthStorage.create(':memory:');
  await storage.credentials.reload();
  const old = process.env.OPENAI_API_KEY;
  process.env.OPENAI_API_KEY = 'ambient-key';
  try {
    expect(imageCapabilities(storage, {})).toEqual([]);
    await storage.credentials.upsert('openai', { type: 'api_key', key: 'explicit-key', source: 'login' });
    await storage.credentials.upsert('openai-codex', { type: 'oauth', access: 'explicit-token', refresh: 'refresh', expires: Date.now() + 3600000 });
    expect(imageCapabilities(storage, {}).map(c => [c.provider_id, c.model_id, c.estimate_usd])).toEqual([
      ['openai', 'gpt-image-2', null], ['openai-codex', 'gpt-image-2', null],
    ]);
    expect(imageCapabilities(storage, { openai: 999999 }).map(c => c.provider_id)).toEqual(['openai-codex']);
  } finally {
    if (old === undefined) delete process.env.OPENAI_API_KEY; else process.env.OPENAI_API_KEY = old;
    storage.close();
  }
});

test('OpenAI image generation uses the selected key and native endpoint', async () => {
  const storage = await storageWithAccounts();
  const requests: { url: string; headers: Headers; body: Record<string, unknown> }[] = [];
  const mock: FetchImpl = async (url, init) => {
    requests.push({ url: String(url), headers: new Headers(init?.headers), body: JSON.parse(String(init?.body)) });
    return Response.json({ data: [{ b64_json: png }], usage: { cost: 0.125 } });
  };
  try {
    const result = await generateManagedImage(storage, {}, 'openai', 'draw a tree', 'wide', signal(), mock);
    expect(requests).toHaveLength(1);
    expect(requests[0].url).toBe('https://api.openai.com/v1/images/generations');
    expect(requests[0].headers.get('authorization')).toBe('Bearer explicit-key');
    expect(requests[0].body).toMatchObject({ model: 'gpt-image-2', prompt: 'draw a tree', n: 1, size: '1536x1024' });
    expect(result).toMatchObject({ data_base64: png, mime_type: 'image/png', model: 'gpt-image-2', cost_usd: 0.125, estimate_usd: null, cost_is_estimate: false });
  } finally { storage.close(); }
});

test('Codex uses its Responses carrier and reports unknown cost', async () => {
  const storage = await storageWithAccounts();
  const requests: { url: string; headers: Headers; body: Record<string, unknown> }[] = [];
  const mock: FetchImpl = async (url, init) => {
    requests.push({ url: String(url), headers: new Headers(init?.headers), body: JSON.parse(String(init?.body)) });
    const response = { output: [{ type: 'image_generation_call', result: png }], tools: [{ type: 'image_generation', model: 'gpt-image-2-codex' }] };
    return new Response(`data: ${JSON.stringify({ type: 'response.completed', response })}\n\ndata: [DONE]\n\n`, { headers: { 'content-type': 'text/event-stream' } });
  };
  try {
    const result = await generateManagedImage(storage, {}, 'openai-codex', 'draw a tree', 'tall', signal(), mock);
    expect(requests).toHaveLength(1);
    expect(requests[0].url).toContain('chatgpt.com/backend-api/codex/responses');
    expect(requests[0].headers.get('authorization')).toBe('Bearer explicit-token');
    expect(requests[0].body.model).toBe('gpt-5.6-luna');
    expect(result).toMatchObject({ data_base64: png, mime_type: 'image/png', model: 'gpt-image-2-codex', cost_usd: null, estimate_usd: null, cost_is_estimate: true });
  } finally { storage.close(); }
});

test('invalid requests, oversized responses and provider failures do not retry or leak errors', async () => {
  const storage = await storageWithAccounts();
  let calls = 0;
  const failure: FetchImpl = async () => { calls++; return new Response('secret upstream detail', { status: 401 }); };
  try {
    await expect(generateManagedImage(storage, {}, 'openai', '', 'square', signal(), failure)).rejects.toThrow();
    await expect(generateManagedImage(storage, {}, 'openai', 'x'.repeat(4097), 'square', signal(), failure)).rejects.toThrow();
    await expect(generateManagedImage(storage, { openai: 999999 }, 'openai', 'x', 'square', signal(), failure)).rejects.toThrow();
    expect(calls).toBe(0);
    await expect(generateManagedImage(storage, {}, 'openai', 'x', 'square', signal(), failure)).rejects.toThrow('Image generation failed');
    expect(calls).toBe(1);
    const oversized: FetchImpl = async () => new Response('x', { headers: { 'content-length': String(17 * 1024 * 1024) } });
    await expect(generateManagedImage(storage, {}, 'openai', 'x', 'square', signal(), oversized)).rejects.toThrow('Image generation failed');
    const controller = new AbortController();
    controller.abort();
    await expect(generateManagedImage(storage, {}, 'openai', 'x', 'square', controller.signal, failure)).rejects.toThrow('Cancelled');
  } finally { storage.close(); }
});
