use std::path::Path;
use std::process::Command;

use anyhow::Result;

pub fn open_in_cosmic_edit(path: &Path) -> Result<()> {
    // Prefer the CLI if available.
    if Command::new("cosmic-edit").arg(path).spawn().is_ok() {
        return Ok(());
    }

    // Fall back to launching the desktop entry.
    if Command::new("gtk-launch")
        .arg("com.system76.CosmicEdit")
        .arg(path)
        .spawn()
        .is_ok()
    {
        return Ok(());
    }

    anyhow::bail!(
        "Failed to launch COSMIC Edit (cosmic-edit / gtk-launch com.system76.CosmicEdit)"
    );
}

pub fn open_log_file(path: &Path) -> Result<()> {
    // Try cosmic-edit first
    if Command::new("cosmic-edit").arg(path).spawn().is_ok() {
        return Ok(());
    }

    // Fall back to xdg-open (opens with system default)
    if Command::new("xdg-open").arg(path).spawn().is_ok() {
        return Ok(());
    }

    anyhow::bail!("Failed to open log file (cosmic-edit / xdg-open)")
}
