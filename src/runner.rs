use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::job_config::{JobConfig, SyncPair};

#[derive(Debug)]
pub struct RunResult {
    pub timestamp: DateTime<Utc>,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub log_file: Option<String>,
    pub duration_secs: Option<u64>,
}

pub fn run_job(cfg: &JobConfig) -> Result<RunResult> {
    let timestamp = Utc::now();

    validate_config(cfg)?;

    if cfg.clean_bisync_locks {
        let _ = clean_bisync_locks();
    }

    let lock_path = cfg
        .lock_file
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("/tmp/rclone-sync.lock");
    let _lock_guard = match LockGuard::acquire(lock_path) {
        Ok(g) => Some(g),
        Err(LockError::AlreadyRunning(pid)) => {
            return Ok(RunResult {
                timestamp,
                exit_code: 0,
                stdout: String::new(),
                stderr: format!("Sync already running (PID: {pid}). Skipping this run."),
                log_file: None,
                duration_secs: None,
            });
        }
        Err(LockError::Other(err)) => return Err(err),
    };

    let (mut log_file, log_file_path) = create_log_file(cfg, timestamp)?;
    writeln!(log_file, "=== rclone bisync run started ===")?;
    writeln!(log_file, "job={}", cfg.name)?;
    writeln!(log_file, "local_base={}", cfg.local_path)?;
    writeln!(log_file, "remote_base={}", cfg.remote)?;
    writeln!(log_file, "timestamp={}", timestamp.to_rfc3339())?;
    if !cfg.pairs.is_empty() {
        writeln!(
            log_file,
            "pairs={:?}",
            cfg.pairs
                .iter()
                .map(|p| (&p.local, &p.remote))
                .collect::<Vec<_>>()
        )?;
    }

    let pairs: Vec<SyncPair> = if cfg.pairs.is_empty() {
        vec![SyncPair {
            local: cfg.local_path.clone(),
            remote: cfg.remote.clone(),
        }]
    } else {
        cfg.pairs.clone()
    };

    let mut combined_stdout = String::new();
    let mut combined_stderr = String::new();
    let mut final_exit = 0;

    for (idx, pair) in pairs.iter().enumerate() {
        let (local, remote) = resolve_pair_paths(cfg, pair);
        let label = format!("pair {}/{}: {} <-> {}", idx + 1, pairs.len(), local, remote);
        writeln!(log_file, "\n=== {label} ===")?;

        let attempt = |extra: &[&str]| -> Result<(i32, String, String)> {
            let mut cmd = build_command(cfg, &local, &remote, extra)?;
            let output = cmd.output().with_context(|| {
                format!(
                    "Failed to execute rclone bisync for job {} ({} <-> {})",
                    cfg.name, local, remote
                )
            })?;
            let code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Ok((code, stdout, stderr))
        };

        // First attempt
        let (mut exit_code, mut last_stdout, mut last_stderr) = attempt(&[])?;
        write_log_chunk(
            &mut log_file,
            "attempt=normal",
            exit_code,
            &last_stdout,
            &last_stderr,
        )?;
        let _ = log_file.flush();

        // Retry after lock cleanup (requested).
        if exit_code != 0 {
            if let Some(lock_path) = detect_prior_lock_file(&last_stdout, &last_stderr) {
                if remove_stale_lock_file(&lock_path).unwrap_or(false) {
                    let (c, out, err) = attempt(&[])?;
                    exit_code = c;
                    last_stdout = out;
                    last_stderr = err;
                    write_log_chunk(
                        &mut log_file,
                        "attempt=retry_after_lock_cleanup",
                        exit_code,
                        &last_stdout,
                        &last_stderr,
                    )?;
                }
            }
        }

        // Recovery: if bisync indicates a resync is required, optionally retry with --resync.
        if exit_code != 0 && needs_resync(&last_stdout, &last_stderr) {
            if cfg.auto_resync {
                let (c, out, err) = attempt(&["--resync"])?;
                exit_code = c;
                last_stdout = out;
                last_stderr = err;
                write_log_chunk(
                    &mut log_file,
                    "attempt=resync_recovery",
                    exit_code,
                    &last_stdout,
                    &last_stderr,
                )?;
                let _ = log_file.flush();
            } else {
                writeln!(
                    log_file,
                    "\n--- note ---\nResync required, but auto_resync=false; run with --resync to recover."
                )?;
                let _ = log_file.flush();
            }
        }

        if !combined_stdout.is_empty() && !last_stdout.is_empty() {
            combined_stdout.push('\n');
        }
        combined_stdout.push_str(&last_stdout);

        if !combined_stderr.is_empty() && !last_stderr.is_empty() {
            combined_stderr.push('\n');
        }
        combined_stderr.push_str(&last_stderr);

        if exit_code != 0 {
            final_exit = exit_code;
        }
    }

    writeln!(
        log_file,
        "=== rclone bisync run finished (exit={}) ===",
        final_exit
    )?;
    let duration_secs = (Utc::now() - timestamp).num_seconds().max(0) as u64;

    Ok(RunResult {
        timestamp,
        exit_code: final_exit,
        stdout: combined_stdout,
        stderr: combined_stderr,
        log_file: Some(log_file_path.display().to_string()),
        duration_secs: Some(duration_secs),
    })
}

fn validate_config(cfg: &JobConfig) -> Result<()> {
    let base_local_ok = !cfg.local_path.trim().is_empty();
    let base_remote_ok = !cfg.remote.trim().is_empty();

    if cfg.pairs.is_empty() {
        if base_local_ok && base_remote_ok {
            return Ok(());
        }
        let path = crate::job_config::job_config_path(&cfg.name)
            .ok()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<config file>".into());
        anyhow::bail!(
            "Job '{}' is not configured. Please set local_path and remote in {}",
            cfg.name,
            path
        );
    }

    if !base_local_ok {
        for p in &cfg.pairs {
            let local = p.local.trim();
            if local.is_empty() || !local.starts_with('/') {
                let path = crate::job_config::job_config_path(&cfg.name)
                    .ok()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<config file>".into());
                anyhow::bail!(
                    "Job '{}' is missing local_path and has a relative/empty pair.local. Update {}",
                    cfg.name,
                    path
                );
            }
        }
    }

    if !base_remote_ok {
        for p in &cfg.pairs {
            let remote = p.remote.trim();
            if remote.is_empty() || !remote.contains(':') {
                let path = crate::job_config::job_config_path(&cfg.name)
                    .ok()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<config file>".into());
                anyhow::bail!(
                    "Job '{}' is missing remote and has a non-absolute/empty pair.remote. Update {}",
                    cfg.name,
                    path
                );
            }
        }
    }

    Ok(())
}

fn build_command(
    cfg: &JobConfig,
    local: &str,
    remote: &str,
    extra_args: &[&str],
) -> Result<Command> {
    let mut args: Vec<String> = Vec::new();
    args.push("bisync".into());
    args.push(local.to_string());
    args.push(remote.to_string());

    if let Some(path) = cfg.rclone_config_path.as_deref() {
        let path = path.trim();
        if !path.is_empty() {
            args.push("--config".into());
            args.push(path.to_string());
        }
    }

    // Note: when using pairs, filtering isn't needed because each pair is a separate bisync root.

    // User-provided extra args (non-secret flags only).
    args.extend(cfg.extra_args.iter().cloned());
    args.extend(extra_args.iter().map(|s| s.to_string()));

    // Prefer running with low priority if possible.
    if cfg.use_nice_ionice && cmd_exists("nice") && cmd_exists("ionice") {
        let mut cmd = Command::new("nice");
        cmd.arg("-n").arg("19");
        cmd.arg("ionice").arg("-c").arg("3");
        cmd.arg("rclone");
        cmd.args(args);
        Ok(cmd)
    } else {
        let mut cmd = Command::new("rclone");
        cmd.args(args);
        Ok(cmd)
    }
}

fn resolve_pair_paths(cfg: &JobConfig, pair: &SyncPair) -> (String, String) {
    let local = pair.local.trim();
    let remote = pair.remote.trim();

    let resolved_local = if local.starts_with('/') {
        local.to_string()
    } else if local.is_empty() {
        cfg.local_path.clone()
    } else {
        format!("{}/{}", cfg.local_path.trim_end_matches('/'), local)
    };

    let resolved_remote = if remote.contains(':') {
        // Full remote like "gdrive:foo/bar"
        remote.to_string()
    } else if remote.is_empty() {
        cfg.remote.clone()
    } else {
        // Remote suffix appended to base remote (handles "gdrive:" and "gdrive:base/path")
        let base = cfg.remote.trim_end_matches('/');
        if base.ends_with(':') {
            format!("{base}{remote}")
        } else {
            format!("{base}/{remote}")
        }
    };

    (resolved_local, resolved_remote)
}

fn cmd_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn create_log_file(cfg: &JobConfig, timestamp: DateTime<Utc>) -> Result<(fs::File, PathBuf)> {
    let dir = if let Some(dir) = cfg.log_dir.as_deref().filter(|s| !s.trim().is_empty()) {
        expand_home(dir)
    } else {
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        PathBuf::from(home).join("logs/rclone-sync")
    };
    fs::create_dir_all(&dir)?;

    let name = format!("sync_{}.log", timestamp.format("%Y%m%d_%H%M%S"));
    let path = dir.join(name);
    let file =
        fs::File::create(&path).with_context(|| format!("Failed to create {}", path.display()))?;
    Ok((file, path))
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    if let Some(rest) = path.strip_prefix("$HOME/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn write_log_chunk(
    file: &mut fs::File,
    label: &str,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> Result<()> {
    writeln!(file, "\n--- {label} (exit={exit_code}) ---")?;
    if !stdout.trim().is_empty() {
        writeln!(file, "STDOUT:\n{stdout}")?;
    }
    if !stderr.trim().is_empty() {
        writeln!(file, "STDERR:\n{stderr}")?;
    }
    Ok(())
}

fn needs_resync(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}").to_lowercase();
    combined.contains("cannot find prior path1 or path2 listings")
        || combined.contains("must run --resync")
        || combined.contains("bisync aborted")
}

fn detect_prior_lock_file(stdout: &str, stderr: &str) -> Option<String> {
    for line in stdout.lines().chain(stderr.lines()) {
        if let Some(rest) = line.split("prior lock file found:").nth(1) {
            let candidate = rest
                .trim()
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            if !candidate.is_empty() {
                return Some(candidate);
            }
        }
    }
    None
}

fn remove_stale_lock_file(path: &str) -> Result<bool> {
    let path = expand_home(path);
    if !path.exists() {
        return Ok(false);
    }

    let pid = fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
        .and_then(|l| l.parse::<u32>().ok());

    // If it contains a PID and that process is not alive, it's stale.
    if let Some(pid) = pid {
        if !pid_alive(pid) {
            let _ = fs::remove_file(&path);
            return Ok(true);
        }
        return Ok(false);
    }

    // No PID: remove if older than 60 minutes and no bisync is running.
    if !is_bisync_running() && file_older_than(&path, Duration::from_secs(60 * 60)) {
        let _ = fs::remove_file(&path);
        return Ok(true);
    }

    Ok(false)
}

fn clean_bisync_locks() -> Result<()> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let dir = PathBuf::from(home).join(".cache/rclone/bisync");
    if !dir.is_dir() {
        return Ok(());
    }

    // If no bisync process is running, delete all `.lck`. Otherwise, only remove clearly stale locks.
    if !is_bisync_running() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("lck") {
                let _ = fs::remove_file(p);
            }
        }
        return Ok(());
    }

    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("lck") {
            continue;
        }
        let pid = fs::read_to_string(&p)
            .ok()
            .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
            .and_then(|l| l.parse::<u32>().ok());
        if let Some(pid) = pid {
            if !pid_alive(pid) {
                let _ = fs::remove_file(p);
            }
        } else if file_older_than(&p, Duration::from_secs(60 * 60)) {
            let _ = fs::remove_file(p);
        }
    }
    Ok(())
}

fn is_bisync_running() -> bool {
    Command::new("pgrep")
        .arg("-f")
        .arg("rclone.*bisync")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn file_older_than(path: &Path, age: Duration) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    let Ok(elapsed) = SystemTime::now().duration_since(modified) else {
        return false;
    };
    elapsed > age
}

#[derive(Debug)]
struct LockGuard {
    path: PathBuf,
}

#[derive(Debug)]
enum LockError {
    AlreadyRunning(u32),
    Other(anyhow::Error),
}

impl LockGuard {
    fn acquire(path: &str) -> std::result::Result<Self, LockError> {
        let path = expand_home(path);
        if let Ok(existing) = fs::read_to_string(&path) {
            if let Ok(pid) = existing.trim().parse::<u32>() {
                if pid_alive(pid) {
                    return Err(LockError::AlreadyRunning(pid));
                }
            }
            let _ = fs::remove_file(&path);
        }

        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let mut file = fs::File::create(&path).map_err(|e| LockError::Other(e.into()))?;
        let pid = std::process::id();
        writeln!(file, "{pid}").map_err(|e| LockError::Other(e.into()))?;
        Ok(Self { path })
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug, Clone)]
pub struct RunningInfo {
    pub started_at: Option<DateTime<Utc>>,
}

/// Detect whether a sync is currently in progress by consulting the job lock file.
///
/// Returns `None` when:
/// - the lock file doesn't exist
/// - the lock file PID is dead (and the stale lock is removed best-effort)
pub fn detect_running(lock_file: &str) -> Option<RunningInfo> {
    let path = expand_home(lock_file);
    let pid = fs::read_to_string(&path).ok()?.trim().parse::<u32>().ok()?;

    if !pid_alive(pid) {
        let _ = fs::remove_file(&path);
        return None;
    }

    let started_at = fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|st| {
            let secs = st.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
            chrono::DateTime::from_timestamp(secs as i64, 0)
        });

    Some(RunningInfo { started_at })
}
