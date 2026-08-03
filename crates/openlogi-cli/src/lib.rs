//! OpenLogi CLI implementation. The `openlogi` binary is a thin wrapper that
//! calls [`run`]; the command tree and argument parsing live here.

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
pub async fn run() -> Result<()> {
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
#[allow(clippy::expect_used, reason = "expect/unwrap are idiomatic in tests")]
mod tests {
    use clap::CommandFactory;

    use super::*;
    use cmd::Command;
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
        assert!(result.is_err());
    }

    #[test]
    fn smartshift_rejects_zero_sensitivity() {
        // `--sensitivity` is a `NonZeroU8`; 0 must fail to parse rather than
        // silently becoming "no change" downstream.
        let result = Cli::try_parse_from(["openlogi", "diag", "smartshift", "--sensitivity", "0"]);
        assert!(result.is_err());
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
        assert!(result.is_err());
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
                assert_eq!(args.device.as_deref(), Some("G502"));
            }
            other => panic!("expected Diag(Profiles), got {other:?}"),
        }
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
