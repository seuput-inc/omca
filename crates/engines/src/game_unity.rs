//! Unity first-use setup through its official CLI. Never returns account identifiers or raw
//! installer output. Authentication and licence changes require an explicit human action above
//! this layer; no module EULA or Personal eligibility is inferred here.
use super::release;
use craft_anything_evidence::{ErrorCode, ToolError};
use craft_anything_platform::process::{Cancel, Process};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{path::Path, time::Duration};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UnityAction {
    Check,
    Login,
    ActivatePersonal,
    ActivateSubscription,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UnityStatus {
    pub signed_in: bool,
    pub licensed: bool,
}

pub fn unity_setup(store: &Path, action: UnityAction, cancel: Option<Cancel>, progress: &dyn Fn(u64, Option<u64>)) -> Result<UnityStatus, ToolError> {
    let cli = release::unity_cli(store, cancel.as_ref(), progress)?;
    let home = store.join("unity-account");
    std::fs::create_dir_all(&home).map_err(|e| ToolError::io(home.display(), e))?;
    setup_with_cli(&cli, &home, action, cancel.as_ref())
}

/// Explicit native-platform release gate: downloads the checksum-verified official CLI and checks
/// its private configuration path. Does not inspect accounts, sign in or activate a licence.
pub fn unity_private_environment_probe(store: &Path) -> Result<(), ToolError> {
    let cli = release::unity_cli(store, None, &|_, _| {})?;
    let home = store.join("unity-account");
    std::fs::create_dir_all(&home).map_err(|error| ToolError::io(home.display(), error))?;
    verify_private_environment(&cli, &home, None)
}

fn setup_with_cli(cli: &Path, home: &Path, action: UnityAction, cancel: Option<&Cancel>) -> Result<UnityStatus, ToolError> {
    verify_private_environment(cli, home, cancel)?;
    match action {
        UnityAction::Check => {}
        UnityAction::Login => {
            execute(cli, home, &["auth", "login"], cancel, Duration::from_secs(10 * 60))?;
        }
        UnityAction::ActivatePersonal | UnityAction::ActivateSubscription => {
            let auth = execute(cli, home, &["auth", "status"], cancel, Duration::from_secs(60))?;
            if !auth.get("data").and_then(|d| d.get("loggedIn")).and_then(Value::as_bool).unwrap_or(false) {
                return Err(ToolError::new(ErrorCode::BadSettings, "Sign in to Unity in this app before activating a licence."));
            }
            let args: &[&str] =
                if action == UnityAction::ActivatePersonal { &["license", "activate", "--personal", "--accept-eula"] } else { &["license", "activate"] };
            execute(cli, home, args, cancel, Duration::from_secs(10 * 60))?;
        }
    }
    let auth = execute(cli, home, &["auth", "status"], cancel, Duration::from_secs(60))?;
    let licence = execute(cli, home, &["license", "status"], cancel, Duration::from_secs(5 * 60))?;
    Ok(status(&auth, &licence))
}

fn verify_private_environment(cli: &Path, home: &Path, cancel: Option<&Cancel>) -> Result<(), ToolError> {
    let environment = execute(cli, home, &["env"], cancel, Duration::from_secs(60))?;
    let isolated = environment.get("data").and_then(|data| data.get("userDataPath")).and_then(Value::as_str).is_some_and(|path| {
        let path = Path::new(path);
        path.starts_with(home) && !path.components().any(|part| matches!(part, std::path::Component::ParentDir))
    });
    if !isolated {
        return Err(ToolError::new(
            ErrorCode::NotSupported,
            "The official Unity CLI did not use the app-private account folder. No account operation was performed.",
        ));
    }
    Ok(())
}

fn status(auth: &Value, licence: &Value) -> UnityStatus {
    UnityStatus {
        signed_in: auth.get("data").and_then(|d| d.get("loggedIn")).and_then(Value::as_bool).unwrap_or(false),
        licensed: licence
            .get("data")
            .is_some_and(|d| d.get("active").and_then(Value::as_bool) == Some(true) || d.get("floatingServerHealthy").and_then(Value::as_bool) == Some(true)),
    }
}

/// Private HOME also isolates Unity Hub's account-list pointer, so the CLI cannot pick a personal
/// account silently. The official CLI still uses the OS credential store for secrets. We never
/// run auth switch, read that store, print tokens, or permit insecure file storage.
fn command(cli: &Path, home: &Path, args: &[&str]) -> Process {
    private_process(Process::new(cli).args(args).args(["--format", "json", "--no-banner", "--no-pager", "--no-log-proxy", "--non-interactive"]), home)
        .stdout_cap(128 << 10)
        .stderr_cap(128 << 10)
        .label("Unity account setup")
}

/// Managed Editors use the same private licensing-client home as setup, without changing the
/// environment of the coding agent or an Editor the person installed outside this app.
pub fn unity_process(program: &Path, process: Process) -> Process {
    let Some(root) = program.ancestors().find(|path| path.file_name().is_some_and(|name| name == "unity")) else { return process };
    let Some(store) = root.parent() else { return process };
    private_process(process, &store.join("unity-account"))
}

pub(super) fn private_process(process: Process, home: &Path) -> Process {
    process
        .cwd(home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("APPDATA", home.join("AppData/Roaming"))
        .env("LOCALAPPDATA", home.join("AppData/Local"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("UNITY_CLI_HOME", home.join("cli"))
        .env("UNITY_NO_CLI_INVOKED_TELEMETRY", "1")
        .env("UNITY_NO_CRASH_REPORT", "1")
        .env("UNITY_NO_UPDATE_CHECK", "1")
        .env_remove("UNITY_SERVICE_ACCOUNT_ID")
        .env_remove("UNITY_SERVICE_ACCOUNT_SECRET")
        .env_remove("UNITY_LICENSE_SERIAL")
        .env_remove("HUB_ENV")
        .env_remove("UNITY_PROJECT_PATH")
}

fn execute(cli: &Path, home: &Path, args: &[&str], cancel: Option<&Cancel>, timeout: Duration) -> Result<Value, ToolError> {
    let process = command(cli, home, args).timeout(timeout).idle_timeout(Duration::from_secs(10 * 60));
    let process = if let Some(cancel) = cancel { process.cancel(cancel.clone()) } else { process };
    // Raw output can contain sign-in URLs, email addresses or licensing identities. It is parsed
    // here and never put into ToolError, logs, progress, the tool result or the UI.
    let out = process.run().map_err(|e| match e.code {
        ErrorCode::Cancelled => ToolError::cancelled(),
        ErrorCode::TimedOut => ToolError::new(ErrorCode::TimedOut, "Unity setup timed out. You can retry from this app."),
        _ => ToolError::new(ErrorCode::ProgramFailed, "The official Unity setup program could not run."),
    })?;
    if out.stdout_truncated {
        return Err(ToolError::new(ErrorCode::OverLimit, "Unity's setup reply exceeded the safe output limit."));
    }
    let reply: Value =
        serde_json::from_slice(&out.stdout).map_err(|_| ToolError::new(ErrorCode::GarbledOutput, "Unity returned an unreadable setup status."))?;
    let expected_absence = matches!(args, ["auth", "status"] | ["license", "status"]) && matches!(out.code, Some(3 | 4));
    if !out.success && !expected_absence {
        return Err(ToolError::new(ErrorCode::ProgramFailed, "Unity could not complete this setup step.")
            .with_remediation("Check the official sign-in window and your licence eligibility, then retry in this app."));
    }
    Ok(reply)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fixture(dir: &Path, response: &str) -> std::path::PathBuf {
        let file = dir.join("unity-fixture");
        let script = format!("#!/bin/sh\nprintf '%s' '{}'\n", response.replace('\'', "'\\''"));
        assert!(std::fs::write(&file, script).is_ok());
        assert!(std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).is_ok());
        file
    }

    #[test]
    fn status_requires_real_typed_auth_and_licence_fields() {
        let auth = serde_json::json!({"data":{"loggedIn":true,"user":{"email":"person@example.invalid"}}});
        assert_eq!(status(&auth, &serde_json::json!({"data":{"active":true}})), UnityStatus { signed_in: true, licensed: true });
        assert!(!status(&auth, &serde_json::json!({"data":{"message":"license active","active":"true"}})).licensed);
        assert!(status(&auth, &serde_json::json!({"data":{"active":false,"floatingServerHealthy":true}})).licensed);
    }

    #[test]
    fn child_status_never_exports_account_or_token_details() {
        let dir = tempfile::tempdir().unwrap();
        let cli = fixture(dir.path(), r#"{"success":true,"data":{"loggedIn":true,"active":true,"token":"sk-test","user":"person@example.invalid"}}"#);
        let auth = execute(&cli, dir.path(), &["auth", "status"], None, Duration::from_secs(3)).unwrap();
        let safe = serde_json::to_string(&status(&auth, &auth)).unwrap();
        assert!(!safe.contains("sk-test") && !safe.contains("person@"));
    }

    #[test]
    fn cancellation_stops_the_official_process_without_echoing_output() {
        let dir = tempfile::tempdir().unwrap();
        let cli = fixture(dir.path(), "not run");
        let cancel: Cancel = std::sync::Arc::new(|| true);
        assert_eq!(execute(&cli, dir.path(), &["auth", "login"], Some(&cancel), Duration::from_secs(3)).unwrap_err().code, ErrorCode::Cancelled);
    }
    #[test]
    fn first_use_protocol_keeps_login_installation_and_licence_states_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let cli = dir.path().join("unity-fixture");
        let script = r#"#!/bin/sh
if test "$1" = "env"; then
  printf '{"success":true,"data":{"userDataPath":"%s/private"}}\n' "$HOME"
  exit 0
fi
case "$1 $2" in
  "auth login") touch "$HOME/signed-in"; echo '{"success":true}';;
  "auth status")
    if test -f "$HOME/signed-in"; then echo '{"success":true,"data":{"loggedIn":true}}'
    else echo '{"success":false,"data":{"loggedIn":false}}'; exit 3; fi;;
  "license activate")
    test -f "$HOME/signed-in" || exit 3
    test "$3" = "--personal" && test "$4" = "--accept-eula" || exit 2
    touch "$HOME/licensed"; echo '{"success":true}';;
  "license status")
    if test -f "$HOME/licensed"; then echo '{"success":true,"data":{"active":true}}'
    else echo '{"success":false,"data":{"active":false}}'; exit 4; fi;;
esac
"#;
        std::fs::write(&cli, script).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(setup_with_cli(&cli, dir.path(), UnityAction::Check, None).unwrap(), UnityStatus::default());
        assert!(setup_with_cli(&cli, dir.path(), UnityAction::ActivatePersonal, None).is_err());
        assert!(!dir.path().join("licensed").exists());
        assert_eq!(setup_with_cli(&cli, dir.path(), UnityAction::Login, None).unwrap(), UnityStatus { signed_in: true, licensed: false });
        assert_eq!(setup_with_cli(&cli, dir.path(), UnityAction::ActivatePersonal, None).unwrap(), UnityStatus { signed_in: true, licensed: true });
    }

    #[test]
    fn unisolated_vendor_configuration_is_refused_before_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let cli = fixture(dir.path(), r#"{"success":true,"data":{"userDataPath":"/outside/private-home"}}"#);
        assert_eq!(setup_with_cli(&cli, dir.path(), UnityAction::Login, None).unwrap_err().code, ErrorCode::NotSupported);
    }
}
