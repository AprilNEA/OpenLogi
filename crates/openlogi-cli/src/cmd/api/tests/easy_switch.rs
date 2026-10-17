use openlogi_core::hid::{EasySwitchHosts, EasySwitchSlot, HostConnection, HostOsInfo, HostOsKind};
use openlogi_core::paths::CONFIG_FILE;

use super::*;

/// Two channels: "Desk" on channel 1, the current one, and nothing on 2.
fn hosts(name: &str) -> EasySwitchHosts {
    EasySwitchHosts {
        current: Some(0),
        slots: vec![
            EasySwitchSlot {
                index: 0,
                paired: true,
                connection: HostConnection::Bluetooth,
                name: Some(name.to_owned()),
                name_max_len: 24,
                os: Some(HostOsInfo {
                    kind: HostOsKind::MacOs,
                    major: 15,
                    minor: 2,
                    patch: 0,
                }),
            },
            EasySwitchSlot {
                index: 1,
                paired: false,
                connection: HostConnection::Unknown,
                name: None,
                name_max_len: 24,
                os: None,
            },
        ],
        can_rename: true,
    }
}

fn command(rename: Option<(u8, &str)>) -> ApiCommand {
    ApiCommand::EasySwitch {
        device: route().to_string(),
        channel: rename.map(|(channel, _)| channel),
        name: rename.map(|(_, name)| name.to_owned()),
    }
}

fn read(name: &'static str) -> Step {
    Box::new(move |request| {
        let AgentRequest::ReadEasySwitchHosts { route: requested } = request else {
            panic!("expected Easy-Switch read, got {request:?}");
        };
        assert_eq!(requested, route());
        AgentResponse::ReadEasySwitchHosts(Ok(hosts(name)))
    })
}

/// A rename of channel 1 (slot 0) to " Studio " answered with `response`.
fn rename(response: Result<EasySwitchHosts, WriteError>) -> Step {
    Box::new(move |request| {
        let AgentRequest::RenameEasySwitchHost {
            route: requested,
            slot,
            name,
        } = request
        else {
            panic!("expected Easy-Switch rename, got {request:?}");
        };
        assert_eq!(requested, route());
        assert_eq!((slot, name.as_str()), (0, " Studio "));
        AgentResponse::RenameEasySwitchHost(response)
    })
}

#[tokio::test]
async fn easy_switch_lists_channels_numbered_from_one() {
    let result = run_script(snapshot(), command(None), vec![read("Desk")]).await;
    assert_eq!(
        result["data"],
        json!({
            "device": "slot 3 on receiver test-receiver", "persistence": "not_saved",
            "current_channel": 1, "can_rename": true,
            "channels": [
                {
                    "channel": 1, "paired": true, "connection": "bluetooth",
                    "name": "Desk", "name_max_len": 24, "os": "macOS 15.2",
                },
                {
                    "channel": 2, "paired": false, "connection": "unknown",
                    "name": null, "name_max_len": 24, "os": null,
                },
            ],
        })
    );
}

#[tokio::test]
async fn easy_switch_rename_reports_the_name_the_device_reads_back() {
    for (readback, success) in [("Studio", true), ("Desk", false)] {
        let result = run_script(
            snapshot(),
            command(Some((1, " Studio "))),
            vec![rename(Ok(hosts(readback)))],
        )
        .await;
        assert_eq!(result["ok"], success, "read back {readback}");
        if success {
            assert_eq!(result["data"]["channels"][0]["name"], "Studio");
        } else {
            assert_eq!(result["error"]["code"], "readback_mismatch");
        }
    }
}

#[tokio::test]
async fn agent_side_rename_rejections_map_to_stable_codes() {
    for (error, code) in [
        (WriteError::HostRenameUnsupported, "unsupported_feature"),
        (WriteError::HostSlotEmpty { slot: 0 }, "invalid_value"),
        (
            WriteError::HostNameRejected { max_len: 24 },
            "invalid_value",
        ),
        (
            WriteError::HostNameNotApplied {
                requested: "Studio".into(),
                actual: Some("Desk".into()),
            },
            "readback_mismatch",
        ),
    ] {
        let result = run_script(
            snapshot(),
            command(Some((1, " Studio "))),
            vec![rename(Err(error))],
        )
        .await;
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"]["code"], code);
    }
}

#[tokio::test]
async fn easy_switch_refuses_save_before_any_device_rpc() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join(CONFIG_FILE);
    let result = run_script_at(
        snapshot(),
        command(Some((1, "Studio"))),
        Vec::new(),
        Some(&path),
    )
    .await;
    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "invalid_value");
    assert!(
        result["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("Easy-Switch")),
        "{result}"
    );
    assert!(!path.exists(), "nothing may be saved");
}

#[test]
fn parser_takes_one_based_channels_and_needs_both_halves_of_a_rename() {
    let cli = crate::Cli::try_parse_from([
        "openlogi",
        "api",
        "easy-switch",
        "--device",
        "id",
        "--channel",
        "3",
        "--name",
        "Studio",
    ])
    .expect("a rename parses");
    assert!(matches!(
        cli.cmd,
        Some(crate::cmd::Command::Api(ApiArgs {
            command: ApiCommand::EasySwitch {
                channel: Some(3),
                name: Some(_),
                ..
            },
            ..
        }))
    ));
    for args in [
        vec![
            "openlogi",
            "api",
            "easy-switch",
            "--device",
            "id",
            "--channel",
            "0",
            "--name",
            "x",
        ],
        vec![
            "openlogi",
            "api",
            "easy-switch",
            "--device",
            "id",
            "--channel",
            "1",
        ],
        vec![
            "openlogi",
            "api",
            "easy-switch",
            "--device",
            "id",
            "--name",
            "x",
        ],
    ] {
        crate::Cli::try_parse_from(args).expect_err("invalid easy-switch arguments");
    }
}
