use anyhow::{Context, Result};
fn main() -> Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .context("inspect-profile <profile.sii>")?;
    let text = ets_launcher::maps::profile::decode(&std::fs::read(path)?)?;
    for line in text.lines().filter(|line| {
        let field = line.trim_start();
        [
            "version:",
            "active_mods:",
            "map_path:",
            "cached_experience:",
            "cached_distance:",
        ]
        .iter()
        .any(|prefix| field.starts_with(prefix))
    }) {
        println!("{line}");
    }
    Ok(())
}
