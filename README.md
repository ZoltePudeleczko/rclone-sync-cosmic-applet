# Rclone Sync Applet
[![Rust](https://github.com/ZoltePudeleczko/rclone-sync-cosmic-applet/actions/workflows/rust.yml/badge.svg)](https://github.com/ZoltePudeleczko/rclone-sync-cosmic-applet/actions/workflows/rust.yml)

COSMIC panel applet for monitoring and triggering **rclone bisync** jobs, plus managing an optional `systemd --user` timer per job.

## Requirements

- COSMIC desktop (Pop!_OS 24.04 / COSMIC).
- `rclone` installed and available in `PATH`.

## Building & running

```bash
cargo build --release
```

## COSMIC applet (easy local install on Pop!_OS 24.04)

This project is a **COSMIC panel applet** (not a standalone GTK tray app).

To install it for your user account and test it in the panel:

```bash
./scripts/install-cosmic-applet-local.sh
```

Then open **COSMIC Settings → Panel** and add **Rclone Sync Helper**.

## Configuration (job files)

The app creates per-job configuration files under an XDG config directory (via `directories::ProjectDirs`) at:

- `$XDG_CONFIG_HOME/io/rclone/sync-helper/jobs/<job>.toml`

These job files **must not contain secrets**. Store your rclone credentials in rclone’s own config (default: `~/.config/rclone/rclone.conf`).

## System timer (systemd --user)

The app can create/manage a per-job `systemd --user` timer and service:

- `rclonesync-helper@<job>.service` runs `rclone_sync_helper run --job <job>`
- `rclonesync-helper@<job>.timer` triggers it on an interval

From the UI, use **Install units**, then **Enable**, and optionally **Apply interval**.

## Usage notes

- The helper stores cached sync state under `$XDG_STATE_HOME` (usually `~/.local/state`) in the app’s project directory.
- When you click “Sync now”, it runs `rclone bisync` for the configured job and records the timestamp/logs whether it succeeds or fails.
