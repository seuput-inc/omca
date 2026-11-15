// Real helper smoke. Uses only a caller-supplied scratch folder and synthetic credentials.
import { mkdirSync, copyFileSync, writeFileSync, readFileSync, existsSync, readdirSync } from "node:fs";
import { join, resolve, basename } from "node:path";
import { spawnSync } from "node:child_process";
const [fixture, scratch, key] = process.argv.slice(2);
if (!fixture || !scratch || !key) throw new Error("Usage: bun packaging/update-smoke.ts FIXTURE_BINARY SCRATCH SIGNING_KEY_FILE");
if (process.platform !== "darwin" && process.platform !== "win32") throw new Error("macOS or Windows required");
const run = (program: string, args: string[], cwd?: string, env?: Record<string, string>) => {
  const result = spawnSync(program, args, { cwd, env: { ...process.env, ...env }, encoding: "utf8", timeout: 120_000 });
  if (result.error || result.status !== 0) throw new Error(`${basename(program)} failed: ${result.error ?? result.stderr}`);
  return result.stdout;
};
const root = resolve(scratch);
mkdirSync(root, { recursive: true });
const version = readFileSync("Cargo.toml", "utf8").match(/\[workspace.package\][\s\S]*?version = "([^"]+)"/)?.[1];
if (!version) throw new Error("Cannot read workspace version");
const next = version.replace(/\d+$/, n => String(Number(n) + 1));
const mac = process.platform === "darwin";
const current = join(root, "installed", mac ? "OMCA.app" : "OMCA");
const payload = join(root, "payload", mac ? "OMCA.app" : "OMCA");
for (const [dir, v] of [[current, version], [payload, next]]) {
  mkdirSync(mac ? join(dir, "Contents", "MacOS") : dir, { recursive: true });
  const markerDir = mac ? join(dir, "Contents", "Resources") : dir;
  mkdirSync(markerDir, { recursive: true });
  writeFileSync(join(markerDir, ".omca-update-fixture"), "synthetic fixture\n");
  copyFileSync(resolve(fixture), mac ? join(dir, "Contents", "MacOS", "craft-anything") : join(dir, "craft-anything.exe"));
  if (mac) {
    writeFileSync(join(dir, "Contents", "Info.plist"), `<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.seuput.omca</string><key>CFBundleExecutable</key><string>craft-anything</string><key>CFBundleName</key><string>OMCA</string><key>CFBundlePackageType</key><string>APPL</string><key>CraftAnythingVersion</key><string>${v}</string><key>CFBundleVersion</key><string>${v}</string></dict></plist>`);
    run("/usr/bin/codesign", ["--force", "--sign", "-", dir]);
  } else {
    writeFileSync(join(dir, "portable.txt"), "portable\n");
    writeFileSync(join(dir, "fixture-version.txt"), v);
  }
}
const credentials = mac ? join(root, "agent.db") : join(current, "OMCAData", "agent.db");
mkdirSync(resolve(credentials, ".."), { recursive: true });
writeFileSync(credentials, "synthetic-credential-retained\n", { mode: 0o600 });
const artifact = join(root, mac ? `omca-${next}-macos-aarch64-update.zip` : `craft-anything-${next}-windows-x64-portable.zip`);
if (mac) run("/usr/bin/zip", ["-q", "-r", artifact, "OMCA.app"], join(root, "payload"));
else run("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", "Compress-Archive -LiteralPath $env:OMCA_SMOKE_PAYLOAD -DestinationPath $env:OMCA_SMOKE_ARCHIVE"], undefined, { OMCA_SMOKE_PAYLOAD: payload, OMCA_SMOKE_ARCHIVE: artifact });
const newExe = mac ? join(payload, "Contents", "MacOS", "craft-anything") : join(payload, "craft-anything.exe");
run(process.execPath, ["packaging/sign-update.ts", artifact, newExe, next, mac ? "mac-zip" : "windows-portable"], undefined, { OMCA_UPDATE_SIGNING_KEY_FILE: resolve(key) });
const currentExe = mac ? join(current, "Contents", "MacOS", "craft-anything") : join(current, "craft-anything.exe");
run(currentExe, ["--prepare", `${artifact}.manifest.json`, `${artifact}.sig`, artifact, join(root, "cache")]);
const marker = join(root, "installed", "restarted-version.txt");
const deadline = Date.now() + 120_000;
while (!existsSync(marker) && Date.now() < deadline) await Bun.sleep(100);
if (!existsSync(marker) || readFileSync(marker, "utf8").trim() !== next) {
  const cache = join(root, "cache");
  if (existsSync(cache)) {
    for (const entry of readdirSync(cache, { recursive: true, withFileTypes: true })) {
      if (!entry.isFile() || !["helper.log", "portable-plan.json"].includes(entry.name)) continue;
      const path = join(entry.parentPath, entry.name);
      console.error(`${path}\n${readFileSync(path, "utf8").slice(-16_384)}`);
    }
  }
  console.error(`Installed version: ${run(currentExe, ["--version"]).trim()}`);
  throw new Error("New version did not restart from the original installation");
}
if (readFileSync(credentials, "utf8") !== "synthetic-credential-retained\n") throw new Error("Credentials changed during replacement");
if (run(currentExe, ["--version"]).trim() !== `craft-anything ${next}`) throw new Error("Original installation did not retain the new version");
console.log(`PASS: signed ${mac ? "macOS" : "Windows portable"} helper replaced ${version} with ${next}, restarted the same install, preserved synthetic credentials.`);
