//! Revision-fenced RGB application shared by all configuration writers.

use std::{
    collections::HashMap,
    future::Future,
    path::Path,
    sync::{Arc, Mutex, PoisonError},
};

use openlogi_core::config::{Config, ConfigFile, ConfigRevision, Lighting};
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId};
use openlogi_hid::{DeviceRoute, HidppOperation, WriteError};

use super::DeviceOp;

/// The stable device identity and link captured by the orchestrator. The config
/// entry is resolved against the file read during the write turn.
pub struct SavedLightingTarget {
    stable: DeviceStableId,
    identity: Option<DeviceIdentity>,
    unit: [u8; 4],
}

impl SavedLightingTarget {
    pub(crate) fn new(
        stable: DeviceStableId,
        identity: Option<DeviceIdentity>,
        unit: [u8; 4],
    ) -> Self {
        Self {
            stable,
            identity,
            unit,
        }
    }

    fn authorizes(&self, config: &Config, lighting: &Lighting) -> bool {
        config
            .resolve_device_key(&self.stable, self.identity.as_ref())
            .and_then(|key| config.devices.get(key.as_str()))
            .and_then(|device| device.effective_lighting(&self.stable.route_key()))
            == Some(lighting)
    }
}

/// Serializes each keyboard's saved RGB applications through native completion.
/// A delayed client cannot replace a newer effective RGB choice, while unrelated
/// saves and writes to other keyboards remain independent.
#[derive(Default)]
pub struct SavedLightingWrites(Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>);

impl SavedLightingWrites {
    /// Apply `lighting` while its save or the latest effective RGB still authorizes it.
    /// Success acknowledges a completed write, rather than hardware readback.
    pub async fn apply(
        &self,
        path: &Path,
        revision: ConfigRevision,
        device: DeviceOp,
        target: &SavedLightingTarget,
        lighting: &Lighting,
    ) -> Result<(), WriteError> {
        let route = device.route.clone();
        let device = device.confirming_saved_lighting(target.unit)?;
        self.apply_with(path, revision, &route, target, lighting, || async move {
            device.lighting(lighting)?.finish().await
        })
        .await
    }

    async fn apply_with<F, Fut>(
        &self,
        path: &Path,
        revision: ConfigRevision,
        route: &DeviceRoute,
        target: &SavedLightingTarget,
        lighting: &Lighting,
        write: F,
    ) -> Result<(), WriteError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), WriteError>>,
    {
        // Normal requests retain their turn through native completion. On RPC
        // cancellation, LightingJob retains its route lock through compensation.
        let turn = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(route.to_string())
            .or_default()
            .clone();
        let _turn = turn.lock().await;
        let (config, file) =
            ConfigFile::load_from_path(path).map_err(|error| WriteError::Hid(error.to_string()))?;
        if file.revision() != revision && !target.authorizes(&config, lighting) {
            return Err(WriteError::WriteSuperseded {
                operation: HidppOperation::Lighting,
            });
        }
        write().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::config::{ConfigEdit, Lighting, LinkConfig, LinkOverrides};

    fn rgb_responder(request: &[u8]) -> Option<Vec<u8>> {
        let mut response = vec![0; 20];
        response[..4].copy_from_slice(request.get(..4)?);
        response[0] = 0x11;
        match (request[2], request[3] >> 4) {
            (0, 1) => response[4] = 4,
            (0, 0) => response[4] = u8::from(request[4..6] == [0x80, 0x70]) * 4,
            (4, 0) => response[4] = 1,
            (4, 1) => {
                response[4] = request[4];
                response[6] = 1;
                response[7] = 1;
            }
            (4, 2) => {
                response[4] = request[4];
                response[5] = request[5];
                response[7] = 1;
            }
            (4, 3) => {}
            _ => return None,
        }
        Some(response)
    }

    #[tokio::test]
    async fn saved_lighting_refuses_replaced_receiver_device() {
        use crate::hardware::{DeviceAccess, TransportTurns};
        use crate::receiver_access::{ExclusiveAccessReason, ReceiverAccess};
        use crate::test_support::{UNIT, reporting_unit};
        let route = DeviceRoute::Bolt {
            receiver_uid: "saved-rgb".into(),
            slot: 1,
        };
        let unit = Arc::new(Mutex::new(UNIT));
        let registry = openlogi_hid::ChannelRegistry::default();
        let (_, reports) = openlogi_device::test_support::publish_scripted_channel(
            &registry,
            "saved-rgb",
            route.clone(),
            reporting_unit(unit.clone(), rgb_responder),
        )
        .await;
        let access = ReceiverAccess::default();
        let device = DeviceAccess {
            channel: Arc::new(std::sync::RwLock::new(None)),
            registry,
            receiver_access: access.clone(),
            device_io: openlogi_hid::device_io_channel().1,
            transport_turns: TransportTurns::default(),
        };
        let target = SavedLightingTarget::new(
            DeviceStableId::Bolt {
                receiver_uid: "saved-rgb".into(),
                slot: 1,
            },
            Some(DeviceIdentity::Serial("saved-rgb".into())),
            UNIT,
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let revision = ConfigEdit::load(&path).unwrap().commit().unwrap();
        let writes = SavedLightingWrites::default();
        let lighting = Lighting::default();
        for replaced in [true, false] {
            let held = access
                .acquire_exclusive(ExclusiveAccessReason::Pairing)
                .await;
            let mut pending = std::pin::pin!(writes.apply(
                &path,
                revision,
                device.op(&route),
                &target,
                &lighting
            ));
            tokio::select! { biased; result = &mut pending => panic!("write bypassed pairing: {result:?}"), () = std::future::ready(()) => {} }
            *unit.lock().unwrap() = if replaced {
                [0x87, 0x65, 0x43, 0x21]
            } else {
                UNIT
            };
            drop(held);
            let result = tokio::time::timeout(std::time::Duration::from_secs(10), pending)
                .await
                .unwrap();
            assert!(reports.written_reports().iter().any(|r| r[2] == 0x0e));
            if replaced {
                assert!(
                    matches!(result, Err(WriteError::DeviceNotFound)),
                    "{result:?}"
                );
                assert!(
                    !reports
                        .written_reports()
                        .iter()
                        .any(|r| r[2] == 4 && r[3] >> 4 == 3)
                );
            } else {
                result.unwrap();
                assert_eq!(
                    reports
                        .written_reports()
                        .iter()
                        .filter(|r| r[2] == 4 && r[3] >> 4 == 3)
                        .count(),
                    1
                );
            }
        }
        let unknown = SavedLightingTarget::new(
            DeviceStableId::Bolt {
                receiver_uid: "saved-rgb".into(),
                slot: 1,
            },
            None,
            [0; 4],
        );
        let before = reports.written_reports().len();
        assert!(matches!(
            writes
                .apply(&path, revision, device.op(&route), &unknown, &lighting)
                .await,
            Err(WriteError::DeviceNotFound)
        ));
        assert_eq!(reports.written_reports().len(), before);
    }

    fn route() -> DeviceRoute {
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc33c,
        }
    }

    fn target() -> SavedLightingTarget {
        SavedLightingTarget::new(
            DeviceStableId::from_parts(
                Some(&route()),
                openlogi_core::hid::DIRECT_DEVICE_INDEX,
                None,
                [0x12, 0x34, 0x56, 0x78],
            ),
            Some(DeviceIdentity::Unit([0x12, 0x34, 0x56, 0x78])),
            [0x12, 0x34, 0x56, 0x78],
        )
    }

    #[tokio::test]
    async fn lighting_saved_writes_keep_revision_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let writes = SavedLightingWrites::default();
        let route = route();
        let target = target();
        let mut edit = ConfigEdit::load(&path).unwrap();
        edit.config_mut()
            .set_lighting("unit:12345678", Lighting::default());
        let older = edit.commit().unwrap();
        // A's reload acknowledgement is delayed. B saves and completes first.
        let mut edit = ConfigEdit::load(&path).unwrap();
        let off = Lighting {
            enabled: false,
            ..Lighting::default()
        };
        edit.config_mut().set_lighting("unit:12345678", off.clone());
        let newer = edit.commit().unwrap();
        writes
            .apply_with(&path, newer, &route, &target, &off, || async { Ok(()) })
            .await
            .unwrap();
        let error = writes
            .apply_with(
                &path,
                older,
                &route,
                &target,
                &Lighting::default(),
                || async { panic!("the older command must never reach the keyboard") },
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            WriteError::WriteSuperseded {
                operation: HidppOperation::Lighting
            }
        ));
    }

    #[tokio::test]
    async fn a_new_save_waits_for_the_previous_native_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let writes = SavedLightingWrites::default();
        let route = route();
        let target = target();
        let lighting = Lighting::default();
        let revision = ConfigEdit::load(&path).unwrap().commit().unwrap();
        let (started, wait_started) = tokio::sync::oneshot::channel();
        let (finish, wait_finish) = tokio::sync::oneshot::channel();
        let first = writes.apply_with(&path, revision, &route, &target, &lighting, || async {
            started.send(()).unwrap();
            wait_finish.await.unwrap();
            Ok(())
        });
        let successor = async {
            wait_started.await.unwrap();
            // A new file is allowed while hardware I/O is in flight.
            let mut edit = ConfigEdit::load(&path).unwrap();
            edit.config_mut()
                .set_lighting("unit:12345678", Lighting::default());
            let revision = edit.commit().unwrap();
            let mut second = std::pin::pin!(writes.apply_with(
                &path,
                revision,
                &route,
                &target,
                &lighting,
                || async { Ok(()) }
            ));
            tokio::select! {
                biased;
                _ = &mut second => panic!("B overtook A's native transaction"),
                () = std::future::ready(()) => {}
            }
            finish.send(()).unwrap();
            second.await.unwrap();
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(first, successor)
        })
        .await
        .expect("saved RGB ordering must complete within ten seconds");
        result.unwrap();
    }

    #[tokio::test]
    async fn unrelated_saves_are_allowed_but_link_rgb_changes_supersede() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let writes = SavedLightingWrites::default();
        let mut edit = ConfigEdit::load(&path).unwrap();
        let lighting = Lighting::default();
        edit.config_mut()
            .set_lighting("unit:12345678", lighting.clone());
        let revision = edit.commit().unwrap();
        let mut edit = ConfigEdit::load(&path).unwrap();
        edit.config_mut().set_dpi("unit:87654321", 2600.into());
        let unrelated = edit.commit().unwrap();
        assert_ne!(revision, unrelated);
        writes
            .apply_with(&path, revision, &route(), &target(), &lighting, || async {
                Ok(())
            })
            .await
            .unwrap();
        let mut edit = ConfigEdit::load(&path).unwrap();
        edit.config_mut()
            .devices
            .get_mut("unit:12345678")
            .unwrap()
            .links
            .insert(
                target().stable.route_key(),
                LinkConfig {
                    overrides: LinkOverrides {
                        lighting: Some(Lighting {
                            enabled: false,
                            ..lighting.clone()
                        }),
                        ..LinkOverrides::default()
                    },
                    ..LinkConfig::default()
                },
            );
        edit.commit().unwrap();
        let error = writes
            .apply_with(&path, revision, &route(), &target(), &lighting, || async {
                panic!("a changed active-link RGB choice must supersede the old write")
            })
            .await
            .unwrap_err();
        assert!(matches!(error, WriteError::WriteSuperseded { .. }));
    }

    #[tokio::test]
    async fn another_keyboard_does_not_wait_for_an_active_native_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let writes = SavedLightingWrites::default();
        let revision = ConfigEdit::load(&path).unwrap().commit().unwrap();
        let route = route();
        let target = target();
        let lighting = Lighting::default();
        let (started, wait_started) = tokio::sync::oneshot::channel();
        let (finish, wait_finish) = tokio::sync::oneshot::channel();
        let first = writes.apply_with(&path, revision, &route, &target, &lighting, || async {
            started.send(()).unwrap();
            wait_finish.await.unwrap();
            Ok(())
        });
        let second = async {
            wait_started.await.unwrap();
            let other = DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xc33d,
            };
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                writes.apply_with(&path, revision, &other, &target, &lighting, || async {
                    Ok(())
                }),
            )
            .await
            .expect("another keyboard must not wait for A's native completion")
            .unwrap();
            finish.send(()).unwrap();
        };
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(first, second)
        })
        .await
        .expect("independent keyboard writes must complete within ten seconds");
        result.unwrap();
    }

    #[tokio::test]
    async fn route_adoption_keeps_the_same_physical_rgb_authorized() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(openlogi_core::paths::CONFIG_FILE);
        let stable = DeviceStableId::Bolt {
            receiver_uid: "rgb-test".into(),
            slot: 1,
        };
        let identity = DeviceIdentity::Unit([0x12, 0x34, 0x56, 0x78]);
        let target =
            SavedLightingTarget::new(stable.clone(), Some(identity), [0x12, 0x34, 0x56, 0x78]);
        let legacy = stable.physical_key().unwrap();
        let lighting = Lighting::default();
        let mut edit = ConfigEdit::load(&path).unwrap();
        edit.config_mut()
            .set_lighting(legacy.as_str(), lighting.clone());
        let revision = edit.commit().unwrap();
        let mut edit = ConfigEdit::load(&path).unwrap();
        let canonical =
            openlogi_core::device_order::PhysicalDeviceKey::parse("unit:12345678").unwrap();
        assert!(
            edit.config_mut()
                .adopt_route(&canonical, &stable.route_key(), None)
        );
        assert!(!edit.config().devices.contains_key(legacy.as_str()));
        assert_ne!(edit.commit().unwrap(), revision);
        SavedLightingWrites::default()
            .apply_with(&path, revision, &route(), &target, &lighting, || async {
                Ok(())
            })
            .await
            .unwrap();
    }
}
