use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(author, version, about)]
pub struct CliArgs {
    #[arg(short, long)]
    pub gpu_id: Option<String>,
    #[command(subcommand)]
    pub subcommand: CliCommand,
}

#[derive(Subcommand)]
pub enum CliCommand {
    /// List GPUs
    #[clap(alias = "list-gpus")]
    List,
    /// Show GPU info
    Info,
    /// Show GPU stats
    Stats,
    /// Generate debug snapshot
    Snapshot,
    /// Manage GPU power limit
    PowerLimit {
        #[command(subcommand)]
        cmd: Option<PowerLimitCmd>,
    },
    /// Manage profiles
    Profile(ProfileArgs),
    /// Detach the GPU from LACT (temporarily ignore it)
    Detach,
    /// Reattach a previously detached GPU
    Reattach,
}

#[derive(Parser, Clone)]
pub enum PowerLimitCmd {
    /// Get current power limit and allowed range
    Get,
    /// Set power limit
    Set { limit: u32 },
    /// Manage automatic power limit throttling
    Throttle {
        #[command(subcommand)]
        cmd: Option<PowerThrottleCmd>,
    },
}

#[derive(Parser, Clone)]
pub enum PowerThrottleCmd {
    /// Show power limit throttling settings and state
    Get,
    /// Enable and configure power limit throttling
    Set(PowerThrottleSetArgs),
    /// Disable power limit throttling
    Off,
}

#[derive(Parser, Clone, Default)]
pub struct PowerThrottleSetArgs {
    /// Lowest power limit in watts that the throttling can set.
    /// Defaults to the lowest power limit of the GPU.
    #[arg(long)]
    pub min_power: Option<f64>,
    /// Maximum temperature of a sensor in degrees, in the `sensor=degrees` form.
    /// Can be used multiple times, e.g. `--threshold junction=92 --threshold mem=95`.
    /// Existing thresholds are kept unless overridden.
    #[arg(long, value_name = "SENSOR=DEGREES")]
    pub threshold: Vec<String>,
    /// Power limit step in watts, used both when lowering and raising the limit
    #[arg(long)]
    pub step: Option<f64>,
    /// Interval between temperature checks in seconds
    #[arg(long = "interval", value_name = "SECONDS")]
    pub interval_secs: Option<u64>,
    /// Number of degrees a temperature has to fall below its threshold
    /// before the power limit is raised back
    #[arg(long)]
    pub hysteresis: Option<u8>,
    /// Power usage at or above this percentage of the current power limit
    /// counts as a heavy load, which blocks raising the power limit
    #[arg(long)]
    pub load_lock_pct: Option<u8>,
    /// Time in seconds that raising the power limit is
    /// blocked after a heavy load was seen
    #[arg(long = "load-lock-duration", value_name = "SECONDS")]
    pub load_lock_duration_secs: Option<u64>,
    /// Removes the load lock, allowing the power limit to be raised at any time
    #[arg(long)]
    pub disable_load_lock: bool,
}

#[derive(Parser)]
pub struct ProfileArgs {
    #[command(subcommand)]
    pub subcommand: Option<ProfileCommand>,
}

#[derive(Subcommand)]
pub enum ProfileCommand {
    /// List profiles
    List,
    /// Current profile
    Get,
    /// Set profile
    Set(SetProfileArgs),
    /// Manage profile auto switching
    AutoSwitch(ProfileAutoSwitchArgs),
}

#[derive(Parser)]
pub struct SetProfileArgs {
    pub name: String,
}

#[derive(Parser)]
pub struct ProfileAutoSwitchArgs {
    #[command(subcommand)]
    pub subcommand: Option<ProfileAutoSwitchCommand>,
}

#[derive(Subcommand)]
pub enum ProfileAutoSwitchCommand {
    /// Current auto switch state
    Get,
    /// Enable auto switching
    Enable,
    /// Disable auto switching
    Disable,
}

#[cfg(test)]
mod tests {
    use super::{CliArgs, CliCommand, PowerLimitCmd, PowerThrottleCmd};
    use clap::Parser;

    #[test]
    fn parse_power_throttle_set() {
        let args = CliArgs::parse_from([
            "lact",
            "power-limit",
            "throttle",
            "set",
            "--min-power",
            "261",
            "--threshold",
            "junction=92",
            "--threshold",
            "mem=95",
            "--load-lock-pct",
            "90",
        ]);

        let CliCommand::PowerLimit { cmd: Some(cmd) } = args.subcommand else {
            panic!("Expected a power limit command");
        };
        let PowerLimitCmd::Throttle {
            cmd: Some(PowerThrottleCmd::Set(settings)),
        } = cmd
        else {
            panic!("Expected a power limit throttling command");
        };

        assert_eq!(settings.min_power, Some(261.0));
        assert_eq!(settings.threshold, ["junction=92", "mem=95"]);
        assert_eq!(settings.load_lock_pct, Some(90));
        assert_eq!(settings.step, None);
        assert!(!settings.disable_load_lock);
    }

    #[test]
    fn parse_power_throttle_off() {
        let args = CliArgs::parse_from(["lact", "--gpu-id", "0", "power-limit", "throttle", "off"]);

        let CliCommand::PowerLimit { cmd: Some(cmd) } = args.subcommand else {
            panic!("Expected a power limit command");
        };
        assert!(matches!(
            cmd,
            PowerLimitCmd::Throttle {
                cmd: Some(PowerThrottleCmd::Off)
            }
        ));
    }
}
