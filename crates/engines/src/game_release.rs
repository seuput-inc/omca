//! Stable engine candidates from the publisher. Candidates are not deserializable: callers cannot
//! substitute a URL or hash. Godot trusts the official GitHub release feed over HTTPS, not an
//! independent signing key. A compromised publisher account remains outside this check.
use super::Builder;
use craft_anything_evidence::{ErrorCode, ToolError};
use craft_anything_platform::process::Cancel;
use craft_anything_platform::process::Process;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const GODOT_FEED: &str = "https://api.github.com/repos/godotengine/godot-builds/releases/latest";
const MAX_ARCHIVE: u64 = 512 << 20;
static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize)]
pub struct Release {
    pub builder: Builder,
    pub version: String,
    pub bytes: u64,
    url: String,
    sha256: String,
    program: String,
    strategy: Strategy,
}

#[derive(Debug, Clone, Copy, Serialize)]
enum Strategy {
    Godot,
    Rust,
    Unity,
}

#[derive(Deserialize)]
struct GodotFeed {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GodotAsset>,
}

#[derive(Deserialize)]
struct GodotAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
    size: u64,
}

impl Release {
    pub fn latest(builder: Builder, cancel: Option<&Cancel>) -> Result<Self, ToolError> {
        if builder == Builder::Unreal {
            return Err(ToolError::new(ErrorCode::NotSupported, "Unreal Engine is installed and updated in the official Epic Games Launcher."));
        }
        Self::resolve(builder, None, cancel)
    }

    pub fn resolve(builder: Builder, version: Option<&str>, cancel: Option<&Cancel>) -> Result<Self, ToolError> {
        match builder {
            Builder::Rust => return Self::rust(version, cancel),
            Builder::Unity => return Self::unity(version, cancel),
            Builder::Unreal => return Err(ToolError::new(ErrorCode::NotSupported, "Use the official Epic Games Launcher for Unreal Engine.")),
            Builder::Godot => {}
        }
        let url = match version {
            Some(v) if stable_version(v) => format!("https://api.github.com/repos/godotengine/godot-builds/releases/tags/{v}"),
            Some(_) => return Err(ToolError::invalid("Godot version must be a stable release such as 4.7.2-stable.")),
            None => GODOT_FEED.to_owned(),
        };
        let mut data = Vec::new();
        crate::install::download_to(&url, &mut data, 2 << 20, cancel, &|_, _| {})?;
        let release = Self::godot(&data, std::env::consts::OS, std::env::consts::ARCH)?;
        if version.is_some_and(|v| v != release.version) {
            return Err(ToolError::new(ErrorCode::GarbledOutput, "Godot returned a different release than requested."));
        }
        Ok(release)
    }

    fn godot(data: &[u8], os: &str, arch: &str) -> Result<Self, ToolError> {
        let feed: GodotFeed =
            serde_json::from_slice(data).map_err(|e| ToolError::new(ErrorCode::GarbledOutput, format!("Godot's release feed could not be read: {e}")))?;
        if feed.draft || feed.prerelease || !stable_version(&feed.tag_name) {
            return Err(ToolError::new(ErrorCode::GarbledOutput, "Godot's latest release is not a stable version."));
        }
        let suffix = match (os, arch) {
            ("macos", "aarch64" | "x86_64") => "macos.universal",
            ("windows", "x86_64") => "win64.exe",
            ("windows", "aarch64") => "windows_arm64.exe",
            ("linux", "x86_64") => "linux.x86_64",
            ("linux", "aarch64") => "linux.arm64",
            _ => return Err(ToolError::new(ErrorCode::NotSupported, "No supported Godot download for this operating system and architecture.")),
        };
        let stem = format!("Godot_v{}_{}", feed.tag_name, suffix);
        let filename = format!("{stem}.zip");
        let asset = feed
            .assets
            .into_iter()
            .find(|a| a.name == filename)
            .ok_or_else(|| ToolError::new(ErrorCode::GarbledOutput, "Godot's stable release has no download for this computer."))?;
        let expected_url = format!("https://github.com/godotengine/godot-builds/releases/download/{}/{filename}", feed.tag_name);
        if asset.browser_download_url != expected_url || asset.size == 0 || asset.size > MAX_ARCHIVE {
            return Err(ToolError::new(ErrorCode::GarbledOutput, "Godot's release has an unexpected download address or size."));
        }
        let sha256 = asset
            .digest
            .as_deref()
            .and_then(|d| d.strip_prefix("sha256:"))
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| ToolError::new(ErrorCode::FileHashMismatch, "Godot's release has no valid official SHA-256 digest."))?
            .to_owned();
        let program = if os == "macos" { "Godot.app/Contents/MacOS/Godot".to_owned() } else { stem };
        Ok(Self { builder: Builder::Godot, version: feed.tag_name, bytes: asset.size, url: expected_url, sha256, program, strategy: Strategy::Godot })
    }

    /// Install only a publisher-resolved candidate. A store-wide OS file lock rejects duplicate
    /// installs across threads and processes; a crash releases it. Each version is immutable.
    pub fn install(&self, store: &Path, cancel: Option<Cancel>, progress: &dyn Fn(u64, Option<u64>)) -> Result<PathBuf, ToolError> {
        match self.strategy {
            Strategy::Rust => return self.install_rust(store, cancel, progress),
            Strategy::Unity => return self.install_unity(store, cancel, progress),
            Strategy::Godot => {}
        }
        if !stable_version(&self.version) {
            return Err(ToolError::invalid("The Godot candidate has an invalid version."));
        }
        let dir = store.join("godot");
        std::fs::create_dir_all(&dir).map_err(|e| ToolError::io(dir.display(), e))?;
        let lock_path = dir.join(".install.lock");
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| ToolError::io(lock_path.display(), e))?;
        lock.try_lock().map_err(|_| {
            ToolError::new(ErrorCode::ProgramFailed, "Another Godot installation is already running.")
                .with_remediation("Wait for that installation to finish, then try again.")
        })?;
        check_cancel(cancel.as_ref())?;
        let dest = dir.join(&self.version);
        if dest.join(&self.program).is_file() {
            return Ok(dest.join(&self.program));
        }
        let incoming = Incoming::new(&dir)?;
        let archive = incoming.0.join("archive.zip");
        crate::install::download_verified(&self.url, &archive, &self.sha256, MAX_ARCHIVE, cancel.as_ref(), progress)?;
        let data = std::fs::read(&archive).map_err(|e| ToolError::io(archive.display(), e))?;
        std::fs::remove_file(&archive).map_err(|e| ToolError::io(archive.display(), e))?;
        self.publish(&data, &incoming, &dest, cancel.as_ref())
    }

    fn publish(&self, data: &[u8], incoming: &Incoming, dest: &Path, cancel: Option<&Cancel>) -> Result<PathBuf, ToolError> {
        crate::install::unpack_zip_into(data, &incoming.0, 1 << 30, cancel)?;
        if !incoming.0.join(&self.program).is_file() {
            return Err(ToolError::new(ErrorCode::GarbledOutput, "The verified Godot archive has no editor executable."));
        }
        check_cancel(cancel)?;
        // Never remove an existing version. Rename only publishes a complete new directory.
        std::fs::rename(&incoming.0, dest).map_err(|e| ToolError::io(dest.display(), e))?;
        Ok(dest.join(&self.program))
    }
}

const UNITY_CDN: &str = "https://public-cdn.cloud.unity3d.com/hub/prod/cli/";

pub(super) fn unity_cli(store: &Path, cancel: Option<&Cancel>, progress: &dyn Fn(u64, Option<u64>)) -> Result<PathBuf, ToolError> {
    use sha2::{Digest, Sha256};
    let release = Release::latest(Builder::Unity, cancel)?;
    let version = release
        .url
        .strip_prefix(UNITY_CDN)
        .and_then(|v| v.split('/').next())
        .filter(|v| numeric_version(v))
        .ok_or_else(|| ToolError::new(ErrorCode::GarbledOutput, "Unity's CLI version is invalid."))?;
    let dir = store.join("unity-cli").join(version);
    let _lock = install_lock(&dir)?;
    let cli = dir.join(format!("unity{}", std::env::consts::EXE_SUFFIX));
    if cli.is_file() {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(&cli).and_then(|file| file.take((128 << 20) + 1).read_to_end(&mut bytes)).map_err(|e| ToolError::io(cli.display(), e))?;
        if bytes.len() <= 128 << 20 && hex::encode(Sha256::digest(&bytes)) == release.sha256 {
            return Ok(cli);
        }
        return Err(ToolError::new(ErrorCode::FileHashMismatch, "The saved Unity CLI no longer matches the official checksum."));
    }
    crate::install::download_verified(&release.url, &cli, &release.sha256, 128 << 20, cancel, progress)?;
    executable(&cli)?;
    Ok(cli)
}

fn fetch(url: &str, max: u64, cancel: Option<&Cancel>) -> Result<Vec<u8>, ToolError> {
    let mut bytes = Vec::new();
    crate::install::download_to(url, &mut bytes, max, cancel, &|_, _| {})?;
    Ok(bytes)
}

fn sha256(text: &str) -> Result<String, ToolError> {
    text.split_whitespace()
        .next()
        .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
        .ok_or_else(|| ToolError::new(ErrorCode::FileHashMismatch, "The publisher did not provide a valid SHA-256 checksum."))
}

fn numeric_version(version: &str) -> bool {
    version.split('.').count() == 3 && version.split('.').all(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
}

fn unity_version(version: &str) -> bool {
    let Some((numbers, patch)) = version.split_once('f') else { return false };
    numeric_version(numbers) && !patch.is_empty() && patch.len() <= 5 && patch.bytes().all(|b| b.is_ascii_digit())
}

fn executable(path: &Path) -> Result<(), ToolError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|e| ToolError::io(path.display(), e))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn windows_sdk(roots: &[PathBuf], arch: &str) -> bool {
    let arch = if arch == "aarch64" { "arm64" } else { "x64" };
    roots.iter().any(|root| {
        std::fs::read_dir(root.join("Include")).into_iter().flatten().flatten().any(|version| {
            let version = version.file_name();
            root.join("Include").join(&version).join("um/Windows.h").is_file()
                && root.join("Lib").join(&version).join("um").join(arch).join("kernel32.lib").is_file()
        })
    })
}

fn install_lock(dir: &Path) -> Result<std::fs::File, ToolError> {
    std::fs::create_dir_all(dir).map_err(|e| ToolError::io(dir.display(), e))?;
    let path = dir.join(".install.lock");
    let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).map_err(|e| ToolError::io(path.display(), e))?;
    lock.try_lock().map_err(|_| ToolError::new(ErrorCode::ProgramFailed, "Another installation of this engine is already running."))?;
    Ok(lock)
}

fn run_installer(process: Process, cancel: Option<&Cancel>) -> Result<(), ToolError> {
    let process = process.timeout(Duration::from_secs(2 * 60 * 60)).idle_timeout(Duration::from_secs(20 * 60)).stdout_cap(2 << 20).stderr_cap(2 << 20);
    let process = if let Some(cancel) = cancel { process.cancel(cancel.clone()) } else { process };
    let out = process.run()?;
    if !out.success {
        return Err(ToolError::new(ErrorCode::ProgramFailed, format!("The official engine installer failed: {}", out.end_text()))
            .with_remediation("Complete any required sign-in, licence or compiler setup in the vendor's app, then try again."));
    }
    Ok(())
}

impl Release {
    fn rust(version: Option<&str>, cancel: Option<&Cancel>) -> Result<Self, ToolError> {
        if version.is_some_and(|v| !numeric_version(v)) {
            return Err(ToolError::invalid("Rust version must contain major.minor.patch."));
        }
        let host = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => "aarch64-apple-darwin",
            ("macos", "x86_64") => "x86_64-apple-darwin",
            ("windows", "aarch64") => "aarch64-pc-windows-msvc",
            ("windows", "x86_64") => "x86_64-pc-windows-msvc",
            ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
            ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
            _ => return Err(ToolError::new(ErrorCode::NotSupported, "No supported Rust host for this computer.")),
        };
        let channel = version.unwrap_or("stable");
        let data = fetch(&format!("https://static.rust-lang.org/dist/channel-rust-{channel}.toml"), 2 << 20, cancel)?;
        let text = std::str::from_utf8(&data).map_err(|_| ToolError::new(ErrorCode::GarbledOutput, "Rust's channel manifest is not UTF-8."))?;
        let manifest: toml::Value =
            toml::from_str(text).map_err(|e| ToolError::new(ErrorCode::GarbledOutput, format!("Rust's channel manifest could not be read: {e}")))?;
        let actual = manifest
            .get("pkg")
            .and_then(|v| v.get("rust"))
            .and_then(|v| v.get("version"))
            .and_then(|v| v.as_str())
            .and_then(|v| v.split_whitespace().next())
            .filter(|v| numeric_version(v))
            .ok_or_else(|| ToolError::new(ErrorCode::GarbledOutput, "Rust's channel manifest has no stable version."))?;
        if version.is_some_and(|v| v != actual) {
            return Err(ToolError::new(ErrorCode::GarbledOutput, "Rust returned a different version than requested."));
        }
        let file = if cfg!(windows) { "rustup-init.exe" } else { "rustup-init" };
        let url = format!("https://static.rust-lang.org/rustup/dist/{host}/{file}");
        let checksum = fetch(&format!("{url}.sha256"), 4096, cancel)?;
        let hash = sha256(&String::from_utf8_lossy(&checksum))?;
        Ok(Self {
            builder: Builder::Rust,
            version: actual.to_owned(),
            bytes: 0,
            url,
            sha256: hash,
            program: format!("cargo/bin/cargo{}", std::env::consts::EXE_SUFFIX),
            strategy: Strategy::Rust,
        })
    }

    fn install_rust(&self, store: &Path, cancel: Option<Cancel>, progress: &dyn Fn(u64, Option<u64>)) -> Result<PathBuf, ToolError> {
        if !numeric_version(&self.version) {
            return Err(ToolError::invalid("Invalid Rust candidate version."));
        }
        // Do not let rustup's prerequisite helper install Visual Studio and accept its terms.
        if cfg!(windows) {
            let places = super::Places::here(store.to_owned());
            let roots: Vec<_> = std::env::var_os("WindowsSdkDir")
                .map(PathBuf::from)
                .into_iter()
                .chain(places.program_files.iter().map(|p| p.join("Windows Kits/10")))
                .collect();
            if !super::cpp_tools(&places, None) || !windows_sdk(&roots, std::env::consts::ARCH) {
                return Err(ToolError::new(ErrorCode::EngineMissing, "Rust needs the Visual C++ tools and Windows SDK first.")
                    .with_remediation("Install the C++ workload in your licensed Visual Studio edition, then retry Rust installation."));
            }
        }
        let dir = store.join("rust");
        let _lock = install_lock(&dir)?;
        check_cancel(cancel.as_ref())?;
        let dest = dir.join(&self.version);
        if dest.join(&self.program).is_file() {
            return Ok(dest.join(&self.program));
        }
        let incoming = Incoming::new(&dir)?;
        let init = incoming.0.join(if cfg!(windows) { "rustup-init.exe" } else { "rustup-init" });
        crate::install::download_verified(&self.url, &init, &self.sha256, 64 << 20, cancel.as_ref(), progress)?;
        executable(&init)?;
        let host = self.url.split("/dist/").nth(1).and_then(|s| s.split('/').next()).ok_or_else(|| ToolError::invalid("Invalid Rust host URL."))?;
        let process = Process::new(&init)
            .args(["-y", "--no-modify-path", "--profile", "minimal", "--default-toolchain", &self.version, "--default-host", host])
            .env("CARGO_HOME", incoming.0.join("cargo"))
            .env("RUSTUP_HOME", incoming.0.join("rustup"))
            .env("RUSTUP_DIST_SERVER", "https://static.rust-lang.org")
            .env("RUSTUP_UPDATE_ROOT", "https://static.rust-lang.org/rustup");
        progress(0, None);
        run_installer(process, cancel.as_ref())?;
        if !incoming.0.join(&self.program).is_file() || !incoming.0.join(format!("cargo/bin/rustc{}", std::env::consts::EXE_SUFFIX)).is_file() {
            return Err(ToolError::new(ErrorCode::GarbledOutput, "Rust's installer did not produce cargo and rustc."));
        }
        std::fs::remove_file(&init).map_err(|e| ToolError::io(init.display(), e))?;
        check_cancel(cancel.as_ref())?;
        std::fs::rename(&incoming.0, &dest).map_err(|e| ToolError::io(dest.display(), e))?;
        Ok(dest.join(&self.program))
    }

    fn unity(version: Option<&str>, cancel: Option<&Cancel>) -> Result<Self, ToolError> {
        if version.is_some_and(|v| !unity_version(v)) {
            return Err(ToolError::invalid("Unity version must name a stable Editor release such as 6000.6.9f1."));
        }
        let (platform, os) = match std::env::consts::OS {
            "macos" => ("MAC_OS", "darwin"),
            "windows" => ("WINDOWS", "win32"),
            "linux" => ("LINUX", "linux"),
            _ => return Err(ToolError::new(ErrorCode::NotSupported, "No supported Unity CLI for this computer.")),
        };
        let (architecture, arch) = match std::env::consts::ARCH {
            "aarch64" => ("ARM64", "arm64"),
            "x86_64" => ("X86_64", "x64"),
            _ => return Err(ToolError::new(ErrorCode::NotSupported, "No supported Unity CLI architecture.")),
        };
        let query = version.map(|v| format!("version={v}")).unwrap_or_default();
        let url = format!("https://services.api.unity.com/unity/editor/release/v1/releases?limit=25&platform={platform}&architecture={architecture}&{query}");
        let data = fetch(&url, 4 << 20, cancel)?;
        let feed: serde_json::Value = serde_json::from_slice(&data).map_err(|e| ToolError::new(ErrorCode::GarbledOutput, e.to_string()))?;
        let editor = feed
            .get("results")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .find(|r| r.get("version").and_then(|v| v.as_str()).is_some_and(|v| unity_version(v) && version.is_none_or(|want| want == v)))
            .ok_or_else(|| ToolError::new(ErrorCode::GarbledOutput, "Unity's feed has no matching stable Editor release."))?;
        let actual = editor.get("version").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
        let bytes = editor
            .get("downloads")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .find(|d| d.get("platform").and_then(|v| v.as_str()) == Some(platform) && d.get("architecture").and_then(|v| v.as_str()) == Some(architecture))
            .and_then(|d| d.get("downloadSize"))
            .and_then(|d| d.get("value"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let manifest: serde_json::Value = serde_json::from_slice(&fetch(&format!("{UNITY_CDN}latest.json"), 128 << 10, cancel)?)
            .map_err(|e| ToolError::new(ErrorCode::GarbledOutput, e.to_string()))?;
        let cli_version = manifest
            .get("version")
            .and_then(|v| v.as_str())
            .filter(|v| numeric_version(v))
            .ok_or_else(|| ToolError::new(ErrorCode::GarbledOutput, "Unity's CLI manifest has no stable version."))?;
        let binary = manifest
            .get("binaries")
            .and_then(|v| v.get(format!("{os}-{arch}")))
            .ok_or_else(|| ToolError::new(ErrorCode::NotSupported, "Unity's CLI manifest has no binary for this computer."))?;
        let name = binary
            .get("filename")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)))
            .ok_or_else(|| ToolError::new(ErrorCode::GarbledOutput, "Unity's CLI manifest has an invalid filename."))?;
        let hash = sha256(binary.get("sha256").and_then(|v| v.as_str()).unwrap_or_default())?;
        let program = if cfg!(target_os = "macos") {
            "Unity.app/Contents/MacOS/Unity"
        } else if cfg!(windows) {
            "Editor/Unity.exe"
        } else {
            "Editor/Unity"
        };
        Ok(Self {
            builder: Builder::Unity,
            version: actual,
            bytes,
            url: format!("{UNITY_CDN}{cli_version}/{name}"),
            sha256: hash,
            program: program.into(),
            strategy: Strategy::Unity,
        })
    }

    fn install_unity(&self, store: &Path, cancel: Option<Cancel>, progress: &dyn Fn(u64, Option<u64>)) -> Result<PathBuf, ToolError> {
        if !unity_version(&self.version) {
            return Err(ToolError::invalid("Invalid Unity candidate version."));
        }
        let dir = store.join("unity");
        let _lock = install_lock(&dir)?;
        check_cancel(cancel.as_ref())?;
        let dest = dir.join(&self.version);
        if dest.join(&self.program).is_file() {
            return Ok(dest.join(&self.program));
        }
        let incoming = Incoming::new(&dir)?;
        let cli = incoming.0.join(format!("unity{}", std::env::consts::EXE_SUFFIX));
        crate::install::download_verified(&self.url, &cli, &self.sha256, 128 << 20, cancel.as_ref(), progress)?;
        executable(&cli)?;
        let mut process = Process::new(&cli)
            .args([
                "install",
                &self.version,
                "--non-interactive",
                "--format",
                "json",
                "--no-banner",
                "--no-pager",
                "--no-wait",
                "--no-log-proxy",
                "--download-directory",
            ])
            .arg(&incoming.0);
        let home = store.join("unity-account");
        std::fs::create_dir_all(&home).map_err(|e| ToolError::io(home.display(), e))?;
        process = super::unity::private_process(process, &home);
        if cfg!(target_os = "macos") {
            process = process.args(["--architecture", if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" }]);
        }
        // Never pass --accept-eula, --set-default or an authentication credential.
        progress(0, None);
        run_installer(process, cancel.as_ref())?;
        let editor = incoming.0.join(&self.version);
        if !editor.join(&self.program).is_file() {
            return Err(ToolError::new(ErrorCode::GarbledOutput, "Unity's installer did not produce the selected Editor."));
        }
        check_cancel(cancel.as_ref())?;
        std::fs::rename(&editor, &dest).map_err(|e| ToolError::io(dest.display(), e))?;
        Ok(dest.join(&self.program))
    }
}

/// Configure only this child. A managed cargo proxy needs its version's private homes, while
/// the original PATH stays available after its bin folder. Never change the parent environment.
pub fn rust_process(program: &Path, process: Process) -> Process {
    let Some(bin) = program.parent().filter(|p| p.file_name().is_some_and(|n| n == "bin")) else { return process };
    let Some(cargo) = bin.parent().filter(|p| p.file_name().is_some_and(|n| n == "cargo")) else { return process };
    let Some(root) = cargo.parent().filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(numeric_version)) else { return process };
    if !root.parent().is_some_and(|p| p.file_name().is_some_and(|n| n == "rust")) {
        return process;
    }
    let path = std::env::join_paths(std::iter::once(bin.to_path_buf()).chain(std::env::var_os("PATH").iter().flat_map(std::env::split_paths)));
    let process = process.env("CARGO_HOME", cargo).env("RUSTUP_HOME", root.join("rustup"));
    match path {
        Ok(path) => process.env("PATH", path),
        Err(_) => process,
    }
}

fn check_cancel(cancel: Option<&Cancel>) -> Result<(), ToolError> {
    if cancel.is_some_and(|c| c()) { Err(ToolError::cancelled()) } else { Ok(()) }
}

fn stable_version(version: &str) -> bool {
    let Some(numbers) = version.strip_suffix("-stable") else { return false };
    let mut count = 0usize;
    let valid = numbers.split('.').all(|p| {
        count += 1;
        !p.is_empty() && p.len() <= 5 && p.bytes().all(|c| c.is_ascii_digit())
    });
    valid && (2..=3).contains(&count)
}

struct Incoming(PathBuf);

impl Incoming {
    fn new(dir: &Path) -> Result<Self, ToolError> {
        let path = dir.join(format!(".incoming-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&path).map_err(|e| ToolError::io(path.display(), e))?;
        Ok(Self(path))
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn installed_godot(path: &Path, name: &str) -> Option<(String, PathBuf)> {
    if stable_version(name) && path.is_dir() {
        let program = [
            "Godot.app/Contents/MacOS/Godot".to_owned(),
            format!("Godot_v{name}_win64.exe"),
            format!("Godot_v{name}_windows_arm64.exe"),
            format!("Godot_v{name}_linux.x86_64"),
            format!("Godot_v{name}_linux.arm64"),
        ]
        .into_iter()
        .map(|rel| path.join(rel))
        .find(|p| p.is_file())?;
        return Some((name.trim_end_matches("-stable").to_owned(), program));
    }
    // Previously installed flat layouts stay usable by their existing project paths.
    if !name.starts_with("Godot_v") || name.contains("console") {
        return None;
    }
    let program = if name.ends_with(".app") {
        path.join("Contents/MacOS/Godot")
    } else if name.ends_with(".exe") || name.ends_with(".x86_64") {
        path.to_path_buf()
    } else {
        return None;
    };
    program.is_file().then(|| (name.trim_start_matches("Godot_v").split('-').next().unwrap_or_default().to_owned(), program))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(version: &str, os_asset: &str) -> Vec<u8> {
        let name = format!("Godot_v{version}_{os_asset}.zip");
        serde_json::to_vec(&serde_json::json!({"tag_name":version,"draft":false,"prerelease":false,"assets":[{
            "name":name,"browser_download_url":format!("https://github.com/godotengine/godot-builds/releases/download/{version}/{name}"),
            "digest":format!("sha256:{}", "a".repeat(64)),"size":100
        }]}))
        .unwrap()
    }

    #[test]
    fn selects_native_stable_candidate_and_rejects_untrusted_metadata() {
        let data = feed("4.7.2-stable", "macos.universal");
        let r = Release::godot(&data, "macos", "aarch64").unwrap();
        assert_eq!(r.version, "4.7.2-stable");
        assert_eq!(r.program, "Godot.app/Contents/MacOS/Godot");
        assert!(Release::godot(&data, "windows", "x86_64").is_err());
        assert!(Release::godot(&feed("4.8-beta3", "macos.universal"), "macos", "aarch64").is_err());
        let mut v: serde_json::Value = serde_json::from_slice(&data).unwrap();
        v["assets"][0]["browser_download_url"] = "https://example.invalid/editor.zip".into();
        assert!(Release::godot(&serde_json::to_vec(&v).unwrap(), "macos", "aarch64").is_err());
        v["assets"][0]["browser_download_url"] = r.url.into();
        v["assets"][0]["digest"] = serde_json::Value::Null;
        assert!(Release::godot(&serde_json::to_vec(&v).unwrap(), "macos", "aarch64").is_err());
    }

    #[test]
    fn cancelled_or_broken_archive_preserves_all_existing_versions() {
        let d = tempfile::tempdir().unwrap();
        let store = d.path();
        let old = store.join("godot/4.6-stable/Godot.app/Contents/MacOS");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("Godot"), b"working editor").unwrap();
        let r = Release::godot(&feed("4.7.2-stable", "macos.universal"), "macos", "aarch64").unwrap();
        let cancel: Cancel = std::sync::Arc::new(|| true);
        assert!(r.install(store, Some(cancel), &|_, _| {}).is_err());
        let dir = store.join("godot");
        let incoming = Incoming::new(&dir).unwrap();
        assert!(r.publish(b"broken zip", &incoming, &dir.join(&r.version), None).is_err());
        drop(incoming);
        assert_eq!(std::fs::read(old.join("Godot")).unwrap(), b"working editor");
        assert!(!dir.join(&r.version).exists());
        assert!(!std::fs::read_dir(&dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".incoming")));
        let inv = super::super::inventory(&super::super::Places { store: store.to_path_buf(), ..Default::default() });
        assert_eq!(inv.best(Builder::Godot).unwrap().version, "4.6");
    }

    #[test]
    fn another_install_holds_the_store_lock_without_network_access() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("godot");
        std::fs::create_dir(&dir).unwrap();
        let lock = std::fs::File::create(dir.join(".install.lock")).unwrap();
        lock.try_lock().unwrap();
        let r = Release::godot(&feed("4.7.2-stable", "win64.exe"), "windows", "x86_64").unwrap();
        assert!(r.install(d.path(), None, &|_, _| {}).unwrap_err().message.contains("already running"));
    }

    #[test]
    fn publishes_complete_versions_without_replacing_previous_editors() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("godot");
        std::fs::create_dir(&dir).unwrap();
        let old = dir.join("4.6-stable/Godot.app/Contents/MacOS");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("Godot"), b"previous editor").unwrap();
        let r = Release::godot(&feed("4.7.2-stable", "macos.universal"), "macos", "aarch64").unwrap();
        let zip = craft_anything_testkit::electron::zip_stored(&[("Godot.app/Contents/MacOS/Godot", b"new editor")]);
        let incoming = Incoming::new(&dir).unwrap();
        let dest = dir.join(&r.version);
        let program = r.publish(&zip, &incoming, &dest, None).unwrap();
        assert_eq!(std::fs::read(program).unwrap(), b"new editor");
        assert_eq!(std::fs::read(old.join("Godot")).unwrap(), b"previous editor");
        let second = Incoming::new(&dir).unwrap();
        assert!(r.publish(&zip, &second, &dest, None).is_err(), "a published version is immutable");
        let inv = super::super::inventory(&super::super::Places { store: d.path().to_owned(), ..Default::default() });
        assert_eq!(inv.engines.iter().filter(|i| i.builder == Builder::Godot).map(|i| i.version.as_str()).collect::<Vec<_>>(), ["4.7.2", "4.6"]);
    }

    #[test]
    fn interrupted_download_and_extraction_never_publish() {
        let d = tempfile::tempdir().unwrap();
        let mut r = Release::godot(&feed("4.7.2-stable", "macos.universal"), "macos", "aarch64").unwrap();
        r.url = "http://127.0.0.1:0/refused".into();
        assert!(r.install(d.path(), None, &|_, _| {}).is_err());
        let dir = d.path().join("godot");
        let incoming = Incoming::new(&dir).unwrap();
        let exe = incoming.0.join(&r.program);
        let cancel: Cancel = std::sync::Arc::new(move || exe.is_file());
        let zip = craft_anything_testkit::electron::zip_stored(&[("Godot.app/Contents/MacOS/Godot", b"editor"), ("Godot.app/data", b"unfinished")]);
        assert!(r.publish(&zip, &incoming, &dir.join(&r.version), Some(&cancel)).is_err());
        assert!(!dir.join(&r.version).exists());
        drop(incoming);
        assert!(!std::fs::read_dir(dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".incoming")));
    }

    #[test]
    fn private_rust_inventory_and_environment_ignore_incomplete_versions() {
        let d = tempfile::tempdir().unwrap();
        for version in ["1.99.0", "1.100.0", ".incoming-1"] {
            let root = d.path().join("rust").join(version);
            std::fs::create_dir_all(root.join("cargo/bin")).unwrap();
            std::fs::create_dir(root.join("rustup")).unwrap();
            std::fs::write(root.join("rustup/settings.toml"), b"version = \"12\"").unwrap();
            for name in ["cargo", "rustc"] {
                std::fs::write(root.join(format!("cargo/bin/{name}{}", std::env::consts::EXE_SUFFIX)), b"test").unwrap();
            }
        }
        std::fs::create_dir_all(d.path().join("rust/2.0.0/cargo/bin")).unwrap();
        let installed = super::super::managed_rust(d.path());
        assert_eq!(installed.iter().map(|i| i.version.as_str()).collect::<Vec<_>>(), ["1.100.0", "1.99.0"]);
        let env = super::super::rust_environment(d.path()).unwrap();
        assert!(env.iter().any(|(k, v)| k == "CARGO_HOME" && Path::new(v) == d.path().join("rust/1.100.0/cargo")));
        let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v).unwrap();
        assert_eq!(std::env::split_paths(path).next().unwrap(), d.path().join("rust/1.100.0/cargo/bin"));
    }

    #[cfg(unix)]
    #[test]
    fn cargo_process_uses_private_homes_without_changing_parent_environment() {
        let d = tempfile::tempdir().unwrap();
        let cargo = d.path().join("rust/1.100.0/cargo/bin/cargo");
        let before = std::env::var_os("CARGO_HOME");
        let out = rust_process(&cargo, Process::new("/bin/sh").args(["-c", "printf '%s\\n%s' \"$CARGO_HOME\" \"$RUSTUP_HOME\""])).run().unwrap();
        assert!(out.success);
        assert_eq!(out.stdout_text(), format!("{}\n{}", d.path().join("rust/1.100.0/cargo").display(), d.path().join("rust/1.100.0/rustup").display()));
        assert_eq!(std::env::var_os("CARGO_HOME"), before);
    }

    #[test]
    fn sdk_headers_without_architecture_libraries_do_not_allow_prerequisite_installation() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("sdk");
        std::fs::create_dir_all(root.join("Include/10.0/um")).unwrap();
        std::fs::write(root.join("Include/10.0/um/Windows.h"), b"header").unwrap();
        assert!(!windows_sdk(std::slice::from_ref(&root), "x86_64"));
        std::fs::create_dir_all(root.join("Lib/10.0/um/x64")).unwrap();
        std::fs::write(root.join("Lib/10.0/um/x64/kernel32.lib"), b"library").unwrap();
        assert!(windows_sdk(std::slice::from_ref(&root), "x86_64"));
        assert!(!windows_sdk(std::slice::from_ref(&root), "aarch64"));
    }
}
