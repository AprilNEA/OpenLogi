use std::path::PathBuf;

use openlogi_core::peripheral::PeripheralError;
use serde::{Deserialize, Serialize};

use super::{JournalStore, MappingEffect};
use crate::peripherals::storage::StateFile;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    effects: Vec<MappingEffect>,
}

/// Agent-owned journal, separate from user configuration and plugin packages.
pub struct FileJournal(StateFile);

impl FileJournal {
    /// Open a journal at an explicit path. No hardware access occurs here.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self(StateFile::new(path))
    }
}

impl JournalStore for FileJournal {
    fn load(&mut self) -> Result<Vec<MappingEffect>, PeripheralError> {
        let Some(document) = self.0.load::<Document>()? else {
            return Ok(Vec::new());
        };
        if document.version != 1 {
            return Err(PeripheralError::JournalFailed(
                "unsupported journal version".into(),
            ));
        }
        Ok(document.effects)
    }

    fn save(&mut self, effects: &[MappingEffect]) -> Result<(), PeripheralError> {
        self.0.save(&Document {
            version: 1,
            effects: effects.to_vec(),
        })
    }
}
