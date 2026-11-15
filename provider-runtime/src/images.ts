import { generateImage, type AuthStorage, type ImageGenerationOptions, type ImageGenerationResult } from '@oh-my-pi/pi-ai';
import { getBundledModel } from '@oh-my-pi/pi-catalog';
import type { FetchImpl } from '@oh-my-pi/pi-catalog/types';
import { assertDirectTransport, directCredentialResolver } from './direct-auth';

type ImageProvider = 'openai' | 'openai-codex';
type Shape = 'square' | 'wide' | 'tall';
type Capability = { provider_id: ImageProvider; model_id: string; name: string; estimate_usd: number | null };

// The SDK leaves quality to the provider and Codex controls its own billing.
// Neither transport exposes enough information to guarantee a dollar upper bound.
const estimate = null;
const maxResponseBytes = 16 * 1024 * 1024;
const sizes: Record<Shape, string> = { square: '1024x1024', wide: '1536x1024', tall: '1024x1536' };

function route(storage: AuthStorage, selected: Record<string, number>, provider: ImageProvider) {
  const row = storage.credentials.list(provider).find(item =>
    !item.disabledCause && (selected[provider] === undefined || item.id === selected[provider]));
  if (!row || (provider === 'openai' ? row.credential.type !== 'api_key' || !row.credential.key : row.credential.type !== 'oauth')) return undefined;
  const catalogModel = getBundledModel(provider, 'gpt-image-2');
  const carrier = provider === 'openai-codex' ? getBundledModel(provider, 'gpt-5.6-luna') : undefined;
  if (!catalogModel || catalogModel.api !== (provider === 'openai' ? 'openai-responses' : 'openai-codex-responses')) return undefined;
  if (provider === 'openai-codex' && (!carrier || carrier.api !== 'openai-codex-responses' || !carrier.hostedImage || carrier.imageModel !== catalogModel.id)) return undefined;
  // The catalog assigns OpenAI image models to Responses; this operation uses its native Images endpoint.
  const model = provider === 'openai' ? { ...catalogModel, api: 'openai-images' as const } : catalogModel;
  assertDirectTransport(provider, model.api);
  if (carrier) assertDirectTransport(provider, carrier.api);
  return { model, carrier };
}

export function imageCapabilities(storage: AuthStorage, selected: Record<string, number>): Capability[] {
  return (['openai', 'openai-codex'] as const).flatMap(provider => {
    const found = route(storage, selected, provider);
    return found ? [{ provider_id: provider, model_id: found.model.id, name: found.model.name, estimate_usd: estimate }] : [];
  });
}

export async function generateManagedImage(
  storage: AuthStorage, selected: Record<string, number>, provider: string, prompt: unknown, shape: unknown,
  signal: AbortSignal, fetchImpl: FetchImpl = fetch,
) {
  if (signal.aborted) throw new Error('Cancelled');
  if ((provider !== 'openai' && provider !== 'openai-codex') || typeof prompt !== 'string' || !prompt.trim() || Buffer.byteLength(prompt, 'utf8') > 4096 ||
      (shape !== 'square' && shape !== 'wide' && shape !== 'tall')) throw new Error('Invalid image request');
  const found = route(storage, selected, provider);
  if (!found) throw new Error('Image route unavailable');
  const resolver = directCredentialResolver(storage, provider, selected[provider]);
  let requested = false;
  const boundedFetch: FetchImpl = async (input, init) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    const expected = provider === 'openai' ? 'https://api.openai.com/v1/images/generations' : 'https://chatgpt.com/backend-api/codex/responses';
    if (requested || url !== expected) throw new Error('Image transport rejected');
    requested = true;
    const response = await fetchImpl(input, { ...init, redirect: 'error' });
    const length = Number(response.headers.get('content-length'));
    if (Number.isFinite(length) && length > maxResponseBytes) throw new Error('Image response too large');
    if (!response.body) return response;
    let bytes = 0;
    const body = response.body.pipeThrough(new TransformStream<Uint8Array, Uint8Array>({
      transform(chunk, controller) {
        bytes += chunk.byteLength;
        if (bytes > maxResponseBytes) throw new Error('Image response too large');
        controller.enqueue(chunk);
      },
    }));
    return new Response(body, { status: response.status, statusText: response.statusText, headers: response.headers });
  };
  const options: ImageGenerationOptions = {
    apiKey: args => args.error === undefined && !args.lastChance ? resolver(args) : Promise.resolve(undefined),
    signal, fetch: boundedFetch, ...(found.carrier ? { carrier: found.carrier, sessionId: crypto.randomUUID() } : {}),
  };
  let result: ImageGenerationResult;
  try {
    result = await generateImage(found.model, { prompt, imageSize: sizes[shape], count: 1 }, options);
  } catch {
    throw new Error(signal.aborted ? 'Cancelled' : 'Image generation failed');
  }
  if (signal.aborted) throw new Error('Cancelled');
  if (result.images.length !== 1) throw new Error('Image response invalid');
  const image = result.images[0];
  if (!image || !image.data || image.data.length > maxResponseBytes || image.data.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(image.data)) throw new Error('Image response invalid');
  const header = Buffer.from(image.data.slice(0, 64), 'base64');
  const mime = header.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])) ? 'image/png'
    : header.subarray(0, 3).equals(Buffer.from([255, 216, 255])) ? 'image/jpeg'
    : header.subarray(0, 4).toString() === 'RIFF' && header.subarray(8, 12).toString() === 'WEBP' ? 'image/webp' : undefined;
  if (!mime || mime !== image.mimeType) throw new Error('Image response invalid');
  const reported = result.usage.cost.total;
  const reliable = provider === 'openai' && Number.isFinite(reported) && reported > 0;
  return { data_base64: image.data, mime_type: mime, model: result.model ?? found.model.id,
    cost_usd: reliable ? reported : null, estimate_usd: estimate, cost_is_estimate: !reliable };
}
