//! Isolated release-update probe. Only operates inside a folder containing .omca-update-fixture.
#[cfg(target_os = "macos")]
use craft_anything_platform::process::Process;
use craft_anything_platform::update::{self, Installation};
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::time::Duration;
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let Some(result) = update::finish(&args) {
        return result;
    }
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let root = if cfg!(target_os = "macos") { me.ancestors().nth(3) } else { me.parent() }.ok_or("Missing fixture folder.")?;
    let marker = if cfg!(target_os = "macos") { "Contents/Resources/.omca-update-fixture" } else { ".omca-update-fixture" };
    if !root.join(marker).is_file() {
        return Err("This probe runs only in an isolated update fixture.".into());
    }
    #[cfg(target_os = "macos")]
    let version = Process::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CraftAnythingVersion"])
        .arg(root.join("Contents/Info.plist"))
        .timeout(Duration::from_secs(5))
        .run_checked()
        .map_err(|e| e.to_string())?
        .stdout_text()
        .trim()
        .to_owned();
    #[cfg(not(target_os = "macos"))]
    let version = std::fs::read_to_string(root.join("fixture-version.txt")).map_err(|e| e.to_string())?.trim().to_owned();
    if args.first().is_some_and(|a| a == "--version") {
        println!("craft-anything {version}");
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "--prepare") {
        if args.len() != 5 {
            return Err("Usage: update_fixture --prepare MANIFEST SIGNATURE ARTIFACT CACHE".into());
        }
        let read = |i| std::fs::read(PathBuf::from(args.get(i).ok_or("Missing fixture argument.")?)).map_err(|e| e.to_string());
        let manifest = read(1)?;
        let offered: serde_json::Value = serde_json::from_slice(&manifest).map_err(|e| e.to_string())?;
        let offered = offered.get("version").and_then(|v| v.as_str()).ok_or("Missing fixture release version.")?.to_owned();
        let restart = update::prepare(
            Installation::current()?,
            manifest,
            read(2)?,
            &PathBuf::from(args.get(3).ok_or("Missing artifact.")?),
            env!("CARGO_PKG_VERSION"),
            &offered,
            &PathBuf::from(args.get(4).ok_or("Missing cache.")?),
        )?;
        return restart.start();
    }
    std::fs::write(root.parent().ok_or("Missing fixture parent.")?.join("restarted-version.txt"), version).map_err(|e| e.to_string())
}
