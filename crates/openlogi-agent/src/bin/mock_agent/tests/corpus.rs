//! Contributed profiles through the production mock service and IPC framing.

use super::super::*;
use openlogi_ipc::AgentClient;

async fn check_profile(client: &AgentClient, profile: &DeviceProfile) {
    let snapshot = client
        .snapshot(tarpc::context::current())
        .await
        .expect("snapshot RPC");
    assert_eq!(snapshot.inventory, profile.inventories, "{}", profile.id);
    assert_eq!(snapshot.standalone, profile.standalone, "{}", profile.id);
    for settings in &profile.settings {
        let route = &settings.route;
        let dpi = client
            .read_dpi(tarpc::context::current(), route.clone())
            .await
            .expect("DPI transport");
        check_setting(&settings.dpi, dpi);
        check_setting(
            &settings.smartshift,
            client
                .read_smartshift(tarpc::context::current(), route.clone())
                .await
                .expect("SmartShift transport"),
        );
        check_setting(
            &settings.wheel,
            client
                .read_wheel(tarpc::context::current(), route.clone())
                .await
                .expect("wheel transport"),
        );
        check_setting(
            &settings.backlight,
            client
                .read_backlight(tarpc::context::current(), route.clone())
                .await
                .expect("backlight transport"),
        );

        if let ProfileSetting::Supported(info) = &settings.dpi {
            let next = info
                .capabilities
                .values()
                .iter()
                .find(|dpi| **dpi != info.current)
                .expect("recorded adjustable DPI range");
            client
                .set_dpi(tarpc::context::current(), route.clone(), *next)
                .await
                .expect("DPI transport")
                .expect("supported DPI write");
            let actual = client
                .read_dpi(tarpc::context::current(), route.clone())
                .await
                .expect("DPI transport")
                .expect("DPI readback");
            assert_eq!(actual.current, *next);
            assert_eq!(actual.capabilities, info.capabilities);
        } else {
            let written = client
                .set_dpi(tarpc::context::current(), route.clone(), Dpi::new(1000))
                .await
                .expect("DPI transport");
            assert!(
                matches!(written, Err(WriteError::FeatureUnsupported { .. })),
                "{}: {written:?}",
                profile.id
            );
        }
        if let ProfileSetting::Supported(initial) = settings.smartshift {
            let mut changed = initial;
            changed.mode = initial.mode.flipped();
            client
                .set_smartshift(tarpc::context::current(), route.clone(), changed)
                .await
                .expect("SmartShift transport")
                .expect("supported SmartShift write");
            let actual = client
                .read_smartshift(tarpc::context::current(), route.clone())
                .await
                .expect("SmartShift transport")
                .expect("SmartShift readback");
            assert_eq!(actual, changed);
        }
    }
}

fn check_setting<T: std::fmt::Debug + PartialEq>(
    expected: &ProfileSetting<T>,
    actual: Result<T, WriteError>,
) {
    match expected {
        ProfileSetting::Supported(value) => {
            assert_eq!(actual.expect("supported recorded setting"), *value);
        }
        ProfileSetting::Unsupported => assert!(
            matches!(actual, Err(WriteError::FeatureUnsupported { .. })),
            "{actual:?}"
        ),
        ProfileSetting::Unavailable => assert!(
            matches!(actual, Err(WriteError::DeviceUnreachable { .. })),
            "{actual:?}"
        ),
    }
}

#[tokio::test]
async fn contributed_corpus_mock_rpcs_preserve_support_and_write_readback() {
    let Some(corpus) = openlogi_fixture::fs::repository_corpus().expect("valid corpus") else {
        return;
    };
    for fixture in corpus {
        let profile = fixture.profile();
        let agent = MockAgent::new(
            State::new(profile.clone(), MockClock::Test(Duration::ZERO)).expect("valid profile"),
        );
        let (client_transport, server_transport) = tarpc::transport::channel::unbounded();
        let server = tokio::spawn(
            BaseChannel::with_defaults(server_transport)
                .execute(agent.serve())
                .for_each(|response| async move {
                    tokio::spawn(response);
                }),
        );
        let client = AgentClient::new(tarpc::client::Config::default(), client_transport).spawn();
        check_profile(&client, profile).await;
        drop(client);
        server.await.expect("mock service shuts down");
    }
}

// Unix endpoints can be isolated with XDG_CONFIG_HOME; Windows uses a fixed pipe.
#[cfg(unix)]
#[test]
fn contributed_corpus_mock_socket_handshake_and_observation() {
    const TEST: &str = "tests::corpus::contributed_corpus_mock_socket_handshake_and_observation";
    const CHILD: &str = "OPENLOGI_CORPUS_SOCKET_TEST";
    if std::env::var_os(CHILD).is_none() {
        let directory = tempfile::tempdir().expect("isolated socket directory");
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, "1")
            .env("XDG_CONFIG_HOME", directory.path())
            .env_remove("XDG_RUNTIME_DIR")
            .env(openlogi_core::env::PROFILE, "dev")
            .output()
            .expect("isolated test process");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    openlogi_core::worker::runtime()
        .expect("test runtime")
        .block_on(async {
            let Some(corpus) = openlogi_fixture::fs::repository_corpus().expect("valid corpus")
            else {
                return;
            };
            let _ownership = single_instance::acquire(Role::Agent).expect("isolated agent lock");
            for fixture in corpus {
                let agent = MockAgent::new(
                    State::new(fixture.profile().clone(), MockClock::Test(Duration::ZERO))
                        .expect("valid profile"),
                );
                let listener = transport::bind().expect("isolated listener");
                let server = tokio::spawn(serve_on(listener, agent.clone()));
                let client = openlogi_ipc::client::connect_as(ClientKind::Gui)
                    .await
                    .expect("production handshake");
                check_profile(&client, fixture.profile()).await;
                let mut observer = openlogi_ipc::client::Observer::state(client);
                let initial = observer
                    .next()
                    .await
                    .expect("observation transport")
                    .expect("initial snapshot");
                assert_eq!(initial.snapshot.inventory, fixture.profile().inventories);
                agent
                    .state
                    .lock()
                    .await
                    .advance_test_time(FOREGROUND_SWITCH_PERIOD);
                let changed = observer
                    .next()
                    .await
                    .expect("observation transport")
                    .expect("new logical-time snapshot");
                assert_ne!(changed.snapshot.foreground, initial.snapshot.foreground);
                assert_eq!(changed.snapshot.inventory, fixture.profile().inventories);
                drop(observer);
                server.abort();
                assert!(server.await.expect_err("server cancelled").is_cancelled());
            }
        });
}
