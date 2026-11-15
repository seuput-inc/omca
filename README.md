<p align="center">
  <img src="assets/omca-hero.webp" width="960" alt="OMCA — inspect software and build your own">
</p>

<p align="center">
  See how any website, app or game works, then have a coding agent build your own version.<br>
  A modified version of <a href="https://github.com/storytold/craft-anything">Craft Anything</a>.
</p>

<p align="center">
  <a href="https://github.com/seuput-inc/omca/releases/download/v0.4.7/craft-anything-0.4.7-macos-aarch64.dmg"><img src="https://img.shields.io/badge/Install-macOS%20Apple%20silicon-111827?style=for-the-badge&amp;logo=apple&amp;logoColor=white" alt="Install for macOS Apple silicon"></a>
  <a href="https://github.com/seuput-inc/omca/releases/download/v0.4.7/craft-anything-0.4.7-windows-x64.msi"><img src="https://img.shields.io/badge/Install-Windows%20x64-0078D4?style=for-the-badge" alt="Install for Windows x64"></a>
</p>
<p align="center">
  Published release: <a href="https://github.com/seuput-inc/omca/releases/tag/v0.4.7">v0.4.7</a>
  · <a href="https://github.com/seuput-inc/omca/releases/download/v0.4.7/craft-anything-0.4.7-windows-x64-portable.zip">Windows portable ZIP</a>
</p>

## What it is

OMCA studies a target (a website, desktop or phone app, game or file) with real inspection
tools, has a coding agent build a new project, compares the build with the original and refines
it: **Study → Build → Check → Ready**. Every tool is also a CLI command and an MCP tool.

The engine, workflow, Clean room / Deep modes and tool set come from upstream Craft Anything; see
[`docs/`](docs/) for how they work (those pages still use the upstream name).

## Differences from upstream

- Name, bundle identifier (`com.seuput.omca`) and app icon.
- Its own storage, so it runs beside an installed Craft Anything without sharing anything:

  | What | OMCA |
  |---|---|
  | Settings, logs, engine store (macOS) | `~/Library/Application Support/omca` |
  | Portable data folder | `OMCAData` next to the executable |
  | Keychain service | `com.seuput.omca` |
  | Projects | `~/Documents/OMCA` |
  | MCP server / skill name | `omca` (tools are `mcp__omca__*`) |

- Releases for Apple silicon Macs and Windows x64 only; the interface in Korean and English only.
- Its own version numbers, starting at 0.1.0, and its own `vX.Y.Z` tags; upstream's tags are not
  kept.
- Local CI, with additional Windows-native checks in the manual release workflow; no push or pull-request CI.
- Internal crate and binary names (`craft_anything_*`, `craft-anything`, `craft-anything-cli`)
  and `CRAFT_ANYTHING_*` environment variables are unchanged, to keep upstream merges simple.

Settings → Models & accounts groups connected providers and their accounts, with sign-in,
account selection, model selection, and provider-reported quota windows. The desktop app uses
only accounts signed into OMCA or API keys entered in OMCA. It does not discover or adopt
installed CLI accounts, environment keys, or other apps' credentials. Legacy configuration stays
on disk but cannot select an external agent for a desktop conversation or build.

Every chat and build input shows its selected model, without account or provider labels.
Open **Change model** to search available chat models from connected providers.
Switching models keeps the draft and conversation visible. Active or queued work must finish before switching.
Unavailable saved project builders require an explicit selection instead of a silent fallback.

One workspace replaces Simple / Advanced: projects stay visible beside conversations and real
result captures, with inspection tools, code, evidence, jobs and logs in contextual panels.
Models & accounts separates model readiness from connected-account status. Failures show
recovery guidance first, with original diagnostics in expandable, copyable details.

Pretendard supplies the interface and Korean text. JetBrains Mono supplies code and logs.
The unified workspace uses 15 px body text, 13 px captions, and 14 px code text.
Optional instructions open from help icons; costs, permissions, errors, and folder paths remain
visible. Compact form spacing and a narrower results panel leave more room for the work.

Choose a common projects folder once, then name each new project. OMCA creates a separate child
folder without overwriting an existing project. Existing projects can still be opened directly.
Different projects use isolated execution sessions and run up to the configured parallel-build
limit. The same project cannot start twice.

The system language is the default for the interface and user-facing agent responses.
An explicit language setting takes precedence. Korean and English are supported; code,
identifiers, and original technical diagnostics keep their original language.

The bundled provider runtime uses the public omp SDK with OMCA's private credential store.
Provider-specific warnings and disabled-provider rules still apply. API keys use a separate
connection form. The standalone CLI retains its separately configured agent integrations.

OMCA runs its own Rust UI and agent engine; it uses omp's public SDK for provider authentication,
model inference and native image generation, rather than wrapping the whole `omp` process.
Eligible connected GPT accounts are preferred for images; fal.ai is optional for pictures and
provides 3D generation. Unknown monetary charges or subscription-credit usage require explicit
approval before generation and are not presented as free. Native failure does not trigger a
hidden paid-provider fallback.

Game-engine setup stays in the app. Godot and Rust use verified, versioned managed installs.
Unity uses its official CLI, with separate sign-in and license actions. Personal-license
activation requires your explicit eligibility and terms confirmation; agents cannot grant it.
Existing projects keep their engine version instead of being migrated automatically.
Unreal uses the official Epic Games Launcher for installation and updates. After the launcher
finishes, confirm completion in OMCA; finding an editor file alone does not auto-start a build.

## Platforms and languages

Releases are built for **Apple silicon Macs with macOS 13 or newer** (`aarch64`) and **Windows x64** only. The
interface is in **Korean** and **English**; release builds embed craft-fonts' Korean and symbol
fonts (pinned in `packaging/env.sh`).

## Build

Requires Rust 1.97 or newer.
Packaged account connections also need Bun 1.4.4 or newer at build time. Release packages include
the standalone helper, so users do not need Bun or an installed coding-agent runtime.

```sh
cargo run --release -p craft-anything                # the desktop app
cargo install --path apps/craft-anything-cli         # the CLI, as `craft-anything-cli`
craft-anything-cli doctor
```

To build the account helper for a source checkout (macOS):

```sh
export CARGO_TARGET_DIR="$PWD/target"
. packaging/env.sh
build_provider_runtime bun-darwin-arm64
export OMCA_PROVIDER_RUNTIME="$PROVIDER_RUNTIME_DIR/omca-provider-runtime"
cargo run -p craft-anything
```


## CI (local)

There is no GitHub Actions CI: the gate runs on your machine.

```sh
git config core.hooksPath .githooks   # once per clone: secrets check on commit, full gate on push
cargo xtask ci                        # the full gate by hand: fmt, clippy, tests, tool/schema checks
```

## Release

```sh
cargo xtask version set X.Y.Z && git commit -am "Release X.Y.Z"
packaging/release.sh
```

`packaging/release.sh` runs the gate and builds the macOS DMG, CLI ZIP, and signed update ZIP.
OS signing is ad-hoc unless `MACOS_SIGN_IDENTITY` is set. Notarization uses `APPLE_ID`,
`APPLE_PASSWORD`, and `APPLE_TEAM_ID`. Update manifests require `OMCA_UPDATE_SIGNING_KEY_FILE`
or `OMCA_UPDATE_SIGNING_KEY`, matching the pinned public key.
The script publishes `vX.Y.Z` and starts the manual `release-windows.yml`, which attaches the
MSI, portable ZIP, and their signed manifests. `sign-updates.yml` can sign an existing release's
macOS update ZIP with the encrypted repository secret. Neither workflow runs on push.

## Upstream

Integrated through upstream `8374ad7` (2026-11-14, Craft Anything 0.6.1), retaining
OMCA's identity, storage, Korean/English interface and Mac/Windows release targets.
OMCA 0.4.1 follows the locally installed 0.4.0 build; upstream version tags remain separate.

The upstream changes include parser and archive safety fixes, builder/session recovery,
settings-file preservation, website traffic recording and application update checks.

Settings → Updates and Help → Check for Updates use the same `seuput-inc/omca` release check.
**Update and restart** downloads an OMCA-signed package, verifies its pinned Ed25519 signature,
version, platform, and hash, then replaces the application and restarts the same installation.
Account credentials, settings, and projects remain outside the replacement payload.
Windows portable updates leave `OMCAData` in place. Windows installer updates can request normal
UAC approval; OMCA does not save administrator passwords or change system security settings.
Versions through 0.4.4 need one manual installation to gain this updater.


```sh
git config remote.upstream.tagOpt --no-tags   # once per clone: keep upstream's version tags out
git fetch upstream
git merge upstream/main
```

## License

MIT OR Apache-2.0, as upstream. See [NOTICE](NOTICE), [LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE) and [ATTRIBUTION.md](ATTRIBUTION.md). OMCA is not made,
sponsored or endorsed by the ArtCraft Team.
