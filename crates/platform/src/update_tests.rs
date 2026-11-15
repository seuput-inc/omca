#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use ed25519_dalek::{Signer, SigningKey};
use std::io::Write;

#[cfg(windows)]
#[test]
fn exited_owner_is_absent_not_an_update_failure() {
    let current = process_identity(std::process::id()).unwrap().expect("running owner identity");
    assert!(!current.is_empty());
    let mut child = std::process::Command::new("cmd.exe").args(["/D", "/C", "exit", "0"]).spawn().unwrap();
    let pid = child.id();
    assert!(child.wait().unwrap().success());
    assert_eq!(process_identity(pid).unwrap(), None);
}

fn manifest() -> Manifest {
    Manifest {
        schema: 1,
        app: "omca".into(),
        repo: "seuput-inc/omca".into(),
        version: semver::Version::new(0, 4, 6),
        platform: "macos-aarch64".into(),
        package: Package::MacZip,
        asset: "omca-0.4.6-macos-aarch64-update.zip".into(),
        size: 7,
        sha256: "a".repeat(64),
        executable_sha256: "b".repeat(64),
    }
}
fn signed(m: &Manifest) -> (Vec<u8>, Vec<u8>, String) {
    let key = SigningKey::from_bytes(&[37; 32]);
    let bytes = serde_json::to_vec(m).unwrap();
    let signature = key.sign(&bytes).to_bytes().to_vec();
    (bytes, signature, hex::encode(key.verifying_key().to_bytes()))
}
#[test]
fn trusted_exact_bytes_bind_identity_platform_asset_and_upgrade() {
    let m = manifest();
    let (bytes, sig, public) = signed(&m);
    assert_eq!(verify_with_key(&bytes, &sig, "0.4.5", "v0.4.6", Package::MacZip, &public).unwrap().manifest().version, m.version);
    let mut altered = bytes.clone();
    altered.push(b' ');
    assert!(verify_with_key(&altered, &sig, "0.4.5", "0.4.6", Package::MacZip, &public).is_err());
    assert!(verify_manifest(&bytes, &sig, "0.4.5", "0.4.6", Package::MacZip).is_err(), "test key must never become a production trust root");
    assert!(verify_with_key(&bytes, &sig, "0.4.6", "0.4.6", Package::MacZip, &public).is_err());
    assert!(verify_with_key(&bytes, &sig, "0.4.5", "0.4.6", Package::WindowsPortable, &public).is_err());
    for field in ["app", "repo", "platform", "asset"] {
        let mut wrong = serde_json::to_value(&m).unwrap();
        wrong[field] = serde_json::Value::String("other".into());
        let wrong: Manifest = serde_json::from_value(wrong).unwrap();
        let (bytes, sig, _) = signed(&wrong);
        assert!(verify_with_key(&bytes, &sig, "0.4.5", "0.4.6", Package::MacZip, &public).is_err(), "{field}");
    }
}
fn archive(path: &Path, names: &[&str]) {
    let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
    for name in names {
        zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(b"fake payload").unwrap();
    }
    zip.finish().unwrap();
}
#[test]
fn hostile_archive_cannot_escape_or_replace_user_data() {
    for names in [vec!["../outside"], vec!["app/file", "app/FILE"], vec!["app/C:escape"], vec!["app\\escape"]] {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("update.zip");
        archive(&zip, &names);
        assert!(extract(&zip, &dir.path().join("extracted")).is_err(), "{names:?}");
        assert!(!dir.path().join("outside").exists());
    }
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("update.zip");
    let mut writer = zip::ZipWriter::new(File::create(&zip).unwrap());
    writer.add_symlink("app/link", "../outside", zip::write::SimpleFileOptions::default()).unwrap();
    writer.finish().unwrap();
    assert!(extract(&zip, &dir.path().join("extracted")).is_err());
}
#[test]
fn changed_artifact_fails_before_installation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("artifact");
    std::fs::write(&path, b"release").unwrap();
    let mut m = manifest();
    m.sha256 = hash_file(&path).unwrap();
    verify_artifact(&path, &m).unwrap();
    std::fs::write(&path, b"altered").unwrap();
    assert!(verify_artifact(&path, &m).is_err());
}
fn job(root: PathBuf, executable: PathBuf, package: Package) -> Job {
    Job {
        schema: 1,
        install: Installation { executable, root, package },
        owner: 1,
        owner_identity: String::new(),
        old_sha256: String::new(),
        running: "0.4.5".into(),
        offered: "0.4.6".into(),
        manifest: vec![],
        signature: vec![],
    }
}
#[test]
fn duplicate_and_cancelled_updates_keep_installation_and_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("OMCA");
    std::fs::create_dir(&root).unwrap();
    let executable = root.join("craft-anything");
    std::fs::write(&executable, b"old executable").unwrap();
    let credentials = dir.path().join("agent.db");
    std::fs::write(&credentials, b"synthetic OMCA credential record").unwrap();
    let job = job(root, executable.clone(), Package::MacZip);
    let staging = tempfile::tempdir_in(dir.path()).unwrap();
    let staging_path = staging.path().to_path_buf();
    let held = lease(&job.install).unwrap();
    assert!(lease(&job.install).is_err());
    drop(Restart { staging, lease: held });
    assert!(!staging_path.exists(), "cancellation cleans only private staging");
    assert_eq!(std::fs::read(executable).unwrap(), b"old executable");
    assert_eq!(std::fs::read(credentials).unwrap(), b"synthetic OMCA credential record");
    assert!(lease(&job.install).is_ok(), "cancelled update releases its lease");
    let _ = std::fs::remove_file(lock_path(&job.install).unwrap());
}
#[cfg(target_os = "macos")]
fn executable(path: &Path, version: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, format!("#!/bin/sh\nprintf 'craft-anything {version}\\n'\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
#[cfg(target_os = "macos")]
#[test]
fn real_atomic_app_exchange_and_rollback_preserve_external_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("OMCA.app");
    let payload = dir.path().join("payload");
    for folder in [&root, &payload] {
        std::fs::create_dir_all(folder.join("Contents/MacOS")).unwrap();
    }
    let relative = Path::new("Contents/MacOS/craft-anything");
    executable(&root.join(relative), "0.4.5");
    executable(&payload.join(relative), "0.4.6");
    let credentials = dir.path().join("agent.db");
    std::fs::write(&credentials, b"fake-auth-persisted").unwrap();
    let job = job(root.clone(), root.join(relative), Package::MacZip);
    replace_tree(&job, &payload).unwrap();
    probe_version(&job.install.executable, "0.4.6").unwrap();
    executable(&payload.join(relative), "0.4.7");
    assert!(replace_tree(&job, &payload).is_err(), "wrong version must roll back");
    probe_version(&job.install.executable, "0.4.6").unwrap();
    assert_eq!(std::fs::read(credentials).unwrap(), b"fake-auth-persisted");
}
#[cfg(windows)]
#[test]
fn real_msi_uses_the_registered_omca_identity() {
    let Some(msi) = std::env::var_os("OMCA_UPDATE_TEST_MSI") else { return };
    let path = PathBuf::from(msi);
    let installed = powershell("$p=(Get-ItemProperty -LiteralPath 'HKLM:\\Software\\OMCA' -ErrorAction Stop).InstallDir; [Console]::Write($p)", &[]).unwrap();
    let root = PathBuf::from(installed.trim());
    let mut m = manifest();
    m.package = Package::WindowsMsi;
    m.platform = "windows-x64".into();
    m.version = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    m.size = std::fs::metadata(&path).unwrap().len();
    m.sha256 = hash_file(&path).unwrap();
    m.executable_sha256 = hash_file(&root.join("craft-anything.exe")).unwrap();
    let mut job = job(root.clone(), root.join("craft-anything.exe"), Package::WindowsMsi);
    job.offered = m.version.to_string();
    install_msi(&job, &m, &path).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn interrupted_portable_transaction_resumes_without_reading_or_replacing_credentials() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("OMCA");
    let payload = dir.path().join("payload");
    let staging = dir.path().join("staging");
    for path in [&root, &payload, &staging.join("portable-backup")] {
        std::fs::create_dir_all(path).unwrap();
    }
    let exe = root.join("craft-anything.exe");
    executable(&exe, "0.4.5");
    executable(&payload.join("craft-anything.exe"), "0.4.6");
    std::fs::write(root.join("release.txt"), b"old").unwrap();
    std::fs::write(payload.join("release.txt"), b"new").unwrap();
    std::fs::create_dir(root.join("OMCAData")).unwrap();
    let credentials = root.join("OMCAData/agent.db");
    std::fs::write(&credentials, b"credential-stays-in-place").unwrap();
    let inode = std::fs::metadata(&credentials).unwrap().ino();
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::copy(&exe, staging.join("portable-backup/craft-anything.exe")).unwrap();
    std::fs::copy(root.join("release.txt"), staging.join("portable-backup/release.txt")).unwrap();
    std::fs::write(staging.join("portable-plan.json"), r#"{"schema":1,"files":[["craft-anything.exe",true],["release.txt",true]]}"#).unwrap();
    // The first release file was already replaced when the previous helper died.
    std::fs::copy(payload.join("release.txt"), root.join("release.txt")).unwrap();
    let job = job(root.clone(), exe.clone(), Package::WindowsPortable);
    portable::replace(&job, &payload, &staging).unwrap();
    probe_version(&exe, "0.4.6").unwrap();
    assert!(!root.join("omca-update-pending.json").exists());
    assert_eq!(std::fs::metadata(&credentials).unwrap().ino(), inode);
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(std::fs::read(credentials).unwrap(), b"credential-stays-in-place");
}
