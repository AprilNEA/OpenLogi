//! Recorded setting reads through the agent's lifecycle gate and exact-route registry.

use super::*;
use crate::{observable::ObservableState, orchestrator::Orchestrator};
use openlogi_fixture::{ProfileSetting, fs::repository_corpus};
use openlogi_hid::{HidppOperation, device_io_channel, replay::ReplayBackend};

#[tokio::test]
async fn contributed_corpus_reads_obey_agent_channel_and_suspend_boundaries() {
    let Some(corpus) = repository_corpus().expect("valid corpus") else {
        return;
    };
    for fixture in corpus {
        for cassette in fixture.cassettes().iter().filter(|case| {
            matches!(
                case.name.as_str(),
                "dpi-info" | "smartshift-status" | "wheel-mode" | "backlight-state"
            )
        }) {
            let (backend, route) = ReplayBackend::from_profile(fixture.profile(), cassette)
                .expect("single-device contribution");
            let backend = Arc::new(backend);
            let (signal, gate) = device_io_channel();
            let hardware = HardwareContext::injected(backend.clone(), gate);
            let orchestrator = Orchestrator::with_hardware(
                openlogi_core::config::Config::ephemeral(),
                Arc::new(ObservableState::new("corpus-test".into())),
                hardware,
            );
            let shared = orchestrator.shared();
            assert_eq!(
                shared
                    .device(&route)
                    .run(HidppOperation::ReadDpi, |_| async {
                        panic!("unpublished route must not run")
                    })
                    .await,
                Err::<(), _>(WriteError::DeviceNotFound)
            );
            backend
                .publish_route(&shared.channel_registry, &route)
                .await
                .expect("recorded route opens");
            assert!(signal.suspend());
            assert_eq!(
                shared
                    .device(&route)
                    .run(HidppOperation::ReadDpi, |_| async {
                        panic!("suspended I/O must not run")
                    })
                    .await,
                Err::<(), _>(WriteError::DeviceNotFound)
            );
            assert!(signal.resume());
            let settings = &fixture.profile().settings[0];
            match cassette.name.as_str() {
                "dpi-info" => check(
                    &settings.dpi,
                    shared
                        .device(&route)
                        .run(HidppOperation::ReadDpiCapabilities, |channel| async move {
                            openlogi_hid::get_dpi_info_on(&channel).await
                        })
                        .await,
                ),
                "smartshift-status" => check(
                    &settings.smartshift,
                    shared
                        .device(&route)
                        .run(HidppOperation::ReadSmartShift, |channel| async move {
                            openlogi_hid::get_smartshift_status_on(&channel).await
                        })
                        .await,
                ),
                "wheel-mode" => check(
                    &settings.wheel,
                    shared
                        .device(&route)
                        .run(HidppOperation::ReadWheelMode, |channel| async move {
                            openlogi_hid::get_scroll_wheel_mode_on(&channel).await
                        })
                        .await,
                ),
                "backlight-state" => check(
                    &settings.backlight,
                    shared
                        .device(&route)
                        .run(HidppOperation::ReadBacklight, |channel| async move {
                            openlogi_hid::get_backlight_on(&channel).await
                        })
                        .await,
                ),
                _ => unreachable!("filtered setting case"),
            }
            backend.require_complete().unwrap_or_else(|error| {
                panic!("{}/{}: {error}", fixture.manifest().id, cassette.name)
            });
            assert_eq!(
                backend
                    .channel_completion(&cassette.channel)
                    .expect("recorded channel")
                    .channel_open_count,
                1
            );
        }
    }
}

fn check<T: std::fmt::Debug + PartialEq>(
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
        ProfileSetting::Unavailable => {
            panic!("recorded setting needs an independently reviewed result")
        }
    }
}
