use std::collections::VecDeque;
use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::job_config::JobConfig;
use crate::runner::{RunResult, run_job};

const STATE_FILE_NAME: &str = "status.json";
const PROJECT_QUALIFIER: &str = "io";
const PROJECT_ORGANIZATION: &str = "rclone";
const PROJECT_APPLICATION: &str = "sync-helper";
const MAX_LOG_LINES: usize = 6;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct SyncState {
    pub job: String,
    pub last_run: Option<DateTime<Utc>>,
    pub last_success: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub log_preview: Vec<String>,
    pub remote_summary: Option<String>,
    pub last_exit_code: Option<i32>,
    #[serde(default)]
    pub last_log_file: Option<String>,
    #[serde(default)]
    pub last_changed_count: Option<u32>,
    #[serde(default)]
    pub last_duration_secs: Option<u64>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            job: "default".into(),
            last_run: None,
            last_success: None,
            last_error: None,
            log_preview: Vec::new(),
            remote_summary: None,
            last_exit_code: None,
            last_log_file: None,
            last_changed_count: None,
            last_duration_secs: None,
        }
    }
}

#[derive(Debug)]
pub struct ScriptResult {
    pub timestamp: DateTime<Utc>,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub log_file: Option<String>,
    pub duration_secs: Option<u64>,
}

impl ScriptResult {
    fn preview_lines(&self) -> Vec<String> {
        let mut queue = VecDeque::with_capacity(MAX_LOG_LINES);

        for line in self.stdout.lines().chain(self.stderr.lines()) {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if queue.len() == MAX_LOG_LINES {
                queue.pop_front();
            }
            queue.push_back(trimmed.to_string());
        }

        queue.into()
    }

    fn error_summary(&self) -> Option<String> {
        let stderr = self.stderr.trim();
        if !stderr.is_empty() {
            return stderr.lines().last().map(|line| line.trim().to_string());
        }
        if self.exit_code != 0 {
            return Some(format!("Exited with code {}", self.exit_code));
        }
        None
    }
}

pub struct StatusStore {
    state_path: PathBuf,
    state: SyncState,
}

impl StatusStore {
    pub fn load(job: &str) -> Result<Self> {
        let path = state_file_path(job)?;
        let state = if path.exists() {
            let content = fs::read_to_string(&path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            let mut st = SyncState::default();
            st.job = job.to_string();
            st
        };

        Ok(Self {
            state_path: path,
            state,
        })
    }

    pub fn state(&self) -> SyncState {
        self.state.clone()
    }

    pub fn persist(&self) -> Result<()> {
        let serialized = serde_json::to_string_pretty(&self.state)?;
        fs::write(&self.state_path, serialized)?;
        Ok(())
    }

    pub fn run_sync(&mut self, job_cfg: &JobConfig) -> Result<ScriptResult> {
        let result = run_job_and_capture(job_cfg)?;
        self.state.update_from_result(&result);
        self.persist()?;
        Ok(result)
    }

    pub fn set_last_error_and_persist(&mut self, message: String) {
        self.state.last_error = Some(message);
        let _ = self.persist();
    }
}

impl SyncState {
    fn update_from_result(&mut self, result: &ScriptResult) {
        self.last_run = Some(result.timestamp);
        self.last_exit_code = Some(result.exit_code);
        self.log_preview = result.preview_lines();
        self.remote_summary = detect_remote_summary(result);
        self.last_log_file = result.log_file.clone();
        self.last_changed_count = detect_changed_count(result);
        self.last_duration_secs = result.duration_secs;

        if result.exit_code == 0 {
            self.last_success = Some(result.timestamp);
            self.last_error = None;
        } else {
            self.last_error = result.error_summary();
        }
    }
}

fn state_file_path(job: &str) -> Result<PathBuf> {
    let dir = if let Some(project_dirs) =
        ProjectDirs::from(PROJECT_QUALIFIER, PROJECT_ORGANIZATION, PROJECT_APPLICATION)
    {
        if let Some(state_dir) = project_dirs.state_dir() {
            state_dir.to_path_buf()
        } else {
            let home = env::var_os("HOME").context("HOME environment variable is not set")?;
            let mut fallback = PathBuf::from(home);
            fallback.push(".local/state");
            fallback.push(PROJECT_APPLICATION);
            fallback
        }
    } else {
        let home = env::var_os("HOME").context("HOME environment variable is not set")?;
        let mut fallback = PathBuf::from(home);
        fallback.push(".local/state");
        fallback.push(PROJECT_APPLICATION);
        fallback
    };

    fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{}-{}", job, STATE_FILE_NAME)))
}

fn run_job_and_capture(job_cfg: &JobConfig) -> Result<ScriptResult> {
    let result: RunResult = run_job(job_cfg)?;
    Ok(ScriptResult {
        timestamp: result.timestamp,
        exit_code: result.exit_code,
        stdout: result.stdout,
        stderr: result.stderr,
        log_file: result.log_file,
        duration_secs: result.duration_secs,
    })
}

fn detect_remote_summary(result: &ScriptResult) -> Option<String> {
    for line in result.stdout.lines().chain(result.stderr.lines()) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let candidate = trimmed.to_lowercase();
        if candidate.contains("remote")
            || candidate.contains("drive")
            || candidate.contains("gdrive")
            || candidate.contains("sync")
        {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn detect_changed_count(result: &ScriptResult) -> Option<u32> {
    // Extract numeric counters from rclone bisync output.
    let combined = format!("{}\n{}", result.stdout, result.stderr);

    // Prefer per-pair "Path1/Path2: N changes:" counters. These appear once per bisync run and
    // work well with multi-pair jobs (we can sum across pairs).
    let (changes_total, saw_changes_lines) = sum_path_changes(&combined);
    if saw_changes_lines {
        return Some(changes_total);
    }

    // Fallback: Look for "Transferred:" and "Copied:" labels with completion counts.
    // This is less reliable for multi-pair runs and also doesn't count deletions, but it helps
    // for logs where change counters aren't present.
    let transferred = extract_last_number_after_label(&combined, "Transferred:");
    let copied = extract_last_number_after_label(&combined, "Copied:");

    // Sum them if both are present, otherwise return whichever is found.
    match (transferred, copied) {
        (Some(t), Some(c)) => Some(t.saturating_add(c)),
        (Some(t), None) => Some(t),
        (None, Some(c)) => Some(c),
        (None, None) => None,
    }
}

fn sum_path_changes(text: &str) -> (u32, bool) {
    let mut total: u32 = 0;
    let mut saw_any = false;

    for line in text.lines() {
        // Example:
        // 2026/01/11 22:25:26 INFO  : Path1:   40 changes:    4 new,   36 newer,    0 older,    0 deleted
        for label in ["Path1:", "Path2:"] {
            let Some(pos) = line.find(label) else { continue };
            let after = line[pos + label.len()..].trim_start();
            let mut parts = after.split_whitespace();
            let Some(num_str) = parts.next() else { continue };
            let Some(changes_str) = parts.next() else { continue };
            if !changes_str.starts_with("changes") {
                continue;
            }
            if let Ok(n) = num_str.parse::<u32>() {
                total = total.saturating_add(n);
                saw_any = true;
            }
        }
    }

    (total, saw_any)
}

fn extract_last_number_after_label(text: &str, label: &str) -> Option<u32> {
    let mut last_100_percent: Option<u32> = None;
    let mut last_any_percent: Option<u32> = None;

    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(label) {
            let rest = rest.trim();
            // Look for "X / Y" format with percentage
            if rest.contains(" / ") {
                if let Some(slash_pos) = rest.find(" / ") {
                    let after_slash = &rest[slash_pos + 3..];
                    if let Some(comma_pos) = after_slash.find(',') {
                        let num_str = after_slash[..comma_pos].trim();
                        if let Ok(num) = num_str.parse::<u32>() {
                            // Prefer lines with "100%" (rclone bisync completion format)
                            // e.g., "Transferred:          262 / 262, 100%" -> extract 262
                            if rest.contains("100%") {
                                last_100_percent = Some(num);
                            } else {
                                // Track any percentage line as fallback
                                // e.g., "Transferred: 52 / 262, 20%" -> extract 262
                                last_any_percent = Some(num);
                            }
                        }
                    }
                }
            }
        }
    }

    // Return 100% line if found, otherwise any percentage line, otherwise None
    last_100_percent.or(last_any_percent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_result(exit_code: i32, stdout: &str, stderr: &str) -> ScriptResult {
        ScriptResult {
            timestamp: Utc.with_ymd_and_hms(2024, 1, 5, 12, 0, 0).unwrap(),
            exit_code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            log_file: None,
            duration_secs: Some(123),
        }
    }

    #[test]
    fn preview_lines_limits_log_and_ignores_empty() {
        let result = sample_result(0, "line1\n\nline2\nline3\nline4\nline5\nline6\nline7", "");
        let preview = result.preview_lines();
        assert_eq!(preview.len(), MAX_LOG_LINES);
        assert_eq!(preview.first().unwrap(), "line2");
        assert_eq!(preview.last().unwrap(), "line7");
    }

    #[test]
    fn error_summary_prefers_stderr() {
        let result = sample_result(1, "stdout", "first err\nmore");
        assert_eq!(result.error_summary().as_deref(), Some("more"));
    }

    #[test]
    fn detect_remote_summary_matches_drive_keywords() {
        let result = sample_result(0, "Syncing local -> Remote Drive", "");
        assert_eq!(
            detect_remote_summary(&result).as_deref(),
            Some("Syncing local -> Remote Drive")
        );
    }

    #[test]
    fn sync_state_updates_success_and_error() {
        let mut state = SyncState::default();
        let success_result = sample_result(0, "remote state reached", "");
        state.update_from_result(&success_result);
        assert!(state.last_success.is_some());
        assert!(state.last_error.is_none());

        let fail_result = sample_result(2, "attempt", "failed");
        state.update_from_result(&fail_result);
        assert_eq!(state.last_error.as_deref(), Some("failed"));
        assert_eq!(state.last_exit_code, Some(2));
    }

    #[test]
    fn detect_changed_count_extracts_from_rclone_bisync_format() {
        // Test with rclone bisync output format - should extract total from 100% line
        let stderr = r#"2026/01/07 01:34:44 NOTICE: 
Transferred:   	   72.278 MiB / 73.224 MiB, 99%, 2.528 MiB/s, ETA 0s
Checks:                29 / 29, 100%
Transferred:           52 / 262, 20%
Elapsed time:       4m0.6s
2026/01/07 01:35:44 NOTICE: 
Transferred:           195 / 262, 74%
Elapsed time:       5m0.6s
2026/01/07 01:36:44 NOTICE: 
Transferred:          262 / 262, 100%
Elapsed time:       6m0.6s"#;
        let result = sample_result(0, "", stderr);
        let count = detect_changed_count(&result);
        assert_eq!(
            count,
            Some(262),
            "Should extract 262 from the 100% completion line"
        );
    }

    #[test]
    fn detect_changed_count_sums_changes_across_multiple_pairs() {
        // Multi-pair combined stderr: should sum per-pair "PathN: X changes:" lines.
        let stderr = r#"2026/01/11 22:25:26 INFO  : Path1:   40 changes:    4 new,   36 newer,    0 older,    0 deleted
2026/01/11 22:28:49 INFO  : Path1:    5 changes:    0 new,    4 newer,    0 older,    1 deleted
2026/01/11 22:33:08 INFO  : No changes found
2026/01/11 22:33:08 NOTICE: 
Transferred:   	          0 B / 0 B, -, 0 B/s, ETA -"#;
        let result = sample_result(0, "", stderr);
        let count = detect_changed_count(&result);
        assert_eq!(count, Some(45), "Should sum 40 + 5 across pairs");
    }

    #[test]
    fn extract_last_number_prefers_100_percent_line() {
        let text = r#"Transferred:           52 / 262, 20%
Transferred:          195 / 262, 74%
Transferred:          262 / 262, 100%"#;
        let result = extract_last_number_after_label(text, "Transferred:");
        assert_eq!(result, Some(262), "Should extract from the 100% line");
    }

    #[test]
    fn extract_last_number_handles_multiple_100_percent_lines() {
        // If there are multiple 100% lines, take the last one
        let text = r#"Transferred:          100 / 100, 100%
Transferred:          262 / 262, 100%"#;
        let result = extract_last_number_after_label(text, "Transferred:");
        assert_eq!(result, Some(262), "Should extract from the last 100% line");
    }

    #[test]
    fn extract_last_number_falls_back_to_any_percentage() {
        // If no 100% line exists, use any percentage line
        let text = r#"Transferred:           52 / 262, 20%"#;
        let result = extract_last_number_after_label(text, "Transferred:");
        assert_eq!(
            result,
            Some(262),
            "Should extract from any percentage line if no 100% exists"
        );
    }

    #[test]
    fn detect_changed_count_with_real_rclone_bisync_output() {
        // Test with actual rclone bisync output from user's logs
        let stderr = r#"2026/01/07 01:30:43 NOTICE: 
Transferred:   	          0 B / 0 B, -, 0 B/s, ETA -
Elapsed time:         7.2s

2026/01/07 01:34:44 NOTICE: 
Transferred:   	   72.278 MiB / 73.224 MiB, 99%, 2.528 MiB/s, ETA 0s
Checks:                29 / 29, 100%
Transferred:           52 / 262, 20%
Elapsed time:       4m0.6s

2026/01/07 01:35:44 NOTICE: 
Transferred:   	   72.887 MiB / 73.224 MiB, 100%, 262.263 KiB/s, ETA 1s
Checks:                29 / 29, 100%
Transferred:          195 / 262, 74%
Elapsed time:       5m0.6s

2026/01/07 01:36:44 NOTICE: 
Transferred:   	   73.224 MiB / 73.224 MiB, 100%, 262.263 KiB/s, ETA 0s
Checks:                30 / 30, 100%
Deleted:                1 (files), 0 (dirs)
Transferred:          262 / 262, 100%
Elapsed time:       6m0.6s

2026/01/07 01:37:44 NOTICE: 
Transferred:   	   73.224 MiB / 73.224 MiB, 100%, 262.263 KiB/s, ETA 0s
Checks:                30 / 30, 100%
Deleted:                1 (files), 0 (dirs)
Transferred:          262 / 262, 100%
Elapsed time:       7m0.6s

2026/01/07 01:38:44 NOTICE: 
Transferred:   	   73.224 MiB / 73.224 MiB, 100%, 262.263 KiB/s, ETA 0s
Checks:                30 / 30, 100%
Deleted:                1 (files), 0 (dirs)
Transferred:          262 / 262, 100%
Elapsed time:       8m0.6s

2026/01/07 01:39:44 NOTICE: 
Transferred:   	   73.224 MiB / 73.224 MiB, 100%, 262.263 KiB/s, ETA 0s
Checks:                30 / 30, 100%
Deleted:                1 (files), 0 (dirs)
Transferred:          262 / 262, 100%
Elapsed time:       9m0.6s

2026/01/07 01:40:44 NOTICE: 
Transferred:   	   73.224 MiB / 73.224 MiB, 100%, 262.263 KiB/s, ETA 0s
Checks:                30 / 30, 100%
Deleted:                1 (files), 0 (dirs)
Transferred:          262 / 262, 100%
Elapsed time:      10m0.6s

2026/01/07 01:41:05 INFO  : Validating listings for Path1 "/home/szymon/Google Drive/ES-DE/" vs Path2 "gdrive{qOqdu}:ES-DE/"
2026/01/07 01:41:05 INFO  : Bisync successful
2026/01/07 01:41:05 NOTICE: 
Transferred:   	   73.224 MiB / 73.224 MiB, 100%, 262.263 KiB/s, ETA 0s
Checks:                30 / 30, 100%
Deleted:                1 (files), 0 (dirs)
Transferred:          262 / 262, 100%
Elapsed time:     10m22.0s"#;
        let result = sample_result(0, "", stderr);
        let count = detect_changed_count(&result);
        assert_eq!(
            count,
            Some(262),
            "Should extract 262 from the last 100% completion line in real rclone bisync output"
        );
    }
}
