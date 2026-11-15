// Sign exact manifest bytes with the OMCA release key. Never print key material.
import { createHash, createPrivateKey, createPublicKey, sign } from "node:crypto";
import { readFileSync, writeFileSync, statSync } from "node:fs";
import { basename } from "node:path";

const [artifact, executable, version, kind] = process.argv.slice(2);
const platforms: Record<string, string> = { "mac-zip": "macos-aarch64", "windows-msi": "windows-x64", "windows-portable": "windows-x64" };
if (!artifact || !executable || !version || !platforms[kind] || !/^\d+\.\d+\.\d+$/.test(version)) {
  throw new Error("Usage: bun packaging/sign-update.ts ARTIFACT EXECUTABLE VERSION mac-zip|windows-msi|windows-portable");
}
const pem = process.env.OMCA_UPDATE_SIGNING_KEY_FILE
  ? readFileSync(process.env.OMCA_UPDATE_SIGNING_KEY_FILE)
  : process.env.OMCA_UPDATE_SIGNING_KEY;
if (!pem) throw new Error("OMCA update signing key is required; unsigned updates cannot be published.");
const key = createPrivateKey(pem);
if (key.asymmetricKeyType !== "ed25519") throw new Error("OMCA update signing requires an Ed25519 key.");
const pinned = "be4b2d855b01518e6f7722dc477888b68e274980ec3ca8d0da5a50a9ab179e0a";
const publicKey = createPublicKey(key).export({ type: "spki", format: "der" }).subarray(-32).toString("hex");
if (publicKey !== pinned) throw new Error("The signing key does not match OMCA's pinned update public key.");
const hash = async (path: string) => {
  const digest = createHash("sha256");
  for await (const chunk of Bun.file(path).stream()) digest.update(chunk);
  return digest.digest("hex");
};
const manifest = {
  schema: 1, app: "omca", repo: "seuput-inc/omca", version,
  platform: platforms[kind], package: kind, asset: basename(artifact),
  size: statSync(artifact).size, sha256: await hash(artifact), executable_sha256: await hash(executable),
};
const bytes = Buffer.from(JSON.stringify(manifest) + "\n");
writeFileSync(`${artifact}.manifest.json`, bytes);
writeFileSync(`${artifact}.sig`, sign(null, bytes, key));
console.log(`Signed ${basename(artifact)} for ${manifest.platform}.`);
