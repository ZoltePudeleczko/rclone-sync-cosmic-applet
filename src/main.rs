mod applet;
mod cli;
mod job_config;
mod notify;
mod open;
mod runner;
mod status;
mod systemd;

use clap::Parser;
use std::ffi::OsString;

use cli::{Cli, Commands, SystemdCommands};

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let _ = tracing_log::LogTracer::init();

    // COSMIC panel may invoke applets with extra/unknown arguments; fall back to UI mode.
    let args: Vec<OsString> = std::env::args_os().collect();
    let cli = Cli::try_parse_from(&args).unwrap_or(Cli { command: None });

    match cli.command.unwrap_or(Commands::Ui) {
        Commands::Ui => cosmic::applet::run::<applet::AppletModel>(()),
        Commands::Run { job } => {
            if let Err(err) = run_once(&job) {
                eprintln!("{err}");
                std::process::exit(1);
            }
            Ok(())
        }
        Commands::Systemd { command } => {
            if let Err(err) = handle_systemd(command) {
                eprintln!("{err}");
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

fn run_once(job: &str) -> anyhow::Result<()> {
    let cfg = job_config::load_or_create_job(job)?;

    let mut store = status::StatusStore::load(job)?;
    let result = store.run_sync(&cfg)?;
    let state = store.state();

    // Notifications for non-interactive runs (errors always; successes only if there were changes).
    if result.exit_code != 0 {
        let body = state
            .last_error
            .clone()
            .unwrap_or_else(|| format!("Job {job} failed (exit {})", result.exit_code));
        let _ = notify::notify("Rclone Sync Failed", &body, true);
        anyhow::bail!("Job {} failed (exit {})", job, result.exit_code);
    } else if let Some(changed) = state.last_changed_count {
        if changed > 0 {
            let body = format!("Job {job}: synced {changed} item(s)");
            let _ = notify::notify("Rclone Sync Completed", &body, false);
        }
    }
    Ok(())
}

fn handle_systemd(cmd: SystemdCommands) -> anyhow::Result<()> {
    let sd = systemd::SystemdUser::new()?;
    match cmd {
        SystemdCommands::Install { job } => sd.install_units(&job)?,
        SystemdCommands::Enable { job } => sd.enable_timer(&job)?,
        SystemdCommands::Disable { job } => sd.disable_timer(&job)?,
        SystemdCommands::Status { job } => {
            let st = sd.status(&job)?;
            println!(
                "{} enabled={} active={} next={:?}",
                st.unit, st.enabled, st.active, st.next_elapse
            );
        }
    }
    Ok(())
}
