//! What the agent last brought each onboard profile of a flash mouse to.
//!
//! A profile's flash is written only when its OpenLogi configuration changed
//! since the agent last applied it: a profile switch, a reconnect or an agent
//! start rewrites nothing, so a profile another tool shaped keeps its shape
//! until the user edits it here. Telling "changed" from "as applied" across
//! agent restarts needs a record of what was applied, and so does telling the
//! agent buttons the agent itself gave a profile's slots from ones another
//! tool set on purpose, which only the former may be taken back. This module
//! keeps that record and its file; [`super::FlashQueue`] consults it.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use openlogi_core::binding::{
    ButtonId, FlashSync, G700, MouseButtonNumber, ProfileRequest, SlotIntent,
};
use serde::{Deserialize, Serialize};

/// What the agent last wrote one profile for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedProfile {
    /// The profile's configuration, as intents, when it was applied.
    pub intents: Vec<SlotIntent>,
    /// The agent buttons the write gave the profile's agent-run slots.
    pub routed: BTreeMap<MouseButtonNumber, ButtonId>,
}

/// The record of every flash mouse, by its configuration entry and then by
/// the flash page of each profile.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlashRecords {
    devices: BTreeMap<String, BTreeMap<u8, AppliedProfile>>,
}

impl FlashRecords {
    /// Read the record at `path`; an absent file is an empty record.
    ///
    /// # Errors
    /// The file exists but does not read or parse.
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    /// Write the record to `path`, whole or not at all.
    ///
    /// # Errors
    /// The directory or the file cannot be written.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        let staged = path.with_extension("json.tmp");
        std::fs::write(&staged, bytes)?;
        std::fs::rename(&staged, path)
    }

    /// What `page` of `owner`'s mouse was last applied as.
    #[must_use]
    pub fn applied(&self, owner: &str, page: u8) -> Option<&AppliedProfile> {
        self.devices.get(owner)?.get(&page)
    }

    /// The request for each profile of `owner`'s mouse, from what its
    /// configuration asks (`asked`, by page): written where that differs
    /// from what was applied ([`FlashSync::Write`]), only observed where it
    /// does not. A profile applied before whose configuration is gone asks
    /// to keep every slot, which releases the agent buttons it was given.
    #[must_use]
    pub fn requests(
        &self,
        owner: &str,
        asked: &BTreeMap<u8, Vec<SlotIntent>>,
    ) -> BTreeMap<u8, ProfileRequest> {
        let applied = self.devices.get(owner);
        let pages = asked
            .keys()
            .chain(applied.into_iter().flat_map(BTreeMap::keys))
            .copied();
        pages
            .map(|page| {
                let intents = asked
                    .get(&page)
                    .cloned()
                    .unwrap_or_else(|| vec![SlotIntent::Keep; G700.slots.len()]);
                let record = applied.and_then(|profiles| profiles.get(&page));
                let changed = match record {
                    Some(record) => record.intents != intents,
                    None => intents.iter().any(|intent| *intent != SlotIntent::Keep),
                };
                let request = ProfileRequest {
                    owned: record
                        .map(|record| record.routed.clone())
                        .unwrap_or_default(),
                    sync: if changed {
                        FlashSync::Write
                    } else {
                        FlashSync::Observe
                    },
                    intents,
                };
                (page, request)
            })
            .collect()
    }

    /// Note that `page` of `owner`'s mouse now holds what `intents` asked,
    /// its agent-run slots on `routed`. A profile left asking nothing and
    /// holding no agent button is forgotten. Returns whether the record
    /// changed.
    pub fn apply(
        &mut self,
        owner: &str,
        page: u8,
        intents: Vec<SlotIntent>,
        routed: BTreeMap<MouseButtonNumber, ButtonId>,
    ) -> bool {
        let empty = routed.is_empty() && intents.iter().all(|i| *i == SlotIntent::Keep);
        let profiles = self.devices.entry(owner.to_owned()).or_default();
        let changed = if empty {
            profiles.remove(&page).is_some()
        } else {
            let record = AppliedProfile { intents, routed };
            profiles.insert(page, record.clone()) != Some(record)
        };
        if profiles.is_empty() {
            self.devices.remove(owner);
        }
        changed
    }
}
