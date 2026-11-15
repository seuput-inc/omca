//! OMCA's signed updates. Preparation never changes the installation or credential stores.
//! A copy of the running executable verifies the signed artifact again after its owner exits.
use crate::{
    fs::read_file_capped,
    process::{Launch, Process},
};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};
#[path = "update_portable.rs"]
mod portable;

pub const PUBLIC_KEY_HEX: &str = "be4b2d855b01518e6f7722dc477888b68e274980ec3ca8d0da5a50a9ab179e0a";
pub const HELPER_FLAG: &str = "--omca-finish-update";
const MAX_ARCHIVE: u64 = 2 * 1024 * 1024 * 1024;
const MAX_EXPANDED: u64 = 6 * 1024 * 1024 * 1024;
#[cfg(target_os = "macos")]
const BUNDLE_ID: &str = "com.seuput.omca";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Package {
    MacZip,
    WindowsMsi,
    WindowsPortable,
}
impl Package {
    pub fn platform(self) -> &'static str {
        match self {
            Self::MacZip => "macos-aarch64",
            Self::WindowsMsi | Self::WindowsPortable => "windows-x64",
        }
    }
}

/// The exact UTF-8 JSON bytes are signed, not a parsed/re-serialized representation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub app: String,
    pub repo: String,
    pub version: semver::Version,
    pub platform: String,
    pub package: Package,
    pub asset: String,
    pub size: u64,
    pub sha256: String,
    pub executable_sha256: String,
}

pub struct VerifiedManifest(Manifest);
impl VerifiedManifest {
    pub fn manifest(&self) -> &Manifest {
        &self.0
    }
}

pub fn verify_manifest(bytes: &[u8], signature: &[u8], running: &str, offered: &str, package: Package) -> Result<VerifiedManifest, String> {
    verify_with_key(bytes, signature, running, offered, package, PUBLIC_KEY_HEX)
}
fn verify_with_key(bytes: &[u8], signature: &[u8], running: &str, offered: &str, package: Package, public_key: &str) -> Result<VerifiedManifest, String> {
    if bytes.len() > 16 * 1024 {
        return Err("The update manifest is too large.".into());
    }
    let key: [u8; 32] = hex::decode(public_key).map_err(err)?.try_into().map_err(|_| "Invalid update public key.")?;
    VerifyingKey::from_bytes(&key)
        .map_err(err)?
        .verify_strict(bytes, &Signature::from_slice(signature).map_err(err)?)
        .map_err(|_| "The OMCA update signature is invalid.")?;
    let m: Manifest = serde_json::from_slice(bytes).map_err(err)?;
    let running = semver::Version::parse(running).map_err(err)?;
    let offered = semver::Version::parse(offered.trim_start_matches('v')).map_err(err)?;
    if m.schema != 1
        || m.app != "omca"
        || m.repo != "seuput-inc/omca"
        || m.package != package
        || m.platform != package.platform()
        || m.version != offered
        || m.version <= running
        || !m.version.pre.is_empty()
        || m.size == 0
        || m.size > MAX_ARCHIVE
        || !hex_hash(&m.sha256)
        || !hex_hash(&m.executable_sha256)
        || m.asset != asset_name(package, &m.version.to_string())
    {
        return Err("The signed update does not match this OMCA installation or a newer stable release.".into());
    }
    Ok(VerifiedManifest(m))
}
fn hex_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Installation {
    pub executable: PathBuf,
    pub root: PathBuf,
    pub package: Package,
}
impl Installation {
    pub fn current() -> Result<Self, String> {
        Self::at(&std::env::current_exe().map_err(err)?)
    }
    fn at(executable: &Path) -> Result<Self, String> {
        let executable = executable.canonicalize().map_err(err)?;
        #[cfg(target_os = "macos")]
        {
            if std::env::consts::ARCH != "aarch64" {
                return Err("No update package for this Mac architecture.".into());
            }
            let root = executable.ancestors().nth(3).ok_or("OMCA is not inside an app bundle.")?.to_path_buf();
            if executable != root.join("Contents/MacOS/craft-anything") || root.extension().is_none_or(|e| e != "app") {
                return Err("OMCA is not inside an app bundle.".into());
            }
            mac_identity(&root, None)?;
            Ok(Self { executable, root, package: Package::MacZip })
        }
        #[cfg(windows)]
        {
            if std::env::consts::ARCH != "x86_64" || executable.file_name().is_none_or(|n| n != "craft-anything.exe") {
                return Err("This OMCA executable cannot update itself.".into());
            }
            let root = executable.parent().ok_or("Missing installation folder.")?.to_path_buf();
            let package = if root.join("portable.txt").is_file() {
                Package::WindowsPortable
            } else {
                let installed =
                    powershell("$p=(Get-ItemProperty -LiteralPath 'HKLM:\\Software\\OMCA' -ErrorAction Stop).InstallDir; [Console]::Write($p)", &[])?;
                if Path::new(installed.trim()).canonicalize().map_err(err)? != root {
                    return Err("This executable is not the registered OMCA installation.".into());
                }
                Package::WindowsMsi
            };
            Ok(Self { executable, root, package })
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = executable;
            Err("Internal updates support macOS and Windows installations.".into())
        }
    }
}

pub fn asset_name(package: Package, version: &str) -> String {
    match package {
        Package::MacZip => format!("omca-{version}-macos-aarch64-update.zip"),
        Package::WindowsMsi => format!("craft-anything-{version}-windows-x64.msi"),
        Package::WindowsPortable => format!("craft-anything-{version}-windows-x64-portable.zip"),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    schema: u32,
    install: Installation,
    owner: u32,
    owner_identity: String,
    old_sha256: String,
    running: String,
    offered: String,
    manifest: Vec<u8>,
    signature: Vec<u8>,
}

/// Owns private staging and the exclusive per-installation update lease until handoff.
pub struct Restart {
    staging: tempfile::TempDir,
    lease: File,
}
impl std::fmt::Debug for Restart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Restart").finish_non_exhaustive()
    }
}
impl Restart {
    pub fn start(self) -> Result<(), String> {
        let helper = self.staging.path().join(helper_name());
        Launch::new(&helper)
            .arg(HELPER_FLAG)
            .arg(self.staging.path())
            .cwd(self.staging.path())
            .log_to(self.staging.path().join("helper.log"))
            .hide_console()
            .start()
            .map_err(err)?;
        // The child acquires the lease after this handle closes. Staging survives our exit.
        let Self { staging, lease } = self;
        let _ = staging.keep();
        drop(lease);
        Ok(())
    }
}
fn helper_name() -> &'static str {
    if cfg!(windows) { "omca-update-helper.exe" } else { "omca-update-helper" }
}
fn lock_path(install: &Installation) -> Result<PathBuf, String> {
    // Local temp is private; the filename binds the lease to the canonical installation.
    let digest = hex::encode(Sha256::digest(install.root.to_string_lossy().as_bytes()));
    Ok(std::env::temp_dir().join(format!("omca-update-{digest}.lock")))
}
fn lease(install: &Installation) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    }
    let file = options.open(lock_path(install)?).map_err(err)?;
    file.try_lock().map_err(|_| "Another update is already pending for this OMCA installation.".to_string())?;
    Ok(file)
}

/// Verify and stage a downloaded artifact. Never changes the installed app.
pub fn prepare(
    install: Installation,
    manifest: Vec<u8>,
    signature: Vec<u8>,
    artifact: &Path,
    running: &str,
    offered: &str,
    cache: &Path,
) -> Result<Restart, String> {
    if running != env!("CARGO_PKG_VERSION") {
        return Err("The updater's running version does not match this OMCA binary.".into());
    }
    let verified = verify_manifest(&manifest, &signature, running, offered, install.package)?;
    let current = Installation::current()?;
    if current.root != install.root || current.executable != install.executable || current.package != install.package {
        return Err("OMCA's installation changed while downloading the update.".into());
    }
    let lease = lease(&install)?;
    verify_artifact(artifact, &verified.0)?;
    std::fs::create_dir_all(cache).map_err(err)?;
    let staging = tempfile::Builder::new().prefix("omca-update-").tempdir_in(cache).map_err(err)?;
    std::fs::copy(artifact, staging.path().join("artifact")).map_err(err)?;
    verify_artifact(&staging.path().join("artifact"), &verified.0)?;
    let helper = staging.path().join(helper_name());
    std::fs::copy(&install.executable, &helper).map_err(err)?;
    let job = Job {
        schema: 1,
        old_sha256: hash_file(&install.executable)?,
        owner: std::process::id(),
        owner_identity: process_identity(std::process::id())?.ok_or("The updating process disappeared.")?,
        install,
        running: running.into(),
        offered: offered.into(),
        manifest,
        signature,
    };
    let bytes = serde_json::to_vec(&job).map_err(err)?;
    crate::fs::write_atomic_private(&staging.path().join("job.json"), &bytes).map_err(err)?;
    // Preflight extraction and all identity checks before asking the app to close.
    prepare_payload(staging.path(), &job, &verified.0)?;
    if job.install.package != Package::WindowsMsi {
        probe_version(&install_executable(staging.path(), job.install.package)?, offered)?;
    }
    Ok(Restart { staging, lease })
}
fn hash_file(path: &Path) -> Result<String, String> {
    let (mut file, meta) = crate::fs::open_regular_file(path).map_err(err)?;
    if meta.len() > MAX_ARCHIVE {
        return Err("Update file exceeds its size limit.".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut remaining = MAX_ARCHIVE;
    loop {
        let count = file.read(&mut buffer).map_err(err)?;
        if count == 0 {
            break;
        }
        remaining = remaining.checked_sub(count as u64).ok_or("Update file exceeds its size limit.")?;
        hash.update(buffer.get(..count).ok_or("Invalid update read.")?);
    }
    Ok(hex::encode(hash.finalize()))
}
fn verify_artifact(path: &Path, manifest: &Manifest) -> Result<(), String> {
    if std::fs::metadata(path).map_err(err)?.len() != manifest.size || hash_file(path)? != manifest.sha256 {
        return Err("The downloaded update does not match its signed SHA-256.".into());
    }
    Ok(())
}

fn extract(archive: &Path, destination: &Path) -> Result<(), String> {
    std::fs::create_dir(destination).map_err(err)?;
    let mut zip = zip::ZipArchive::new(File::open(archive).map_err(err)?).map_err(err)?;
    if zip.len() > 100_000 {
        return Err("Too many files in update archive.".into());
    }
    let mut total = 0u64;
    let mut seen = std::collections::HashSet::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(err)?;
        let relative = entry.enclosed_name().ok_or("Unsafe update archive path.")?;
        if relative.components().any(|c| !matches!(c, Component::Normal(_)))
            || entry.name().map_err(err)?.contains(['\\', ':'])
            || !seen.insert(relative.to_string_lossy().to_lowercase())
        {
            return Err("Unsafe or duplicate update archive path.".into());
        }
        let mode = entry.unix_mode().unwrap_or(0o644);
        if mode & 0o170000 != 0 && mode & 0o170000 != 0o100000 && mode & 0o170000 != 0o040000 {
            return Err("Links and special files are not allowed in updates.".into());
        }
        total = total.checked_add(entry.size()).filter(|n| *n <= MAX_EXPANDED).ok_or("Update archive expands beyond its limit.")?;
        let path = destination.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&path).map_err(err)?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(err)?;
        }
        let mut output = File::create_new(&path).map_err(err)?;
        let written = std::io::copy(&mut (&mut entry).take(MAX_EXPANDED + 1), &mut output).map_err(err)?;
        if written != entry.size() {
            return Err("Update archive contains an invalid file size.".into());
        }
        output.sync_all().map_err(err)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777)).map_err(err)?;
        }
    }
    Ok(())
}
fn single_root(dir: &Path) -> Result<PathBuf, String> {
    let mut entries = std::fs::read_dir(dir).map_err(err)?;
    let root = entries.next().ok_or("Empty update archive.")?.map_err(err)?.path();
    if entries.next().is_some() || !root.is_dir() {
        return Err("Update archive must contain exactly one app folder.".into());
    }
    Ok(root)
}
fn install_executable(staging: &Path, package: Package) -> Result<PathBuf, String> {
    match package {
        Package::MacZip => Ok(staging.join("extracted/OMCA.app/Contents/MacOS/craft-anything")),
        Package::WindowsPortable => Ok(single_root(&staging.join("extracted"))?.join("craft-anything.exe")),
        Package::WindowsMsi => Err("MSI executable probing happens after Windows Installer commits.".into()),
    }
}
fn prepare_payload(staging: &Path, job: &Job, manifest: &Manifest) -> Result<PathBuf, String> {
    if job.install.package == Package::WindowsMsi {
        return Ok(staging.join("artifact"));
    }
    let extracted = staging.join("extracted");
    extract(&staging.join("artifact"), &extracted)?;
    let root = single_root(&extracted)?;
    let executable = if job.install.package == Package::MacZip {
        if root.file_name().is_none_or(|n| n != "OMCA.app") {
            return Err("The update is not OMCA.app.".into());
        }
        mac_identity(&root, Some(&manifest.version.to_string()))?;
        root.join("Contents/MacOS/craft-anything")
    } else {
        root.join("craft-anything.exe")
    };
    if hash_file(&executable)? != manifest.executable_sha256 {
        return Err("The update executable does not match the signed identity.".into());
    }
    if job.install.package == Package::MacZip {
        #[cfg(target_os = "macos")]
        {
            Process::new("/usr/bin/codesign")
                .args(["--verify", "--strict", "--deep"])
                .arg(&root)
                .timeout(Duration::from_secs(90))
                .run_checked()
                .map_err(err)?;
        }
    }
    Ok(root)
}

#[cfg(target_os = "macos")]
fn mac_identity(root: &Path, version: Option<&str>) -> Result<(), String> {
    let plist = root.join("Contents/Info.plist");
    let property = |name: &str| {
        Process::new("/usr/libexec/PlistBuddy")
            .args(["-c", &format!("Print :{name}")])
            .arg(&plist)
            .timeout(Duration::from_secs(5))
            .stdout_cap(4096)
            .run_checked()
            .map(|o| o.stdout_text().trim().to_owned())
            .map_err(err)
    };
    if property("CFBundleIdentifier")? != BUNDLE_ID
        || property("CFBundleExecutable")? != "craft-anything"
        || version.is_some_and(|v| property("CraftAnythingVersion").as_deref() != Ok(v))
    {
        return Err("The app bundle's OMCA identity or version is incorrect.".into());
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn mac_identity(_root: &Path, _version: Option<&str>) -> Result<(), String> {
    Err("Mac bundles must be verified on macOS.".into())
}

#[cfg(windows)]
fn powershell(script: &str, env: &[(&str, &std::ffi::OsStr)]) -> Result<String, String> {
    let shell = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .ok_or("Windows system folder is unavailable.")?
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let mut process = Process::new(shell).args(["-NoProfile", "-NonInteractive", "-Command", script]).timeout(Duration::from_secs(900)).stdout_cap(16 * 1024);
    for (key, value) in env {
        process = process.env(key, value);
    }
    process.run_checked().map(|o| o.stdout_text()).map_err(err)
}
fn process_identity(pid: u32) -> Result<Option<String>, String> {
    #[cfg(unix)]
    {
        let out = Process::new("/bin/ps")
            .args(["-p", &pid.to_string(), "-o", "lstart=", "-o", "comm="])
            .timeout(Duration::from_secs(5))
            .stdout_cap(16 * 1024)
            .run()
            .map_err(err)?;
        if out.code == Some(1) {
            return Ok(None);
        }
        if !out.success {
            return Err("Could not establish the update owner's process identity.".into());
        }
        Ok(Some(out.stdout_text().trim().to_owned()))
    }
    #[cfg(windows)]
    {
        let id = std::ffi::OsString::from(pid.to_string());
        let value = powershell(include_str!("update_owner.ps1"), &[("OMCA_OWNER_PID", &id)])?;
        Ok((!value.is_empty()).then_some(value))
    }
}
fn wait_for_owner(job: &Job) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(90);
    while process_identity(job.owner)?.as_deref() == Some(&job.owner_identity) {
        if Instant::now() >= deadline {
            return Err("OMCA did not exit; the update was not installed.".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

/// Handle only our private helper flag, before opening a GUI or reading credentials.
pub fn finish(args: &[std::ffi::OsString]) -> Option<Result<(), String>> {
    if args.first().is_none_or(|a| a != HELPER_FLAG) {
        #[cfg(windows)]
        if !args.iter().any(|a| a == "--version" || a == "-V") {
            return portable::resume();
        }
        return None;
    }
    Some((|| {
        if args.len() != 2 {
            return Err("Invalid update helper arguments.".into());
        }
        let staging = PathBuf::from(args.get(1).ok_or("Missing update staging folder.")?).canonicalize().map_err(err)?;
        let me = std::env::current_exe().map_err(err)?.canonicalize().map_err(err)?;
        if me != staging.join(helper_name()) {
            return Err("Only OMCA's prepared helper may install an update.".into());
        }
        let bytes = read_file_capped(&staging.join("job.json"), 64 * 1024).map_err(err)?;
        let job: Job = serde_json::from_slice(&bytes).map_err(err)?;
        let verified = verify_manifest(&job.manifest, &job.signature, &job.running, &job.offered, job.install.package)?;
        let recovering = job.install.package == Package::WindowsPortable && job.install.root.join("omca-update-pending.json").exists();
        let authorized_hash = |hash: &str| hash == job.old_sha256 || (recovering && hash == verified.0.executable_sha256);
        if job.schema != 1
            || job.owner == 0
            || job.owner == std::process::id()
            || (env!("CARGO_PKG_VERSION") != job.running && !(recovering && env!("CARGO_PKG_VERSION") == job.offered))
            || !authorized_hash(&hash_file(&me)?)
            || !authorized_hash(&hash_file(&job.install.executable)?)
        {
            return Err("The update helper does not match the installed OMCA executable.".into());
        }
        let actual = Installation::at(&job.install.executable)?;
        if actual.root != job.install.root || actual.package != job.install.package {
            return Err("The update destination no longer identifies this OMCA installation.".into());
        }
        wait_for_owner(&job)?;
        let lease = lease(&job.install)?;
        // Check again under the lease: another instance may have updated while we waited.
        if !authorized_hash(&hash_file(&job.install.executable)?) {
            return Err("A different OMCA version was installed before this helper acquired its lease.".into());
        }
        let actual = Installation::at(&job.install.executable)?;
        if actual.root != job.install.root || actual.package != job.install.package {
            return Err("The update destination changed before installation.".into());
        }
        let result: Result<semver::Version, String> = (|| {
            verify_artifact(&staging.join("artifact"), &verified.0)?;
            // Ignore preflight output. Re-extract verified bytes after the owner exits.
            if staging.join("extracted").exists() {
                std::fs::remove_dir_all(staging.join("extracted")).map_err(err)?;
            }
            let payload = prepare_payload(&staging, &job, &verified.0)?;
            match job.install.package {
                Package::WindowsMsi => install_msi(&job, &verified.0, &payload)?,
                Package::WindowsPortable => portable::replace(&job, &payload, &staging)?,
                Package::MacZip => replace_tree(&job, &payload)?,
            }
            Ok(verified.0.version)
        })();
        let outcome = match &result {
            Ok(version) => Outcome::Installed(version.clone()),
            Err(error) => Outcome::Failed(error.clone()),
        };
        let record = OutcomeRecord { schema: 1, outcome };
        let _ = crate::fs::write_atomic_private(&outcome_path(&job.install.root), &serde_json::to_vec(&record).map_err(err)?);
        drop(lease);
        // Do not race an MSI transaction or recursively restart an incomplete portable recovery.
        let pending_portable = job.install.package == Package::WindowsPortable && job.install.root.join("omca-update-pending.json").exists();
        let restarted =
            if result.is_err() && (job.install.package == Package::WindowsMsi || pending_portable) { Ok(()) } else { launch_installed(&job.install) };
        // Keep failed staging for diagnostics. Never delete the installed app or backups here.
        if result.is_ok() && restarted.is_ok() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        result.map(|_| ()).and(restarted)
    })())
}
fn launch_installed(install: &Installation) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if install.package == Package::MacZip {
        return Launch::new("/usr/bin/open").arg("-n").arg(&install.root).start().map(|_| ()).map_err(err);
    }
    Launch::new(&install.executable).cwd(&install.root).start().map(|_| ()).map_err(err)
}
#[cfg(target_os = "macos")]
fn replace_tree(job: &Job, payload: &Path) -> Result<(), String> {
    let parent = job.install.root.parent().ok_or("Missing installation parent.")?;
    let transaction = tempfile::Builder::new().prefix(".omca-update-").tempdir_in(parent).map_err(err)?;
    let replacement = transaction.path().join("replacement");
    copy_tree(payload, &replacement)?;
    let directory = File::open(parent).map_err(err)?;
    rustix::fs::renameat_with(&directory, &replacement, &directory, &job.install.root, rustix::fs::RenameFlags::EXCHANGE).map_err(err)?;
    let checked = directory.sync_all().map_err(err).and_then(|()| probe_version(&job.install.executable, &job.offered));
    if let Err(error) = checked {
        if let Err(rollback) = rustix::fs::renameat_with(&directory, &replacement, &directory, &job.install.root, rustix::fs::RenameFlags::EXCHANGE) {
            let saved = transaction.keep();
            return Err(format!("{error}; rollback failed ({rollback}); previous app preserved in {}.", saved.display()));
        }
        directory.sync_all().map_err(err)?;
        return Err(error);
    }
    Ok(())
}
#[cfg(not(target_os = "macos"))]
fn replace_tree(_job: &Job, _payload: &Path) -> Result<(), String> {
    Err("App bundles can only be replaced on macOS.".into())
}
fn probe_version(executable: &Path, offered: &str) -> Result<(), String> {
    let out = Process::new(executable).arg("--version").timeout(Duration::from_secs(20)).stdout_cap(4096).run_checked().map_err(err)?;
    let text = out.stdout_text();
    if !text.split_whitespace().any(|v| v == offered.trim_start_matches('v')) {
        return Err("The installed OMCA executable reports the wrong version.".into());
    }
    Ok(())
}
#[cfg(target_os = "macos")]
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir(to).map_err(err)?;
    overlay_tree(from, to)
}
#[cfg(target_os = "macos")]
fn overlay_tree(from: &Path, to: &Path) -> Result<(), String> {
    let mut pending = vec![(from.to_path_buf(), to.to_path_buf())];
    let mut count = 0usize;
    let mut directories = vec![to.to_path_buf()];
    let mut total = 0u64;
    while let Some((source, destination)) = pending.pop() {
        for entry in std::fs::read_dir(source).map_err(err)? {
            count += 1;
            if count > 100_000 {
                return Err("Too many installation files.".into());
            }
            let entry = entry.map_err(err)?;
            let metadata = entry.file_type().map_err(err)?;
            let target = destination.join(entry.file_name());
            if metadata.is_symlink() {
                return Err("Linked files are not allowed in the installation transaction.".into());
            }
            if metadata.is_dir() {
                std::fs::create_dir_all(&target).map_err(err)?;
                directories.push(target.clone());
                pending.push((entry.path(), target));
            } else if metadata.is_file() {
                total = total.checked_add(entry.metadata().map_err(err)?.len()).filter(|n| *n <= MAX_EXPANDED).ok_or("Installation exceeds its size limit.")?;
                std::fs::copy(entry.path(), &target).map_err(err)?;
                File::open(target).map_err(err)?.sync_all().map_err(err)?;
            } else {
                return Err("Special files are not allowed in the installation transaction.".into());
            }
        }
    }
    for directory in directories.iter().rev() {
        File::open(directory).map_err(err)?.sync_all().map_err(err)?;
    }
    Ok(())
}
#[cfg(windows)]
fn install_msi(job: &Job, manifest: &Manifest, payload: &Path) -> Result<(), String> {
    // Hold a deny-write/delete handle for both identity validation and the elevated install.
    use std::os::windows::fs::OpenOptionsExt;
    let _artifact = OpenOptions::new().read(true).share_mode(1).open(payload).map_err(err)?;
    verify_artifact(payload, manifest)?;
    let version = manifest.version.to_string();
    let msi = crate::process::native_path(payload);
    let root = crate::process::native_path(&job.install.root);
    powershell(
        include_str!("update_msi.ps1"),
        &[("OMCA_UPDATE_MSI", &msi), ("OMCA_UPDATE_ROOT", &root), ("OMCA_UPDATE_VERSION", std::ffi::OsStr::new(&version))],
    )?;
    if hash_file(&job.install.executable)? != manifest.executable_sha256 {
        return Err("The installed executable does not match the signed OMCA release.".into());
    }
    probe_version(&job.install.executable, &job.offered)
}
#[cfg(not(windows))]
fn install_msi(_job: &Job, _manifest: &Manifest, _payload: &Path) -> Result<(), String> {
    Err("Windows installers can only run on Windows.".into())
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Outcome {
    Installed(semver::Version),
    Failed(String),
}
#[derive(Serialize, Deserialize)]
struct OutcomeRecord {
    schema: u32,
    outcome: Outcome,
}
fn outcome_path(root: &Path) -> PathBuf {
    let digest = hex::encode(Sha256::digest(root.to_string_lossy().as_bytes()));
    std::env::temp_dir().join(format!("omca-update-{digest}.json"))
}
pub fn take_outcome() -> Option<Outcome> {
    // No subprocesses on the UI thread. This receipt is display-only, never installation authority.
    let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
    let root = if cfg!(target_os = "macos") { executable.ancestors().nth(3)? } else { executable.parent()? };
    let path = outcome_path(root);
    let bytes = read_file_capped(&path, 16 * 1024).ok()?;
    let record: OutcomeRecord = serde_json::from_slice(&bytes).ok()?;
    let _ = std::fs::remove_file(path);
    (record.schema == 1).then_some(record.outcome)
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
