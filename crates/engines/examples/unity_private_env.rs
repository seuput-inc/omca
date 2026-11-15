//! Explicit release gate only. Downloads the verified official Unity CLI, then asks only for
//! its environment paths. Does not read account status, log in, activate a licence or accept terms.
use craft_anything_engines::game;
use std::path::PathBuf;

fn main() -> Result<(), String> {
    let store = std::env::args_os().nth(1).map(PathBuf::from).ok_or("Pass a new absolute scratch engine-store directory.")?;
    if !store.is_absolute() || store.exists() {
        return Err("The probe requires a new absolute directory, not a person's existing engine store.".into());
    }
    game::unity_private_environment_probe(&store).map_err(|error| error.for_people())?;
    println!("Official Unity CLI uses the private account folder. No account or licence operation ran.");
    Ok(())
}
