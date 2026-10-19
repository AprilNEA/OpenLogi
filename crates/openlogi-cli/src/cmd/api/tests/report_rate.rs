use openlogi_core::hid::{ReportRate, ReportRateInfo};

use super::*;

/// What the scripted mouse lists for its connection.
const LISTED: [u16; 3] = [125, 500, 1000];

pub(super) fn info(current: u16) -> ReportRateInfo {
    ReportRateInfo {
        current: ReportRate::new(current),
        supported: LISTED.map(ReportRate::new).to_vec(),
    }
}

pub(super) fn report_rate_command(set: Option<u16>) -> ApiCommand {
    ApiCommand::ReportRate {
        device: route().to_string(),
        set,
    }
}

pub(super) fn report_rate_read(current: u16) -> Step {
    Box::new(move |request| {
        let AgentRequest::ReadReportRate { route: requested } = request else {
            panic!("expected report-rate read, got {request:?}");
        };
        assert_eq!(requested, route());
        AgentResponse::ReadReportRate(Ok(info(current)))
    })
}

/// A write of 1000 Hz answered with `response`.
pub(super) fn report_rate_write(response: Result<ReportRateInfo, WriteError>) -> Step {
    Box::new(move |request| {
        let AgentRequest::SetReportRate {
            route: requested,
            rate,
        } = request
        else {
            panic!("expected report-rate write, got {request:?}");
        };
        assert_eq!(requested, route());
        assert_eq!(rate, ReportRate::new(1000));
        AgentResponse::SetReportRate(response)
    })
}

#[tokio::test]
async fn report_rate_reads_the_rate_and_the_rates_the_device_lists() {
    let result = run_script(
        snapshot(),
        report_rate_command(None),
        vec![report_rate_read(500)],
    )
    .await;
    assert_eq!(
        result["data"],
        json!({
            "device": "slot 3 on receiver test-receiver", "persistence": "not_saved",
            "current": 500, "supported": [125, 500, 1000],
        })
    );
}

#[tokio::test]
async fn report_rate_refuses_an_unlisted_rate_without_writing() {
    let result = run_script(
        snapshot(),
        report_rate_command(Some(2000)),
        vec![report_rate_read(500)],
    )
    .await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "invalid_value");
}

#[tokio::test]
async fn report_rate_write_reports_the_rate_the_device_reads_back() {
    for (readback, success) in [(1000, true), (500, false)] {
        let result = run_script(
            snapshot(),
            report_rate_command(Some(1000)),
            vec![report_rate_read(500), report_rate_write(Ok(info(readback)))],
        )
        .await;
        assert_eq!(result["ok"], success);
        if success {
            assert_eq!(result["data"]["current"], 1000);
            assert_eq!(result["data"]["supported"], json!([125, 500, 1000]));
        } else {
            assert_eq!(result["error"]["code"], "readback_mismatch");
        }
    }
}

#[tokio::test]
async fn agent_side_report_rate_rejections_use_the_local_codes() {
    for (error, code) in [
        (
            WriteError::ReportRateUnsupported {
                requested: ReportRate::new(1000),
            },
            "invalid_value",
        ),
        (
            WriteError::ReportRateNotApplied {
                requested: ReportRate::new(1000),
                actual: ReportRate::new(500),
            },
            "readback_mismatch",
        ),
        (
            WriteError::FeatureUnsupported {
                feature_hex: 0x8061,
            },
            "unsupported_feature",
        ),
    ] {
        let result = run_script(
            snapshot(),
            report_rate_command(Some(1000)),
            vec![report_rate_read(500), report_rate_write(Err(error))],
        )
        .await;
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"]["code"], code);
    }
}

#[test]
fn parser_takes_report_rates_in_hertz() {
    let cli = crate::Cli::try_parse_from([
        "openlogi",
        "api",
        "report-rate",
        "--device",
        "id",
        "--set",
        "8000",
    ])
    .expect("an 8 kHz rate parses");
    assert!(matches!(
        cli.cmd,
        Some(crate::cmd::Command::Api(ApiArgs {
            command: ApiCommand::ReportRate {
                set: Some(8000),
                ..
            },
            ..
        }))
    ));
    for args in [
        vec!["openlogi", "api", "report-rate", "--set", "1000"],
        vec![
            "openlogi",
            "api",
            "report-rate",
            "--device",
            "id",
            "--set",
            "65536",
        ],
    ] {
        crate::Cli::try_parse_from(args).expect_err("invalid report-rate arguments");
    }
}
