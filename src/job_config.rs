use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

const PROJECT_QUALIFIER: &str = "io";
const PROJECT_ORGANIZATION: &str = "rclone";
const PROJECT_APPLICATION: &str = "sync-helper";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobConfig {
    pub name: String,
    pub local_path: String,
    pub remote: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rclone_config_path: Option<String>,
    /// Local/remote pairs to sync. If empty, a single bisync is run using `local_path` <-> `remote`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pairs: Vec<SyncPair>,
    /// Deprecated: older configs used `directories` instead of `pairs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directories: Vec<String>,
    /// Lock file to prevent concurrent runs. If not set, defaults to `/tmp/rclone-sync.lock`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock_file: Option<String>,
    /// Log directory for per-run log files. If not set, defaults to `$HOME/logs/rclone-sync`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_dir: Option<String>,
    /// Attempt a second run with `--resync` when bisync indicates recovery is required.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub auto_resync: bool,
    /// Remove stale bisync `.lck` files under `$HOME/.cache/rclone/bisync` before starting.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub clean_bisync_locks: bool,
    /// Run rclone under low CPU/IO priority if `nice` and `ionice` exist.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub use_nice_ionice: bool,
}

impl JobConfig {
    pub fn empty(job: &str) -> Self {
        Self {
            name: job.to_string(),
            local_path: String::new(),
            remote: String::new(),
            extra_args: vec![],
            rclone_config_path: None,
            pairs: vec![],
            directories: vec![],
            lock_file: None,
            log_dir: None,
            auto_resync: true,
            clean_bisync_locks: true,
            use_nice_ionice: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPair {
    pub local: String,
    pub remote: String,
}

fn default_true() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

pub fn config_dir() -> Result<PathBuf> {
    let project_dirs =
        ProjectDirs::from(PROJECT_QUALIFIER, PROJECT_ORGANIZATION, PROJECT_APPLICATION)
            .context("Unable to resolve config directory")?;
    Ok(project_dirs.config_dir().to_path_buf())
}

pub fn jobs_dir() -> Result<PathBuf> {
    let dir = config_dir()?.join("jobs");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn job_config_path(job: &str) -> Result<PathBuf> {
    Ok(jobs_dir()?.join(format!("{job}.toml")))
}

pub fn load_or_create_job(job: &str) -> Result<JobConfig> {
    let path = job_config_path(job)?;
    if path.exists() {
        let mut cfg = load_job_from_path(&path)?;
        if cfg.name.trim().is_empty() {
            cfg.name = job.to_string();
        }
        // Migration: if no explicit pairs are configured, interpret each directory as a pair suffix.
        if cfg.pairs.is_empty() && cfg.directories.iter().any(|d| !d.trim().is_empty()) {
            cfg.pairs = cfg
                .directories
                .iter()
                .map(|d| d.trim())
                .filter(|d| !d.is_empty())
                .map(|d| SyncPair {
                    local: d.to_string(),
                    remote: d.to_string(),
                })
                .collect();
        }
        return Ok(cfg);
    }

    let cfg = JobConfig::empty(job);
    save_job(&cfg)?;
    Ok(cfg)
}

pub fn save_job(cfg: &JobConfig) -> Result<()> {
    let path = job_config_path(&cfg.name)?;
    let content = toml::to_string_pretty(cfg)?;
    fs::write(&path, content)?;
    Ok(())
}

fn load_job_from_path(path: &Path) -> Result<JobConfig> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read job config {}", path.display()))?;
    let cfg: JobConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse job config {}", path.display()))?;
    Ok(cfg)
}
