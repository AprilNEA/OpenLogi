use super::*;
use openlogi_core::binding::KeyCombo;
use openlogi_core::peripheral::HidUsage;
use openlogi_hid::native_mapping::MappingArray;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Host(Arc<Mutex<HostState>>);

struct HostState {
    evidence: ServiceEvidence,
    writes: usize,
    fail_write: bool,
    replace_on_next_read: Option<u64>,
}

impl Host {
    fn new(mappings: MappingArray) -> Self {
        Self(Arc::new(Mutex::new(HostState {
            evidence: ServiceEvidence {
                boot: "boot-one".into(),
                services: BTreeMap::from([(41, mappings)]),
            },
            writes: 0,
            fail_write: false,
            replace_on_next_read: None,
        })))
    }

    fn mappings(&self) -> MappingArray {
        self.0.lock().unwrap().evidence.common().unwrap().clone()
    }

    fn edit(&self, source: MappingUsage, destination: MappingUsage) {
        let mut host = self.0.lock().unwrap();
        let mappings = host
            .evidence
            .common()
            .unwrap()
            .replacing(source, Some(MappingEntry::new(source, destination)))
            .unwrap();
        host.evidence
            .services
            .values_mut()
            .for_each(|array| *array = mappings.clone());
    }
}

impl MappingBackend for Host {
    fn read(
        &mut self,
        _: MappingScope,
    ) -> impl Future<Output = Result<ServiceEvidence, PeripheralError>> + Send {
        let mut host = self.0.lock().unwrap();
        if let Some(id) = host.replace_on_next_read.take() {
            let mappings = match host.evidence.common() {
                Ok(mappings) => mappings.clone(),
                Err(error) => return std::future::ready(Err(error)),
            };
            host.evidence.services = BTreeMap::from([(id, mappings)]);
        }
        std::future::ready(Ok(host.evidence.clone()))
    }

    fn write(
        &mut self,
        _: MappingScope,
        mappings: &MappingArray,
    ) -> impl Future<Output = Result<(), PeripheralError>> + Send {
        let mut host = self.0.lock().unwrap();
        if host.fail_write {
            host.fail_write = false;
            return std::future::ready(Err(PeripheralError::WriteFailed(
                "interrupted before write".into(),
            )));
        }
        host.writes += 1;
        host.evidence
            .services
            .values_mut()
            .for_each(|array| *array = mappings.clone());
        std::future::ready(Ok(()))
    }
}

#[derive(Clone, Default)]
struct Journal(Arc<Mutex<JournalState>>);

#[derive(Default)]
struct JournalState {
    saved: Vec<MappingEffect>,
    saves: usize,
    fail_save: Option<usize>,
    during_save: Option<(Host, u64)>,
}

impl JournalStore for Journal {
    fn load(&mut self) -> Result<Vec<MappingEffect>, PeripheralError> {
        Ok(self.0.lock().unwrap().saved.clone())
    }
    fn save(&mut self, effects: &[MappingEffect]) -> Result<(), PeripheralError> {
        let mut journal = self.0.lock().unwrap();
        journal.saves += 1;
        if Some(journal.saves) == journal.fail_save {
            journal.fail_save = None;
            return Err(PeripheralError::JournalFailed(
                "interrupted journal commit".into(),
            ));
        }
        journal.saved = effects.to_vec();
        if let Some((host, id)) = journal.during_save.take() {
            host.0.lock().unwrap().replace_on_next_read = Some(id);
        }
        Ok(())
    }
}

fn key() -> EffectKey {
    EffectKey {
        scope: MappingScope {
            vendor_id: 0xffff,
            product_id: 1,
        },
        source: HidUsage {
            page: 12,
            usage: 233,
        }
        .into(),
    }
}
fn target(key: &str) -> MappingUsage {
    key.parse::<KeyCombo>().unwrap().key().into()
}
fn initial() -> MappingArray {
    serde_json::from_value(json!([
        {"HIDKeyboardModifierMappingSrc": 30_064_771_076_u64, "HIDKeyboardModifierMappingDst": 30_064_771_179_u64, "label": "keep string 17"},
        {"HIDKeyboardModifierMappingSrc": 51_539_607_785_u64, "HIDKeyboardModifierMappingDst": 30_064_771_180_u64, "custom": true}
    ])).unwrap()
}

#[tokio::test]
async fn target_changes_keep_the_first_baseline_and_preserve_unrelated_edits() {
    let host = Host::new(initial());
    let mut manager = MappingManager::new(host.clone(), Journal::default()).unwrap();
    manager
        .reconcile(key(), "mic", Some(target("F18")), || Ok(()))
        .await
        .unwrap();
    manager
        .reconcile(key(), "mic", Some(target("F19")), || Ok(()))
        .await
        .unwrap();
    host.edit(HidUsage { page: 7, usage: 4 }.into(), target("F15"));
    assert_eq!(
        manager
            .reconcile(key(), "mic", None, || Ok(()))
            .await
            .unwrap(),
        ApplicationStatus::Restored
    );
    assert_eq!(
        serde_json::to_value(host.mappings()).unwrap(),
        json!([
            {"HIDKeyboardModifierMappingSrc": 30_064_771_076_u64, "HIDKeyboardModifierMappingDst": 30_064_771_178_u64},
            {"HIDKeyboardModifierMappingSrc": 51_539_607_785_u64, "HIDKeyboardModifierMappingDst": 30_064_771_180_u64, "custom": true}
        ])
    );
    assert_eq!(manager.effects().count(), 0);
}

#[tokio::test]
async fn external_source_changes_remain_suspended_across_restart_until_explicit_resolution() {
    let host = Host::new(initial());
    let journal = Journal::default();
    let mut manager = MappingManager::new(host.clone(), journal.clone()).unwrap();
    manager
        .reconcile(key(), "mic", Some(target("F18")), || Ok(()))
        .await
        .unwrap();
    host.edit(key().source, target("F19"));
    assert_eq!(
        manager
            .reconcile(key(), "mic", None, || Ok(()))
            .await
            .unwrap_err(),
        PeripheralError::ExternalModification
    );
    drop(manager);
    let mut manager = MappingManager::new(host.clone(), journal).unwrap();
    assert_eq!(
        manager
            .reconcile(key(), "mic", Some(target("F18")), || Ok(()))
            .await
            .unwrap_err(),
        PeripheralError::ExternalModification
    );
    assert_eq!(host.0.lock().unwrap().writes, 1);
    manager.resolve_conflict(key()).unwrap();
    let baseline = host.mappings();
    manager
        .reconcile(key(), "mic", Some(target("F18")), || Ok(()))
        .await
        .unwrap();
    manager
        .reconcile(key(), "mic", None, || Ok(()))
        .await
        .unwrap();
    assert_eq!(host.mappings(), baseline);
}

#[tokio::test]
async fn prepared_journal_recovers_crashes_before_and_after_the_native_write() {
    for after_write in [false, true] {
        let host = Host::new(initial());
        let journal = Journal::default();
        if after_write {
            journal.0.lock().unwrap().fail_save = Some(2);
        } else {
            host.0.lock().unwrap().fail_write = true;
        }
        let mut manager = MappingManager::new(host.clone(), journal.clone()).unwrap();
        manager
            .reconcile(key(), "mic", Some(target("F18")), || Ok(()))
            .await
            .expect_err("injected interruption");
        drop(manager);
        let mut restarted = MappingManager::new(host.clone(), journal).unwrap();
        restarted
            .reconcile(key(), "mic", None, || Ok(()))
            .await
            .unwrap();
        assert_eq!(host.mappings(), initial());
        assert_eq!(restarted.effects().count(), 0);
    }
}

#[tokio::test]
async fn a_new_attachment_captures_its_own_baseline_and_mixed_services_are_refused() {
    let host = Host::new(initial());
    let mut manager = MappingManager::new(host.clone(), Journal::default()).unwrap();
    manager
        .reconcile(key(), "mic", Some(target("F18")), || Ok(()))
        .await
        .unwrap();
    let replacement = MappingArray::default()
        .replacing(
            key().source,
            Some(MappingEntry::new(key().source, target("F19"))),
        )
        .unwrap();
    host.0.lock().unwrap().evidence.services = BTreeMap::from([(99, replacement.clone())]);
    manager
        .reconcile(key(), "mic", Some(target("F18")), || Ok(()))
        .await
        .unwrap();
    manager
        .reconcile(key(), "mic", None, || Ok(()))
        .await
        .unwrap();
    assert_eq!(host.mappings(), replacement);
    host.0
        .lock()
        .unwrap()
        .evidence
        .services
        .insert(100, initial());
    let writes = host.0.lock().unwrap().writes;
    assert_eq!(
        manager
            .reconcile(key(), "mic", Some(target("F18")), || Ok(()))
            .await
            .unwrap_err(),
        PeripheralError::MappingScopeConflict
    );
    assert_eq!(host.0.lock().unwrap().writes, writes);
}

#[tokio::test]
async fn failed_journal_or_replacement_during_prepare_prevents_writes() {
    let host = Host::new(initial());
    let journal = Journal::default();
    journal.0.lock().unwrap().fail_save = Some(1);
    let mut manager = MappingManager::new(host.clone(), journal.clone()).unwrap();
    manager
        .reconcile(key(), "mic", Some(target("F18")), || Ok(()))
        .await
        .expect_err("journal must commit before writing");
    assert_eq!(host.0.lock().unwrap().writes, 0);
    journal.0.lock().unwrap().during_save = Some((host.clone(), 99));
    assert_eq!(
        manager
            .reconcile(key(), "mic", Some(target("F18")), || Ok(()))
            .await
            .unwrap_err(),
        PeripheralError::StaleSession
    );
    assert_eq!(host.0.lock().unwrap().writes, 0);
}

#[tokio::test]
async fn file_journal_restores_after_restart_and_refuses_external_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("effects.json");
    let host = Host::new(initial());
    let mut manager = MappingManager::new(host.clone(), FileJournal::new(path.clone())).unwrap();
    manager
        .reconcile(key(), "mic", Some(target("F18")), || Ok(()))
        .await
        .unwrap();
    drop(manager);
    let mut manager = MappingManager::new(host.clone(), FileJournal::new(path.clone())).unwrap();
    manager
        .reconcile(key(), "mic", None, || Ok(()))
        .await
        .unwrap();
    assert_eq!(host.mappings(), initial());
    std::fs::write(path, "external edit").unwrap();
    manager
        .reconcile(key(), "mic", Some(target("F18")), || Ok(()))
        .await
        .expect_err("external journal edit must stop new writes");
    assert_eq!(host.0.lock().unwrap().writes, 2);
}

#[tokio::test]
async fn a_stale_desired_revision_cannot_write_after_preparation() {
    let host = Host::new(initial());
    let journal = Journal::default();
    let mut manager = MappingManager::new(host.clone(), journal.clone()).unwrap();
    let guard = || {
        if journal.0.lock().unwrap().saves == 0 {
            Ok(())
        } else {
            Err(PeripheralError::StaleSession)
        }
    };
    assert_eq!(
        manager
            .reconcile(key(), "mic", Some(target("F18")), guard)
            .await
            .unwrap_err(),
        PeripheralError::StaleSession
    );
    assert_eq!(host.0.lock().unwrap().writes, 0);
    manager
        .reconcile(key(), "recovery", None, || Ok(()))
        .await
        .unwrap();
    assert_eq!(host.mappings(), initial());
    assert_eq!(manager.effects().count(), 0);
}
