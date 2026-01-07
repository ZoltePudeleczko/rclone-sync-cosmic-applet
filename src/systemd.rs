use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};

#[derive(Debug, Clone)]
pub struct TimerStatus {
    pub unit: String,
    pub installed: bool,
    pub enabled: bool,
    pub active: bool,
    pub next_elapse: Option<String>,
}

pub struct SystemdUser {
    systemd_user_dir: PathBuf,
}

impl SystemdUser {
    pub fn new() -> Result<Self> {
        // systemd user unit files live under XDG_CONFIG_HOME/systemd/user
        let systemd_user_dir = systemd_user_dir()?;
        fs::create_dir_all(&systemd_user_dir)?;
        Ok(Self { systemd_user_dir })
    }

    pub fn install_units(&self, job: &str) -> Result<()> {
        let service_name = service_unit_name(job);
        let timer_name = timer_unit_name(job);

        let service_path = self.systemd_user_dir.join(&service_name);
        let timer_path = self.systemd_user_dir.join(&timer_name);

        let exe = std::env::current_exe().context("Failed to find current executable path")?;

        let service = format!(
            r#"[Unit]
Description=Rclone bisync job ({job})

[Service]
Type=oneshot
ExecStart={exe} run --job {job}
"#,
            job = job,
            exe = exe.display()
        );

        let timer = format!(
            r#"[Unit]
Description=Run rclone bisync job ({job}) hourly

[Timer]
OnCalendar=hourly
Persistent=true
Unit={service_name}

[Install]
WantedBy=timers.target
"#,
            job = job,
            service_name = service_name
        );

        fs::write(&service_path, service)
            .with_context(|| format!("Failed to write {}", service_path.display()))?;
        fs::write(&timer_path, timer)
            .with_context(|| format!("Failed to write {}", timer_path.display()))?;

        self.daemon_reload()?;
        Ok(())
    }

    pub fn enable_timer(&self, job: &str) -> Result<()> {
        systemctl_user(&["enable", "--now", &timer_unit_name(job)])?;
        Ok(())
    }

    pub fn disable_timer(&self, job: &str) -> Result<()> {
        systemctl_user(&["disable", "--now", &timer_unit_name(job)])?;
        Ok(())
    }

    pub fn status(&self, job: &str) -> Result<TimerStatus> {
        let unit = timer_unit_name(job);
        let service = service_unit_name(job);
        let installed = self.systemd_user_dir.join(&unit).exists()
            && self.systemd_user_dir.join(&service).exists();
        let enabled = is_enabled(&unit)?;
        let active = is_active(&unit)?;
        // For calendar timers, `list-timers` is the most reliable user-facing representation.
        let next_from_list = systemctl_list_timer_next(&unit).ok().flatten();
        let next_elapse = systemctl_show_property(&unit, "NextElapseUSecRealtime")?;

        Ok(TimerStatus {
            unit,
            installed,
            enabled,
            active,
            next_elapse: next_from_list.or_else(|| parse_next_elapse(next_elapse)),
        })
    }

    fn daemon_reload(&self) -> Result<()> {
        systemctl_user(&["daemon-reload"])?;
        Ok(())
    }
}

fn timer_unit_name(job: &str) -> String {
    format!("rclonesync-helper@{job}.timer")
}

fn service_unit_name(job: &str) -> String {
    format!("rclonesync-helper@{job}.service")
}

fn systemctl_user(args: &[&str]) -> Result<String> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .with_context(|| format!("Failed to run systemctl --user {}", args.join(" ")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        anyhow::bail!(
            "systemctl --user {} failed (code {:?}): {}{}",
            args.join(" "),
            output.status.code(),
            stdout,
            stderr
        );
    }
    Ok(stdout)
}

fn is_enabled(unit: &str) -> Result<bool> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(["is-enabled", unit])
        .output()?;
    Ok(output.status.success())
}

fn is_active(unit: &str) -> Result<bool> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(["is-active", unit])
        .output()?;
    Ok(output.status.success())
}

fn systemctl_show_property(unit: &str, prop: &str) -> Result<Option<String>> {
    let out = systemctl_user(&["show", unit, "-p", prop])?;
    // Format: Key=value
    let value = out
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{prop}=")))
        .map(|s| s.to_string());
    Ok(value)
}

fn systemctl_list_timer_next(unit: &str) -> Result<Option<String>> {
    // Output columns are separated by multiple spaces; we parse the first "NEXT" column.
    // Example:
    // Tue 2026-01-06 14:05:00 CET  55min left  Tue 2026-01-06 13:10:00 CET  ...  <unit>
    let out = systemctl_user(&["list-timers", "--all", "--no-pager", "--no-legend", unit])?;
    let line = out.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return Ok(None);
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.is_empty() {
        return Ok(None);
    }
    if tokens[0].eq_ignore_ascii_case("n/a") || tokens[0] == "-" {
        return Ok(None);
    }
    if tokens.len() < 3 {
        return Ok(None);
    }

    let mut next = vec![tokens[0], tokens[1], tokens[2]];
    // Include timezone if it looks like one (simple heuristic).
    if tokens.len() >= 4 {
        let tz = tokens[3];
        let tz_like = tz.len() <= 6
            && tz
                .chars()
                .all(|c| c.is_ascii_alphabetic() || c == '_' || c == '/');
        if tz_like {
            next.push(tz);
        }
    }
    Ok(Some(next.join(" ")))
}

fn parse_next_elapse(value: Option<String>) -> Option<String> {
    let s = value?;
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("n/a") || s == "0" {
        return None;
    }

    // Depending on systemd/version, this can be:
    // - a human readable timestamp (e.g. "Mon 2026-01-05 15:00:00 UTC")
    // - a raw usec timestamp (e.g. "1736089200000000")
    if s.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(us) = s.parse::<u64>() {
            let secs = (us / 1_000_000) as i64;
            let nsec = ((us % 1_000_000) * 1_000) as u32;
            if let Some(ts) = DateTime::<Utc>::from_timestamp(secs, nsec) {
                let local = ts.with_timezone(&Local);
                return Some(local.format("%F %T").to_string());
            }
        }
    }

    Some(s.to_string())
}

fn systemd_user_dir() -> Result<PathBuf> {
    if let Some(xdg_config) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg_config).join("systemd/user"));
    }
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/systemd/user"))
}
