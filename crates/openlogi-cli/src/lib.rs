//! OpenLogi CLI implementation. The `openlogi` binary is a thin wrapper that
//! calls [`run`]; the command tree and argument parsing live here.

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::{EnvFilter, fmt};

mod cmd;

/// OpenLogi: a local-first companion for Logitech HID++ peripherals.
#[derive(Debug, Parser)]
#[command(
    name = "openlogi",
    version,
    about = "OpenLogi: a local-first companion for Logitech HID++ peripherals.",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<cmd::Command>,
}

/// Initialise logging, parse arguments, and dispatch the chosen subcommand.
///
/// Returns the exit status the process should terminate with — `list` uses a
/// distinct one to report that no hardware is connected.
pub async fn run() -> Result<ExitCode> {
    fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_env("OPENLOGI_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let command = cli
        .cmd
        .unwrap_or(cmd::Command::List(cmd::list::ListArgs {}));
    command.run().await
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;
    use cmd::Command;
    use cmd::backlight::BacklightAction;
    use cmd::diag::DiagCmd;
    use cmd::diag::lighting::Method;
    use cmd::diag::wheel::ResolutionArg;

    /// Clap's own structural validation (arg ID collisions, invalid
    /// `conflicts_with` targets, etc.) — cheap and catches a broken derive
    /// tree before it ever reaches a user.
    #[test]
    fn cli_command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    /// A bare `openlogi` invocation must remain valid — `run()` defaults the
    /// missing subcommand to `list`.
    #[test]
    fn bare_invocation_has_no_subcommand() {
        let cli = Cli::try_parse_from(["openlogi"]).expect("bare invocation parses");
        assert!(cli.cmd.is_none());
    }

    /// A bare `openlogi backlight` must stay valid — `run` treats a missing
    /// action as `status`, so it can never write to the device by accident.
    #[test]
    fn backlight_defaults_to_status_and_accepts_a_device_filter() {
        let cli = Cli::try_parse_from(["openlogi", "backlight", "--device", "MX KEYS S"])
            .expect("bare backlight invocation parses");

        match cli.cmd.expect("subcommand present") {
            Command::Backlight(args) => {
                assert_eq!(args.device.as_deref(), Some("MX KEYS S"));
                assert!(args.action.is_none());
            }
            other => panic!("expected Backlight, got {other:?}"),
        }
    }

    #[test]
    fn backlight_off_is_parsed_as_its_own_action() {
        let cli =
            Cli::try_parse_from(["openlogi", "backlight", "off"]).expect("backlight off parses");

        match cli.cmd.expect("subcommand present") {
            Command::Backlight(args) => {
                assert!(matches!(args.action, Some(BacklightAction::Off)));
            }
            other => panic!("expected Backlight, got {other:?}"),
        }
    }

    #[test]
    fn backlight_rejects_an_unknown_action() {
        let result = Cli::try_parse_from(["openlogi", "backlight", "dim"]);
        result.expect_err("an unknown backlight action must be rejected");
    }

    #[test]
    fn smartshift_leave_flipped_conflicts_with_sensitivity() {
        let result = Cli::try_parse_from([
            "openlogi",
            "diag",
            "smartshift",
            "--leave-flipped",
            "--sensitivity",
            "10",
        ]);
        result.expect_err("--leave-flipped and --sensitivity must conflict");
    }

    #[test]
    fn smartshift_rejects_zero_sensitivity() {
        // `--sensitivity` is a `NonZeroU8`; 0 must fail to parse rather than
        // silently becoming "no change" downstream.
        let result = Cli::try_parse_from(["openlogi", "diag", "smartshift", "--sensitivity", "0"]);
        result.expect_err("a zero --sensitivity must fail to parse");
    }

    #[test]
    fn dpi_target_and_device_flags_are_mapped() {
        let cli = Cli::try_parse_from([
            "openlogi",
            "diag",
            "dpi",
            "--target",
            "800",
            "--device",
            "MX Master",
        ])
        .expect("valid dpi invocation parses");

        match cli.cmd.expect("subcommand present") {
            Command::Diag(DiagCmd::Dpi(args)) => {
                assert_eq!(args.target, Some(800));
                assert_eq!(args.device.as_deref(), Some("MX Master"));
            }
            other => panic!("expected Diag(Dpi), got {other:?}"),
        }
    }

    #[test]
    fn profiles_read_only_and_device_flags_are_mapped() {
        let cli = Cli::try_parse_from([
            "openlogi",
            "diag",
            "profiles",
            "--read-only",
            "--device",
            "G502",
        ])
        .expect("valid profiles invocation parses");

        match cli.cmd.expect("subcommand present") {
            Command::Diag(DiagCmd::Profiles(args)) => {
                assert!(args.read_only);
                assert!(!args.leave_onboard);
                assert_eq!(args.device.as_deref(), Some("G502"));
            }
            other => panic!("expected Diag(Profiles), got {other:?}"),
        }
    }

    #[test]
    fn profiles_read_only_conflicts_with_leave_onboard() {
        Cli::try_parse_from([
            "openlogi",
            "diag",
            "profiles",
            "--read-only",
            "--leave-onboard",
        ])
        .expect_err("read-only and leave-onboard must conflict");
    }

    #[test]
    fn lighting_color_is_positional_and_method_is_a_flag() {
        let cli = Cli::try_parse_from([
            "openlogi", "diag", "lighting", "ff0000", "--method", "effects",
        ])
        .expect("valid lighting invocation parses");

        match cli.cmd.expect("subcommand present") {
            Command::Diag(DiagCmd::Lighting(args)) => {
                assert_eq!(args.color, "ff0000");
                assert!(matches!(args.method, Method::Effects));
            }
            other => panic!("expected Diag(Lighting), got {other:?}"),
        }
    }

    #[test]
    fn lighting_rejects_unknown_method() {
        let result = Cli::try_parse_from([
            "openlogi", "diag", "lighting", "ff0000", "--method", "bogus",
        ]);
        result.expect_err("an unknown lighting method must be rejected");
    }

    #[test]
    fn wheel_resolution_and_device_flags_are_mapped() {
        let cli = Cli::try_parse_from([
            "openlogi",
            "diag",
            "wheel",
            "--device",
            "MX Anywhere 3S",
            "--resolution",
            "low",
        ])
        .expect("valid wheel invocation parses");

        match cli.cmd.expect("subcommand present") {
            Command::Diag(DiagCmd::Wheel(args)) => {
                assert_eq!(args.device.as_deref(), Some("MX Anywhere 3S"));
                assert_eq!(args.resolution, Some(ResolutionArg::Low));
            }
            other => panic!("expected Diag(Wheel), got {other:?}"),
        }
    }
}
