use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "rclone_sync_helper")]
#[command(
    about = "COSMIC panel applet for running rclone bisync jobs + managing a systemd --user timer"
)]
#[command(version)]
#[command(propagate_version = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Launch the applet UI (default)
    Ui,

    /// Run a configured job once (used by the systemd timer)
    Run {
        /// Job name (config: $XDG_CONFIG_HOME/io/rclone/sync-helper/jobs/<job>.toml)
        #[arg(long, default_value = "default")]
        job: String,
    },

    /// Manage the per-job systemd --user timer/service
    Systemd {
        #[command(subcommand)]
        command: SystemdCommands,
    },
}

#[derive(Debug, Subcommand)]
pub enum SystemdCommands {
    /// Create/update the unit files for a job (does not enable automatically)
    /// Timer runs hourly on the hour (10:00, 11:00, 12:00, etc.)
    Install {
        #[arg(long, default_value = "default")]
        job: String,
    },

    Enable {
        #[arg(long, default_value = "default")]
        job: String,
    },

    Disable {
        #[arg(long, default_value = "default")]
        job: String,
    },

    Status {
        #[arg(long, default_value = "default")]
        job: String,
    },
}
