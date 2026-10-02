//! Durable ownership of native mappings across disable, restart, and reconnect.

mod journal;
pub use journal::FileJournal;

use std::collections::{BTreeMap, BTreeSet};

use openlogi_core::peripheral::{ApplicationStatus, PeripheralError};
use openlogi_hid::native_mapping::{
    MappingBackend, MappingEntry, MappingScope, MappingUsage, ServiceEvidence,
};
use serde::{Deserialize, Serialize};

/// Device scope and source usage that identify one independently owned effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EffectKey {
    /// Native setter scope.
    pub scope: MappingScope,
    /// Source usage owned by the rule.
    pub source: MappingUsage,
}

/// Durable recovery record. Cleanup does not require a working driver plugin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappingEffect {
    key: EffectKey,
    owner: String,
    original: Option<MappingEntry>,
    boot: String,
    services: BTreeSet<u64>,
    state: EffectState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum EffectState {
    Stable(Option<MappingEntry>),
    Prepared {
        before: Option<MappingEntry>,
        after: Option<MappingEntry>,
    },
    Conflicted,
}

/// Single-writer persistence boundary, also used for crash-point tests.
pub trait JournalStore: Send {
    /// Load recovery obligations before capturing any new baseline.
    fn load(&mut self) -> Result<Vec<MappingEffect>, PeripheralError>;
    /// Durably save all obligations before performing a reversible write.
    fn save(&mut self, effects: &[MappingEffect]) -> Result<(), PeripheralError>;
}

/// Serializes all native mapping writes and conditional restoration.
pub struct MappingManager<B, J> {
    backend: B,
    journal: J,
    effects: BTreeMap<EffectKey, MappingEffect>,
}

impl<B: MappingBackend, J: JournalStore> MappingManager<B, J> {
    /// Recover journal state. A journal failure disables new mapping writes.
    pub fn new(backend: B, mut journal: J) -> Result<Self, PeripheralError> {
        let mut effects = BTreeMap::new();
        for effect in journal.load()? {
            let source = effect.key.source;
            let entries = match &effect.state {
                EffectState::Stable(current) => vec![current],
                EffectState::Prepared { before, after } => vec![before, after],
                EffectState::Conflicted => Vec::new(),
            };
            if effect.services.is_empty()
                || effect.boot.is_empty()
                || entries
                    .into_iter()
                    .chain([&effect.original])
                    .flatten()
                    .any(|entry| entry.source() != source)
                || effects.insert(effect.key, effect).is_some()
            {
                return Err(PeripheralError::JournalFailed(
                    "invalid or duplicate recovery obligation".into(),
                ));
            }
        }
        Ok(Self {
            backend,
            journal,
            effects,
        })
    }

    /// Recovery obligations, including effects whose descriptor is no longer installed.
    pub fn effects(&self) -> impl Iterator<Item = EffectKey> + '_ {
        self.effects.keys().copied()
    }

    /// Conflicts owned by one saved rule, for an explicit user resolution.
    pub fn owned_effects(&self, owner: &str) -> Vec<EffectKey> {
        self.effects
            .values()
            .filter(|effect| effect.owner == owner)
            .map(|effect| effect.key)
            .collect()
    }

    /// Relinquish a conflicted effect only after explicit user resolution.
    pub fn resolve_conflict(&mut self, key: EffectKey) -> Result<(), PeripheralError> {
        if self
            .effects
            .get(&key)
            .is_some_and(|effect| effect.state == EffectState::Conflicted)
        {
            self.commit(key, None)?;
        }
        Ok(())
    }

    /// Apply a target or conditionally restore this source's original entry.
    pub async fn reconcile(
        &mut self,
        key: EffectKey,
        owner: &str,
        desired: Option<MappingUsage>,
        check: impl Fn() -> Result<(), PeripheralError>,
    ) -> Result<ApplicationStatus, PeripheralError> {
        if desired.is_none() && !self.effects.contains_key(&key) {
            return Ok(ApplicationStatus::Disabled);
        }
        check()?;
        let evidence = match self.backend.read(key.scope).await {
            Err(PeripheralError::Offline) if desired.is_none() => {
                return Ok(ApplicationStatus::RestorePending);
            }
            result => result?,
        };
        check()?;
        self.retire_expired(key, &evidence)?;
        if desired.is_none() && !self.effects.contains_key(&key) {
            return Ok(ApplicationStatus::Restored);
        }
        let current = evidence.common()?.entry(key.source)?.cloned();
        let mut effect = self
            .effects
            .get(&key)
            .cloned()
            .unwrap_or_else(|| MappingEffect {
                key,
                owner: owner.into(),
                original: current.clone(),
                boot: evidence.boot.clone(),
                services: evidence.services.keys().copied().collect(),
                state: EffectState::Stable(current.clone()),
            });
        if desired.is_some() && effect.owner != owner {
            return Err(PeripheralError::DriverConflict(format!(
                "mapping source is owned by {}",
                effect.owner
            )));
        }
        let expected = match &effect.state {
            EffectState::Stable(expected) => expected,
            EffectState::Prepared { before, .. } if &current == before => before,
            EffectState::Prepared { after, .. } if &current == after => after,
            EffectState::Prepared { .. } | EffectState::Conflicted => {
                return self.conflict(effect);
            }
        };
        if &current != expected {
            return self.conflict(effect);
        }
        let after = match desired {
            Some(destination) => Some(current.as_ref().map_or_else(
                || MappingEntry::new(key.source, destination),
                |entry| entry.with_destination(destination),
            )),
            None => effect.original.clone(),
        };
        if current == after {
            if desired.is_some() {
                effect.state = EffectState::Stable(after);
                self.commit(key, Some(effect))?;
                return Ok(ApplicationStatus::Applied);
            }
            self.commit(key, None)?;
            return Ok(ApplicationStatus::Restored);
        }
        effect.state = EffectState::Prepared {
            before: current.clone(),
            after: after.clone(),
        };
        self.commit(key, Some(effect.clone()))?;

        // Journal I/O can race an external editor. Merge from a fresh array so
        // unrelated edits made during the journal commit survive the setter.
        let latest = self.backend.read(key.scope).await?;
        check()?;
        if latest.boot != evidence.boot || latest.services.keys().ne(evidence.services.keys()) {
            return Err(PeripheralError::StaleSession);
        }
        let mappings = latest.common()?;
        if mappings.entry(key.source)?.cloned() != current {
            return self.conflict(effect);
        }
        let mappings = mappings.replacing(key.source, after.clone())?;
        check()?;
        self.backend.write(key.scope, &mappings).await?;
        let readback = self.backend.read(key.scope).await?;
        check()?;
        if readback.boot != evidence.boot || readback.services.keys().ne(evidence.services.keys()) {
            return Err(PeripheralError::StaleSession);
        }
        if readback.common()?.entry(key.source)?.cloned() != after {
            return Err(PeripheralError::ReadbackMismatch);
        }
        if desired.is_some() {
            effect.state = EffectState::Stable(after);
            self.commit(key, Some(effect))?;
            Ok(ApplicationStatus::Applied)
        } else {
            self.commit(key, None)?;
            Ok(ApplicationStatus::Restored)
        }
    }

    fn conflict(
        &mut self,
        mut effect: MappingEffect,
    ) -> Result<ApplicationStatus, PeripheralError> {
        effect.state = EffectState::Conflicted;
        self.commit(effect.key, Some(effect))?;
        Err(PeripheralError::ExternalModification)
    }

    fn retire_expired(
        &mut self,
        key: EffectKey,
        evidence: &ServiceEvidence,
    ) -> Result<(), PeripheralError> {
        let Some(effect) = self.effects.get(&key) else {
            return Ok(());
        };
        let current: BTreeSet<_> = evidence.services.keys().copied().collect();
        if evidence.boot != effect.boot || current.is_disjoint(&effect.services) {
            // The old OS service lifetime ended. Never transfer its baseline
            // to a new receiver merely because the VID/PID still matches.
            return self.commit(key, None);
        }
        if !current.is_subset(&effect.services) {
            return Err(PeripheralError::MappingScopeConflict);
        }
        if current != effect.services {
            let mut effect = effect.clone();
            effect.services = current;
            self.commit(key, Some(effect))?;
        }
        Ok(())
    }

    fn commit(
        &mut self,
        key: EffectKey,
        effect: Option<MappingEffect>,
    ) -> Result<(), PeripheralError> {
        if self.effects.get(&key) == effect.as_ref() {
            return Ok(());
        }
        let mut next = self.effects.clone();
        match effect {
            Some(effect) => {
                next.insert(key, effect);
            }
            None => {
                next.remove(&key);
            }
        }
        self.journal
            .save(&next.values().cloned().collect::<Vec<_>>())?;
        self.effects = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
