//! Portable updates replace only release files. OMCAData stays in place and is never read.
use super::*;
const PENDING: &str = "omca-update-pending.json";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    schema: u32,
    staging: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema: u32,
    files: Vec<(PathBuf, bool)>,
}
fn files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![PathBuf::new()];
    let mut files = Vec::new();
    let mut total = 0u64;
    while let Some(relative) = pending.pop() {
        for entry in std::fs::read_dir(root.join(&relative)).map_err(err)? {
            let entry = entry.map_err(err)?;
            let path = relative.join(entry.file_name());
            let name = path.components().next().ok_or("Empty update path.")?.as_os_str().to_string_lossy().to_lowercase();
            if ["omcadata", PENDING, "agent.db", "selection.json"].contains(&name.as_str()) {
                return Err("An update must not contain credentials, settings, or its transaction receipt.".into());
            }
            let kind = entry.file_type().map_err(err)?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                total =
                    total.checked_add(entry.metadata().map_err(err)?.len()).filter(|n| *n <= MAX_EXPANDED).ok_or("Update payload exceeds its size limit.")?;
                files.push(path);
            } else {
                return Err("An update may only contain regular files and folders.".into());
            }
            if files.len() + pending.len() > 100_000 {
                return Err("Too many update files.".into());
            }
        }
    }
    files.sort();
    Ok(files)
}
fn safe_destination(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err("Unsafe transaction path.".into());
    }
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component.as_os_str());
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => return Err("Linked installation paths cannot be replaced.".into()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(err(e)),
        }
    }
    Ok(path)
}
fn atomic_copy(source: &Path, destination: &Path) -> Result<(), String> {
    let parent = destination.parent().ok_or("Missing update destination folder.")?;
    std::fs::create_dir_all(parent).map_err(err)?;
    let temp = tempfile::NamedTempFile::new_in(parent).map_err(err)?;
    std::fs::copy(source, temp.path()).map_err(err)?;
    temp.as_file().sync_all().map_err(err)?;
    temp.persist(destination).map_err(err)?;
    Ok(())
}
pub(super) fn replace(job: &Job, payload: &Path, staging: &Path) -> Result<(), String> {
    let paths = files(payload)?;
    let plan_path = staging.join("portable-plan.json");
    let backup = staging.join("portable-backup");
    let plan = if plan_path.exists() {
        let bytes = read_file_capped(&plan_path, 4 * 1024 * 1024).map_err(err)?;
        let plan: Plan = serde_json::from_slice(&bytes).map_err(err)?;
        if plan.schema != 1 || plan.files.iter().map(|(p, _)| p).ne(paths.iter()) {
            return Err("The portable update journal does not match the verified release.".into());
        }
        plan
    } else {
        let mut records = Vec::with_capacity(paths.len());
        for relative in &paths {
            let target = safe_destination(&job.install.root, relative)?;
            let existed = target.try_exists().map_err(err)?;
            if existed {
                if !target.is_file() {
                    return Err("An update file conflicts with an existing folder.".into());
                }
                atomic_copy(&target, &backup.join(relative))?;
            }
            records.push((relative.clone(), existed));
        }
        let plan = Plan { schema: 1, files: records };
        crate::fs::write_atomic_private(&plan_path, &serde_json::to_vec(&plan).map_err(err)?).map_err(err)?;
        plan
    };
    let pending = Pending { schema: 1, staging: staging.to_path_buf() };
    crate::fs::write_atomic_private(&job.install.root.join(PENDING), &serde_json::to_vec(&pending).map_err(err)?).map_err(err)?;
    let applied = (|| {
        for (relative, _) in &plan.files {
            let target = safe_destination(&job.install.root, relative)?;
            atomic_copy(&payload.join(relative), &target)?;
        }
        probe_version(&job.install.executable, &job.offered)
    })();
    if let Err(error) = applied {
        for (relative, existed) in plan.files.iter().rev() {
            let target = safe_destination(&job.install.root, relative)?;
            let restored = if *existed {
                atomic_copy(&backup.join(relative), &target)
            } else if target.exists() {
                std::fs::remove_file(target).map_err(err)
            } else {
                Ok(())
            };
            if let Err(rollback) = restored {
                return Err(format!("{error}; rollback incomplete ({rollback}). The verified update will resume on the next start."));
            }
        }
        std::fs::remove_file(job.install.root.join(PENDING)).map_err(err)?;
        return Err(error);
    }
    std::fs::remove_file(job.install.root.join(PENDING)).map_err(err)?;
    Ok(())
}

/// Before starting a partially updated portable app, hand off to its signed release helper.
#[cfg(windows)]
pub(super) fn resume() -> Option<Result<(), String>> {
    let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
    let root = executable.parent()?;
    let receipt = root.join(PENDING);
    if !receipt.exists() {
        return None;
    }
    Some((|| {
        let install = Installation::at(&executable)?;
        if install.package != Package::WindowsPortable {
            return Err("Only portable OMCA installs may resume a file transaction.".into());
        }
        let pending: Pending = serde_json::from_slice(&read_file_capped(&receipt, 16 * 1024).map_err(err)?).map_err(err)?;
        if pending.schema != 1 {
            return Err("Unknown portable update journal version.".into());
        }
        let staging = pending.staging.canonicalize().map_err(err)?;
        let mut job: Job = serde_json::from_slice(&read_file_capped(&staging.join("job.json"), 64 * 1024).map_err(err)?).map_err(err)?;
        if job.schema != 1 || job.install.root != install.root || job.install.package != install.package {
            return Err("The pending update belongs to a different installation.".into());
        }
        let verified = verify_manifest(&job.manifest, &job.signature, &job.running, &job.offered, install.package)?;
        if env!("CARGO_PKG_VERSION") != job.running && env!("CARGO_PKG_VERSION") != job.offered {
            return Err("The pending update does not belong to this OMCA version.".into());
        }
        verify_artifact(&staging.join("artifact"), &verified.0)?;
        let me = hash_file(&executable)?;
        if me != job.old_sha256 && me != verified.0.executable_sha256 {
            return Err("The running file is not part of this signed OMCA update.".into());
        }
        let _lease = lease(&install)?;
        if hash_file(&executable)? != me {
            return Err("OMCA changed while acquiring the recovery lease.".into());
        }
        // Rebuild the helper exclusively from authenticated archive bytes, never the journal's paths.
        if staging.join("extracted").exists() {
            std::fs::remove_dir_all(staging.join("extracted")).map_err(err)?;
        }
        let payload = prepare_payload(&staging, &job, &verified.0)?;
        let helper = staging.join(helper_name());
        std::fs::copy(payload.join("craft-anything.exe"), &helper).map_err(err)?;
        job.owner = std::process::id();
        job.owner_identity = process_identity(job.owner)?.ok_or("The recovery owner disappeared.")?;
        crate::fs::write_atomic_private(&staging.join("job.json"), &serde_json::to_vec(&job).map_err(err)?).map_err(err)?;
        Launch::new(helper).arg(HELPER_FLAG).arg(&staging).cwd(&staging).hide_console().start().map_err(err)?;
        Ok(())
    })())
}
