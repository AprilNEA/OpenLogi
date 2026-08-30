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
    use cmd::fixture::record_case::FixtureOperation;
    use cmd::fixture::{FixtureCmd, FixtureRecordCmd};

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

    #[test]
    fn fixture_record_case_parses_every_read_only_operation() {
        let operations = [
            ("feature-table", FixtureOperation::FeatureTable),
            ("firmware-entities", FixtureOperation::FirmwareEntities),
            (
                "reprogrammable-controls",
                FixtureOperation::ReprogrammableControls,
            ),
            ("raw-battery", FixtureOperation::RawBattery),
            ("dpi-info", FixtureOperation::DpiInfo),
            ("smartshift-status", FixtureOperation::SmartshiftStatus),
            ("wheel-mode", FixtureOperation::WheelMode),
            ("backlight-state", FixtureOperation::BacklightState),
        ];

        for (name, expected) in operations {
            let cli = Cli::try_parse_from([
                "openlogi",
                "fixture",
                "record",
                "case",
                "--operation",
                name,
                "--name",
                "human name",
                "--channel",
                "logical-channel",
                "--output",
                "case.json",
                "--device",
                "MX Master 3S",
                "--capacity",
                "1024",
                "--force",
            ])
            .unwrap_or_else(|error| panic!("{name} should parse: {error}"));

            match cli.cmd.expect("subcommand present") {
                Command::Fixture(FixtureCmd::Record(FixtureRecordCmd::Case(args))) => {
                    assert_eq!(args.operation, expected);
                    assert_eq!(args.name, "human name");
                    assert_eq!(args.channel, "logical-channel");
                    assert_eq!(args.output, std::path::PathBuf::from("case.json"));
                    assert_eq!(args.device.as_deref(), Some("MX Master 3S"));
                    assert_eq!(args.capacity, 1024);
                    assert!(args.force);
                }
                other => panic!("expected Fixture(Record(Case)), got {other:?}"),
            }
        }
    }

    #[test]
    fn fixture_record_case_requires_operation_metadata_and_output() {
        let required_flags = ["--operation", "--name", "--channel", "--output"];
        let complete = [
            "--operation",
            "feature-table",
            "--name",
            "human name",
            "--channel",
            "logical-channel",
            "--output",
            "case.json",
        ];

        for missing in required_flags {
            let mut args = vec!["openlogi", "fixture", "record", "case"];
            let mut index = 0;
            while index < complete.len() {
                if complete[index] != missing {
                    args.extend_from_slice(&complete[index..=index + 1]);
                }
                index += 2;
            }
            Cli::try_parse_from(args)
                .expect_err("every operation and metadata flag must be required");
        }
    }

    #[test]
    fn fixture_record_case_capacity_is_nonzero_and_bounded() {
        let parse = |capacity| {
            Cli::try_parse_from([
                "openlogi",
                "fixture",
                "record",
                "case",
                "--operation",
                "dpi-info",
                "--name",
                "dpi",
                "--channel",
                "direct",
                "--output",
                "case.json",
                "--capacity",
                capacity,
            ])
        };

        parse("1").expect("minimum capacity parses");
        parse("65536").expect("maximum capacity parses");
        parse("0").expect_err("zero capacity is rejected");
        parse("65537").expect_err("capacity above the bound is rejected");
    }

    #[test]
    fn fixture_record_case_exposes_no_write_pairing_or_allow_writes_path() {
        for operation in ["set-dpi", "pair", "unpair", "raw-lighting"] {
            Cli::try_parse_from([
                "openlogi",
                "fixture",
                "record",
                "case",
                "--operation",
                operation,
                "--name",
                "unsafe",
                "--channel",
                "direct",
                "--output",
                "case.json",
            ])
            .expect_err("write and pairing operation names must be rejected");
        }

        Cli::try_parse_from([
            "openlogi",
            "fixture",
            "record",
            "case",
            "--operation",
            "dpi-info",
            "--name",
            "dpi",
            "--channel",
            "direct",
            "--output",
            "case.json",
            "--allow-writes",
        ])
        .expect_err("--allow-writes must not exist in this command");
    }
}
