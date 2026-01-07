use crate::job_config;
use crate::status::{StatusStore, SyncState};
use crate::systemd::{SystemdUser, TimerStatus};

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use cosmic::iced::widget::container;
use cosmic::iced::widget::text::Wrapping;
use cosmic::iced::widget::tooltip;
use cosmic::iced::{Length, Limits, Subscription, window::Id};
use cosmic::iced_winit::commands::popup::{destroy_popup, get_popup};
use cosmic::prelude::*;
use cosmic::widget;
use cosmic::widget::settings;
use cosmic::widget::text as ctext;

#[derive(Default)]
pub struct AppletModel {
    core: cosmic::Core,
    popup: Option<Id>,
    job: String,
    state: SyncState,
    systemd_status: Option<TimerStatus>,
    systemd_error: Option<String>,
    syncing: bool,
    manual_syncing: bool,
    sync_started_at: Option<chrono::DateTime<chrono::Utc>>,
    sync_log_tail: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    PopupClosed(Id),
    ShowLogs,
    Refresh,
    SyncNow,
    SyncFinished(Result<SyncState, String>),
    SyncLogTick,
    SystemdInstall,
    SystemdEnable,
    SystemdDisable,
    OpenConfigFile,
}

impl cosmic::Application for AppletModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "io.rclone.sync-helper";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(
        core: cosmic::Core,
        _flags: Self::Flags,
    ) -> (Self, Task<cosmic::Action<Self::Message>>) {
        let job = "default".to_string();
        let state = StatusStore::load(&job)
            .map(|s| s.state())
            .unwrap_or_default();

        let mut app = AppletModel {
            core,
            popup: None,
            job,
            state,
            systemd_status: None,
            systemd_error: None,
            syncing: false,
            manual_syncing: false,
            sync_started_at: None,
            sync_log_tail: Vec::new(),
        };
        app.refresh_systemd_summary();
        app.refresh_syncing_summary();
        (app, Task::none())
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let icon = if self.syncing {
            "content-loading-symbolic"
        } else if self.state.last_error.is_some() {
            "dialog-error-symbolic"
        } else if self.state.last_success.is_some() {
            "emblem-ok-symbolic"
        } else {
            "view-refresh-symbolic"
        };
        self.core
            .applet
            .icon_button(icon)
            .on_press(Message::TogglePopup)
            .into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Self::Message> {
        let state = &self.state;

        let (status_label, status_color) = if self.syncing {
            ("Syncing", cosmic::iced::Color::from_rgb(0.95, 0.75, 0.2))
        } else if state.last_error.is_some() {
            ("Error", cosmic::iced::Color::from_rgb(0.85, 0.25, 0.25))
        } else if state.last_run.is_some() {
            ("OK", cosmic::iced::Color::from_rgb(0.2, 0.7, 0.3))
        } else {
            ("Idle", cosmic::iced::Color::from_rgb(0.55, 0.55, 0.55))
        };

        let status_dot = widget::container(ctext::caption("●"))
            .class(cosmic::theme::Container::custom(move |_theme| {
                container::Style {
                    text_color: Some(status_color),
                    ..Default::default()
                }
            }))
            .width(Length::Shrink);

        let status_badge = widget::row()
            .spacing(6)
            .push(status_dot)
            .push(ctext::caption(status_label).wrapping(Wrapping::Word));

        let refresh_button =
            widget::button::icon(cosmic::widget::icon::from_name("view-refresh-symbolic"))
                .on_press(Message::Refresh);

        let sync_now_button = widget::button::suggested(if self.syncing {
            "Syncing…"
        } else {
            "Sync now"
        })
        .on_press_maybe((!self.syncing).then_some(Message::SyncNow));

        let header = widget::column()
            .spacing(2)
            .push(
                widget::row()
                    .spacing(10)
                    .push(ctext::title4("Rclone Sync Helper"))
                    .push(
                        widget::container(status_badge)
                            .width(Length::Shrink)
                            .padding([2, 8]),
                    )
                    .push(
                        widget::container(widget::Space::with_width(Length::Fill))
                            .width(Length::Fill),
                    )
                    .push(refresh_button),
            )
            .push(ctext::caption(format!("Job: {}", self.job)))
            .push(sync_now_button);

        let (status_section, logs_section): (Element<'_, Message>, Option<Element<'_, Message>>) =
            if self.syncing {
                let started = format_datetime(&self.sync_started_at);
                let elapsed = self
                    .sync_started_at
                    .map(|t| (Utc::now() - t).num_seconds().max(0) as u64)
                    .unwrap_or(0);
                let logs = if self.sync_log_tail.is_empty() {
                    "Starting…".to_string()
                } else {
                    self.sync_log_tail.join("\n")
                };
                let logs_widget = widget::scrollable::scrollable(
                    widget::container(ctext::monotext(logs).size(12).wrapping(Wrapping::Word))
                        .padding(8)
                        .width(Length::Fill),
                )
                .height(Length::Fixed(200.0));

                let show_logs_button =
                    widget::button::standard("Show logs").on_press(Message::ShowLogs);

                let status = settings::section()
                    .title("Status")
                    .add(settings::item(
                        "Sync started",
                        ctext::body(started).wrapping(Wrapping::Word),
                    ))
                    .add(settings::item(
                        "Elapsed",
                        ctext::body(format_duration(Duration::from_secs(elapsed)))
                            .wrapping(Wrapping::Word),
                    ));

                // Create a full-width logs section
                let logs_section = widget::column()
                    .spacing(8)
                    .width(Length::Fill)
                    .push(logs_widget.width(Length::Fill))
                    .push(show_logs_button);

                (status.into(), Some(logs_section.into()))
            } else if state.last_error.is_some() {
                let last_run = format_datetime(&state.last_run);
                let last_err = state
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "Unknown error".into());
                let duration = state
                    .last_duration_secs
                    .map(|s| format_duration(Duration::from_secs(s)));
                let show_logs_button =
                    widget::button::standard("Show logs").on_press(Message::ShowLogs);
                let status = settings::section()
                    .title("Status")
                    .add(settings::item(
                        "Last run",
                        ctext::body(last_run).wrapping(Wrapping::Word),
                    ))
                    .add_maybe(duration.map(|d| {
                        settings::item("Duration", ctext::body(d).wrapping(Wrapping::Word))
                    }))
                    .add(settings::item(
                        "Error",
                        ctext::body(last_err).wrapping(Wrapping::Word),
                    ))
                    .add(settings::item("", show_logs_button));
                (status.into(), None)
            } else if state.last_success.is_some() {
                let last_success_rel = format_relative_time(&state.last_success);
                let last_success_exact = format_datetime(&state.last_success);
                let duration = state
                    .last_duration_secs
                    .map(|s| format_duration(Duration::from_secs(s)));
                let last_success_text = ctext::body(last_success_rel).wrapping(Wrapping::Word);
                let last_success_widget = tooltip(
                    last_success_text,
                    ctext::body(last_success_exact),
                    tooltip::Position::FollowCursor,
                );
                let show_logs_button =
                    widget::button::standard("Show logs").on_press(Message::ShowLogs);
                let mut section = settings::section()
                    .title("Status")
                    .add(settings::item("Last successful", last_success_widget));
                if let Some(d) = duration {
                    section = section.add(settings::item(
                        "Duration",
                        ctext::body(d).wrapping(Wrapping::Word),
                    ));
                }
                // Only show Changes if we have a valid count
                if let Some(count) = state.last_changed_count {
                    section = section.add(settings::item(
                        "Changes",
                        ctext::body(count.to_string()).wrapping(Wrapping::Word),
                    ));
                }
                let section = section.add(settings::item("", show_logs_button));
                (section.into(), None)
            } else {
                let show_logs_button =
                    widget::button::standard("Show logs").on_press(Message::ShowLogs);
                let section = settings::section()
                    .title("Status")
                    .add(settings::item(
                        "State",
                        ctext::body("Idle").wrapping(Wrapping::Word),
                    ))
                    .add(settings::item("", show_logs_button));
                (section.into(), None)
            };

        let remote = state
            .remote_summary
            .clone()
            .unwrap_or_else(|| "Not yet detected".into());
        let details_section = settings::section()
            .title("Details")
            .add(settings::item(
                "Remote",
                ctext::body(remote).wrapping(Wrapping::Word),
            ))
            .add(settings::item(
                "Last run",
                ctext::body(format_datetime(&state.last_run)).wrapping(Wrapping::Word),
            ))
            .add_maybe(self.state.last_log_file.as_ref().map(|p| {
                settings::item(
                    "Log file",
                    ctext::caption(p.clone()).wrapping(Wrapping::Word),
                )
            }));

        let (active, next, sd_err) = match (&self.systemd_status, &self.systemd_error) {
            (Some(st), _) => (
                st.enabled.to_string(),
                st.next_elapse
                    .clone()
                    .unwrap_or_else(|| "Not scheduled".into()),
                None,
            ),
            (None, Some(err)) => ("unknown".into(), "Not scheduled".into(), Some(err.clone())),
            (None, None) => (
                "unknown".into(),
                "Not scheduled".into(),
                Some("not checked yet".into()),
            ),
        };

        let systemd_details = settings::section()
            .title("Systemd timer")
            .add(settings::item("Active", ctext::body(active)))
            .add(settings::item(
                "Next",
                ctext::body(next).wrapping(Wrapping::Word),
            ))
            .add_maybe(
                sd_err.map(|e| settings::item("Note", ctext::caption(e).wrapping(Wrapping::Word))),
            );

        let st = self.systemd_status.as_ref();
        let show_install = st.map(|s| !s.installed).unwrap_or(true);
        let show_enable = st.map(|s| s.installed && !s.enabled).unwrap_or(false);
        let show_disable = st.map(|s| s.enabled).unwrap_or(false);

        let systemd_actions =
            widget::row()
                .spacing(10)
                .push_maybe(show_install.then_some(
                    widget::button::suggested("Install").on_press(Message::SystemdInstall),
                ))
                .push_maybe(show_enable.then_some(
                    widget::button::suggested("Enable").on_press(Message::SystemdEnable),
                ))
                .push_maybe(show_disable.then_some(
                    widget::button::destructive("Disable").on_press(Message::SystemdDisable),
                ));

        let systemd_details = systemd_details.add(settings::item("Actions", systemd_actions));

        let show_details =
            state.last_error.is_some() || (!self.syncing && state.last_success.is_none());

        let mut sections: Vec<Element<'_, Message>> = vec![header.into(), status_section.into()];
        if let Some(logs) = logs_section {
            sections.push(logs);
        }
        if show_details {
            sections.push(details_section.into());
        }
        sections.push(systemd_details.into());

        let config_button =
            widget::button::standard("Open config").on_press(Message::OpenConfigFile);
        sections.push(config_button.into());

        let content = widget::scrollable::scrollable(settings::view_column(sections).padding(12))
            .height(Length::Shrink);

        self.core.applet.popup_container(content).into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        // Periodic refresh of cached state.
        let mut subs = vec![
            cosmic::iced::time::every(std::time::Duration::from_secs(30)).map(|_| Message::Refresh),
        ];

        if self.syncing {
            subs.push(
                cosmic::iced::time::every(std::time::Duration::from_secs(1))
                    .map(|_| Message::SyncLogTick),
            );
        }

        Subscription::batch(subs)
    }

    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::TogglePopup => {
                return if let Some(p) = self.popup.take() {
                    destroy_popup(p)
                } else {
                    let new_id = Id::unique();
                    self.popup.replace(new_id);
                    let mut popup_settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().unwrap(),
                        new_id,
                        None,
                        None,
                        None,
                    );
                    popup_settings.positioner.size_limits = Limits::NONE
                        .max_width(560.0)
                        .min_width(380.0)
                        .min_height(1.0)
                        .max_height(700.0);
                    get_popup(popup_settings)
                };
            }
            Message::PopupClosed(id) => {
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                }
            }
            Message::ShowLogs => {
                // Try to open the log file from state first
                let log_path = if let Some(log_file) = &self.state.last_log_file {
                    let path = expand_home(log_file);
                    if path.exists() {
                        Some(path)
                    } else {
                        find_latest_log_file(&self.job).ok()
                    }
                } else {
                    find_latest_log_file(&self.job).ok()
                };

                if let Some(path) = log_path {
                    let _ = crate::open::open_log_file(&path);
                }
            }
            Message::OpenConfigFile => {
                // Ensure the file exists, then open it in the user's default editor.
                let _ = job_config::load_or_create_job(&self.job);
                if let Ok(path) = job_config::job_config_path(&self.job) {
                    let _ = crate::open::open_in_cosmic_edit(&path);
                }
            }
            Message::Refresh => {
                if let Ok(store) = StatusStore::load(&self.job) {
                    self.state = store.state();
                }
                self.refresh_systemd_summary();
                self.refresh_syncing_summary();
            }
            Message::SyncNow => {
                if self.syncing {
                    return Task::none();
                }
                self.syncing = true;
                self.manual_syncing = true;
                self.sync_started_at = Some(Utc::now());
                self.sync_log_tail = tail_latest_sync_log_lines(&self.job).unwrap_or_default();
                let job = self.job.clone();
                return Task::perform(
                    async move {
                        // rclone can run for a long time; use a blocking thread to keep the UI responsive.
                        tokio::task::spawn_blocking(move || {
                            let cfg =
                                job_config::load_or_create_job(&job).map_err(|e| format!("{e}"))?;
                            let mut store = StatusStore::load(&job).map_err(|e| format!("{e}"))?;
                            if let Err(err) = store.run_sync(&cfg) {
                                store.set_last_error_and_persist(format!("Sync run failed: {err}"));
                            }
                            Ok::<SyncState, String>(store.state())
                        })
                        .await
                        .map_err(|e| format!("Sync task failed: {e}"))?
                    },
                    |res| cosmic::action::app(Message::SyncFinished(res)),
                );
            }
            Message::SyncFinished(res) => {
                self.manual_syncing = false;
                match res {
                    Ok(state) => self.state = state,
                    Err(err) => self.state.last_error = Some(err),
                }
                // Notifications: errors -> critical; success with changes -> normal; no changes -> silent.
                if let Some(code) = self.state.last_exit_code {
                    if code != 0 {
                        let body = self
                            .state
                            .last_error
                            .clone()
                            .unwrap_or_else(|| "Sync failed".into());
                        let _ = crate::notify::notify("Rclone Sync Failed", &body, true);
                    } else if let Some(changed) = self.state.last_changed_count {
                        if changed > 0 {
                            let body = format!("Synced {changed} item(s)");
                            let _ = crate::notify::notify("Rclone Sync Completed", &body, false);
                        }
                    }
                }
                self.refresh_systemd_summary();
                self.refresh_syncing_summary();
            }
            Message::SyncLogTick => {
                if self.syncing {
                    self.sync_log_tail = tail_latest_sync_log_lines(&self.job).unwrap_or_default();
                }
            }
            Message::SystemdInstall => {
                let _ = SystemdUser::new().and_then(|sd| sd.install_units(&self.job));
                self.refresh_systemd_summary();
            }
            Message::SystemdEnable => {
                let _ = SystemdUser::new().and_then(|sd| sd.enable_timer(&self.job));
                self.refresh_systemd_summary();
            }
            Message::SystemdDisable => {
                let _ = SystemdUser::new().and_then(|sd| sd.disable_timer(&self.job));
                self.refresh_systemd_summary();
            }
        }

        Task::none()
    }

    fn style(&self) -> Option<cosmic::iced_runtime::Appearance> {
        Some(cosmic::applet::style())
    }
}

impl AppletModel {
    fn refresh_systemd_summary(&mut self) {
        match SystemdUser::new().and_then(|sd| sd.status(&self.job)) {
            Ok(st) => {
                self.systemd_status = Some(st);
                self.systemd_error = None;
            }
            Err(err) => {
                self.systemd_status = None;
                self.systemd_error = Some(err.to_string());
            }
        }
    }

    // Config editing moved out of the applet UI: we open the config file in the user's editor.

    fn refresh_syncing_summary(&mut self) {
        // If a manual sync is in flight, keep the syncing UI active regardless of lock timing.
        if self.manual_syncing {
            self.syncing = true;
            if self.sync_started_at.is_none() {
                self.sync_started_at = Some(Utc::now());
            }
            return;
        }

        let cfg = match job_config::load_or_create_job(&self.job) {
            Ok(c) => c,
            Err(_) => {
                self.syncing = false;
                self.sync_started_at = None;
                self.sync_log_tail.clear();
                return;
            }
        };

        let lock_path = cfg
            .lock_file
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("/tmp/rclone-sync.lock");

        if let Some(info) = crate::runner::detect_running(lock_path) {
            self.syncing = true;
            self.sync_started_at = info.started_at.or_else(|| Some(Utc::now()));
            self.sync_log_tail = tail_latest_sync_log_lines(&self.job).unwrap_or_default();
        } else {
            self.syncing = false;
            self.sync_started_at = None;
            self.sync_log_tail.clear();
        }
    }
}

// Pair parsing/formatting moved to the config file (opened in user's editor).

fn format_datetime(value: &Option<chrono::DateTime<chrono::Utc>>) -> String {
    value
        .map(|dt| {
            let local = dt.with_timezone(&chrono::Local);
            local.format("%F %T").to_string()
        })
        .unwrap_or_else(|| "Never".into())
}

fn format_relative_time(value: &Option<chrono::DateTime<chrono::Utc>>) -> String {
    value
        .map(|dt| {
            let now = Utc::now();
            let diff = now - dt;

            let total_seconds = diff.num_seconds();
            if total_seconds < 60 {
                "Just now".to_string()
            } else if total_seconds < 3600 {
                let minutes = total_seconds / 60;
                format!(
                    "{} minute{} ago",
                    minutes,
                    if minutes == 1 { "" } else { "s" }
                )
            } else if total_seconds < 86400 {
                let hours = total_seconds / 3600;
                format!("{} hour{} ago", hours, if hours == 1 { "" } else { "s" })
            } else {
                let days = total_seconds / 86400;
                format!("{} day{} ago", days, if days == 1 { "" } else { "s" })
            }
        })
        .unwrap_or_else(|| "Never".into())
}

fn format_duration(d: Duration) -> String {
    let total = d.as_secs();
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;

    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn find_latest_log_file(job: &str) -> anyhow::Result<PathBuf> {
    let cfg = job_config::load_or_create_job(job)?;
    let dir: PathBuf = if let Some(dir) = cfg.log_dir.as_deref().filter(|s| !s.trim().is_empty()) {
        expand_home(dir)
    } else {
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        PathBuf::from(home).join("logs/rclone-sync")
    };

    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    let entries =
        fs::read_dir(&dir).with_context(|| format!("Failed to read {}", dir.display()))?;
    for ent in entries {
        let ent = match ent {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = ent.path();
        let name_ok = path
            .file_name()
            .and_then(|os| os.to_str())
            .map(|s| s.starts_with("sync_") && s.ends_with(".log"))
            .unwrap_or(false);
        if !name_ok {
            continue;
        }
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if newest.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            newest = Some((mtime, path));
        }
    }

    newest
        .map(|(_, path)| path)
        .ok_or_else(|| anyhow::anyhow!("No log files found"))
}

fn tail_latest_sync_log_lines(job: &str) -> anyhow::Result<Vec<String>> {
    let cfg = job_config::load_or_create_job(job)?;
    let dir: PathBuf = if let Some(dir) = cfg.log_dir.as_deref().filter(|s| !s.trim().is_empty()) {
        expand_home(dir)
    } else {
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
        PathBuf::from(home).join("logs/rclone-sync")
    };

    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    let entries =
        fs::read_dir(&dir).with_context(|| format!("Failed to read {}", dir.display()))?;
    for ent in entries {
        let ent = match ent {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = ent.path();
        let name_ok = path
            .file_name()
            .and_then(|os| os.to_str())
            .map(|s| s.starts_with("sync_") && s.ends_with(".log"))
            .unwrap_or(false);
        if !name_ok {
            continue;
        }
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if newest.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            newest = Some((mtime, path));
        }
    }

    let Some((_, path)) = newest else {
        return Ok(Vec::new());
    };

    Ok(read_full_log_file(&path)?)
}

fn read_full_log_file(path: &PathBuf) -> anyhow::Result<Vec<String>> {
    let mut f = fs::File::open(path)?;
    let mut buf = String::new();
    f.read_to_string(&mut buf)?;

    let mut out = Vec::new();
    for line in buf.lines() {
        out.push(line.to_string());
    }
    Ok(out)
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
