//! `openlogi diag power-off` — read or set a headset's HID++ 0x1F20
//! automatic power-off timeout.

use std::fmt;

use anyhow::{Context, Result, bail};
use clap::Args;
use openlogi_hid::PowerOffTimeout;

use crate::cmd::diag::{exclude_agent, select_device};

#[derive(Debug, Args)]
pub struct PowerOffArgs {
    /// Timeout to write, in idle minutes (0 is never, 90 the longest). The
    /// headset stores it; nothing is written to config.toml.
    #[arg(value_parser = parse_minutes)]
    pub minutes: Option<PowerOffTimeout>,

    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: PowerOffArgs) -> Result<()> {
    // The agent writes the timeout too; held to the end, so none starts
    // between this read and the write.
    let _agent_lock = exclude_agent().await?;
    let (route, name) = select_device(args.device.as_deref(), &[0x1f20]).await?;
    println!("device: {name} ({route})");

    let before = openlogi_hid::get_power_off_timeout(&route)
        .await
        .context("read power-off timeout")?;
    println!("  current power-off: {}", Minutes(before));

    let Some(requested) = args.minutes else {
        return Ok(());
    };
    let applied = openlogi_hid::set_power_off_timeout(&route, requested)
        .await
        .context("set power-off timeout")?;
    println!("  confirmed: {}", Minutes(applied));
    if applied != requested {
        bail!(
            "power-off write not applied: requested {}, device confirms {}",
            Minutes(requested),
            Minutes(applied)
        );
    }
    println!("✓ power-off set to {}", Minutes(applied));
    Ok(())
}

/// Parses the minutes argument, leaving the range to [`PowerOffTimeout`].
fn parse_minutes(raw: &str) -> Result<PowerOffTimeout, String> {
    let minutes: u8 = raw.parse().map_err(|err| format!("{err}"))?;
    PowerOffTimeout::try_new(minutes).map_err(|err| err.to_string())
}

/// A timeout as the user reads it: `0` is "never", not "0 min".
struct Minutes(PowerOffTimeout);

impl fmt::Display for Minutes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == PowerOffTimeout::NEVER {
            f.write_str("never")
        } else {
            write!(f, "{} min", self.0.minutes())
        }
    }
}

#[cfg(test)]
mod tests {
    use openlogi_hid::PowerOffTimeout;

    use super::{Minutes, parse_minutes};

    #[test]
    fn parses_the_whole_range_the_headset_accepts() {
        assert_eq!(parse_minutes("0"), Ok(PowerOffTimeout::NEVER));
        assert_eq!(parse_minutes("90"), Ok(PowerOffTimeout::MAX));
    }

    #[test]
    fn rejects_a_timeout_the_headset_would_not_take_before_writing() {
        assert!(parse_minutes("91").is_err(), "91 is above the range");
        assert!(parse_minutes("256").is_err(), "256 does not fit a byte");
        assert!(parse_minutes("-1").is_err(), "minutes are not negative");
    }

    // Unix socket paths honor XDG_CONFIG_HOME; Windows uses a fixed pipe name
    // and cannot isolate this endpoint test from a real agent that way.
    #[cfg(unix)]
    #[test]
    fn a_running_agent_keeps_the_diagnostic_off_the_headset() {
        use openlogi_core::single_instance::{self, InstanceError, Role};

        use super::{PowerOffArgs, run};

        const TEST: &str =
            "cmd::diag::power_off::tests::a_running_agent_keeps_the_diagnostic_off_the_headset";
        const MODE: &str = "OPENLOGI_DIAG_POWER_OFF_OWNERSHIP_TEST";
        if std::env::var(MODE).as_deref() != Ok("exercise") {
            // Run in a child with its own config home, so neither the lock
            // nor the endpoint is a real agent's.
            let home = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env(MODE, "exercise")
                .env("XDG_CONFIG_HOME", home.path())
                .env_remove("XDG_RUNTIME_DIR")
                .env(
                    openlogi_core::env::PROFILE,
                    openlogi_core::paths::Profile::Production.env_value(),
                )
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        // Only reads: should the guard be missing, nothing is written.
        let args = || PowerOffArgs {
            minutes: None,
            device: None,
        };
        openlogi_core::worker::runtime().unwrap().block_on(async {
            // An agent starting up holds its lock before its socket binds.
            {
                let _agent = single_instance::acquire(Role::Agent).unwrap();
                let error = run(args())
                    .await
                    .expect_err("an agent holding its lock must exclude the diagnostic");
                assert!(
                    matches!(
                        error.downcast_ref::<InstanceError>(),
                        Some(InstanceError::AlreadyRunning { .. })
                    ),
                    "refused for another reason: {error:#}"
                );
            }
            // An endpoint that accepts connections is an agent, too.
            let _listener = openlogi_ipc::transport::bind().unwrap();
            let error = run(args())
                .await
                .expect_err("an answering endpoint must exclude the diagnostic");
            assert!(
                error.to_string().contains("agent is running"),
                "refused for another reason: {error:#}"
            );
        });
    }

    #[test]
    fn zero_reads_as_never_rather_than_zero_minutes() {
        assert_eq!(Minutes(PowerOffTimeout::NEVER).to_string(), "never");
        assert_eq!(Minutes(PowerOffTimeout::clamped(15)).to_string(), "15 min");
    }
}
