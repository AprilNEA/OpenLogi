//! Agent-owned onboard editor, durable backups, and competing-writer checks.
use openlogi_device::{
    SharedChannel,
    gaming::{
        self, GamingBackup, ProfileEdit,
        profile::{ButtonAction, ButtonPatch, Layer, ProfilePatch},
    },
};
use openlogi_ipc::gaming::{
    GamingAction, GamingCommand, GamingDraft, GamingProfile, GamingSnapshot,
};
use std::io::Write as _;

fn make_edit(backup_json: &str, draft: GamingDraft) -> Result<ProfileEdit, String> {
    if backup_json.len() > 1_000_000 {
        return Err("Backup exceeds size limit".into());
    }
    let original: GamingBackup = serde_json::from_str(backup_json).map_err(|e| e.to_string())?;
    let before = original
        .profiles()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|p| p.sector == draft.sector)
        .ok_or("Profile is not present in the captured directory")?;
    let before_rate = 1000u16
        .checked_div(u16::from(before.report_interval_ms))
        .unwrap_or(0);
    let edit = ProfileEdit {
        schema_version: 1,
        original,
        sector: draft.sector,
        patch: ProfilePatch {
            // Untouched semantic fields must retain their original encoding,
            // including erased/padded names and unknown future metadata.
            name: (draft.name != before.name).then_some(draft.name),
            report_rate_hz: (draft.report_rate_hz != before_rate).then_some(draft.report_rate_hz),
            dpi: (draft.dpi != before.dpi).then_some(draft.dpi),
            default_dpi_slot: (draft.default_dpi_slot != before.default_dpi_slot)
                .then_some(draft.default_dpi_slot),
            shift_dpi_slot: (draft.shift_dpi_slot != before.shift_dpi_slot)
                .then_some(draft.shift_dpi_slot),
            buttons: draft
                .buttons
                .into_iter()
                .map(|b| ButtonPatch {
                    index: b.index,
                    layer: if b.shifted {
                        Layer::Shifted
                    } else {
                        Layer::Normal
                    },
                    action: match b.action {
                        GamingAction::Mouse(button) => ButtonAction::Mouse { button },
                        GamingAction::Key { usage, modifiers } => {
                            ButtonAction::Key { usage, modifiers }
                        }
                        GamingAction::Consumer(usage) => ButtonAction::Consumer { usage },
                        GamingAction::Special(code) => ButtonAction::Special { code },
                        GamingAction::Disabled => ButtonAction::Disabled,
                    },
                })
                .collect(),
        },
    };
    edit.render().map_err(|e| e.to_string())?;
    Ok(edit)
}

fn save_json(value: &impl serde::Serialize, kind: &str) -> Result<String, String> {
    let dir = openlogi_core::paths::config_dir()
        .map_err(|e| e.to_string())?
        .join("gaming-backups");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let path = dir.join(format!("g502x-{kind}-{stamp}.json"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Executed under the agent's device lease; the task survives IPC cancellation.
pub async fn execute(
    channel: &SharedChannel,
    command: GamingCommand,
) -> Result<GamingSnapshot, String> {
    if channel.route()
        != &(openlogi_core::hid::DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id: 0xc098,
        })
    {
        return Err("This build supports G502 X LIGHTSPEED over USB cable (046D:C098)".into());
    }
    let mut saved_path = None;
    match command {
        GamingCommand::Read => {}
        GamingCommand::Export => {
            let backup = gaming::backup_on(channel)
                .await
                .map_err(|e| e.to_string())?;
            backup.profiles().map_err(|e| e.to_string())?;
            saved_path = Some(save_json(&backup, "backup")?);
        }
        GamingCommand::Prepare { backup_json, draft } => {
            let edit = make_edit(&backup_json, draft)?;
            if !channel.matches(&edit.original.route) {
                return Err("Backup device mismatch".into());
            }
            saved_path = Some(save_json(&edit, "prepared")?);
        }
        GamingCommand::Apply { backup_json, draft } => {
            openlogi_hid::gaming_guard::ensure_exclusive(
                openlogi_hid::gaming_guard::Writer::Agent,
            )?;
            let edit = make_edit(&backup_json, draft)?;
            if !channel.matches(&edit.original.route) {
                return Err("Backup device mismatch".into());
            }
            // Flush the full original and requested patch before the first flash write.
            let path = save_json(&edit, "before-write")?;
            gaming::apply_on(channel, &edit)
                .await
                .map_err(|e| format!("{e}; backup: {path}"))?;
            saved_path = Some(path);
        }
        GamingCommand::SetMode(mode) => {
            openlogi_hid::gaming_guard::ensure_exclusive(
                openlogi_hid::gaming_guard::Writer::Agent,
            )?;
            gaming::set_mode_on(channel, mode)
                .await
                .map_err(|e| e.to_string())?;
        }
        GamingCommand::Select(sector) => {
            openlogi_hid::gaming_guard::ensure_exclusive(
                openlogi_hid::gaming_guard::Writer::Agent,
            )?;
            gaming::select_on(channel, sector)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    let backup = gaming::backup_on(channel)
        .await
        .map_err(|e| e.to_string())?;
    let profiles = backup
        .profiles()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|p| GamingProfile {
            sector: p.sector,
            enabled: p.enabled,
            checksum_valid: p.checksum_valid,
            name: p.name,
            report_interval_ms: p.report_interval_ms,
            dpi: p.dpi,
            default_dpi_slot: p.default_dpi_slot,
            shift_dpi_slot: p.shift_dpi_slot,
            buttons: p.buttons.into_iter().map(|b| b.description).collect(),
            shifted_buttons: p
                .shifted_buttons
                .into_iter()
                .map(|b| b.description)
                .collect(),
        })
        .collect();
    Ok(GamingSnapshot {
        mode: backup.mode,
        active_profile: backup.active_profile,
        backup_json: serde_json::to_string(&backup).map_err(|e| e.to_string())?,
        profiles,
        saved_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn original() -> GamingBackup {
        let mut sectors = vec![vec![0xff; 255]; 2];
        sectors[0][..4].copy_from_slice(&[0, 1, 1, 0]);
        sectors[1][0] = 1;
        sectors[1][1] = 0;
        sectors[1][2] = 0;
        sectors[1][3..5].copy_from_slice(&800u16.to_le_bytes());
        sectors[1][5..13].fill(0);
        sectors[1][32..36].copy_from_slice(&[0x80, 1, 0, 1]);
        for sector in &mut sectors {
            let crc = gaming::profile::crc16(&sector[..253]);
            sector[253..].copy_from_slice(&crc.to_be_bytes());
        }
        GamingBackup {
            schema_version: 1,
            route: openlogi_core::hid::DeviceRoute::Direct {
                vendor_id: 0x046d,
                product_id: 0xc098,
            },
            memory_model: 1,
            profile_format: 3,
            macro_format: 1,
            profile_count: 1,
            button_count: 11,
            sector_size: 255,
            mode: 1,
            active_profile: 1,
            sectors,
        }
    }
    fn draft() -> GamingDraft {
        GamingDraft {
            sector: 1,
            name: "Work".into(),
            report_rate_hz: 500,
            dpi: [800, 1600, 0, 0, 0],
            default_dpi_slot: 0,
            shift_dpi_slot: 1,
            buttons: vec![openlogi_ipc::gaming::GamingAssignment {
                index: 4,
                shifted: true,
                action: GamingAction::Key {
                    usage: 75,
                    modifiers: 0,
                },
            }],
        }
    }
    #[test]
    fn gui_draft_preserves_original_and_unknown_bytes() {
        let original = original();
        let edit = make_edit(&serde_json::to_string(&original).unwrap(), draft()).unwrap();
        let patched = edit.render().unwrap();
        assert_eq!(&patched[112..116], &[0x80, 2, 0, 75]);
        assert_eq!(&patched[32..96], &original.sectors[1][32..96]);
        assert_eq!(&patched[208..253], &original.sectors[1][208..253]);
        assert_eq!(edit.original.sectors, original.sectors);
        assert!(gaming::profile::checksum_valid(&patched));
    }
    #[test]
    fn gui_draft_rejects_invalid_values_and_last_left_click_removal() {
        let backup = serde_json::to_string(&original()).unwrap();
        let mut invalid = draft();
        invalid.dpi[0] = 801;
        make_edit(&backup, invalid).unwrap_err();
        let mut invalid = draft();
        invalid.buttons = vec![openlogi_ipc::gaming::GamingAssignment {
            index: 0,
            shifted: false,
            action: GamingAction::Disabled,
        }];
        make_edit(&backup, invalid).unwrap_err();
        make_edit("{}", draft()).unwrap_err();
    }

    #[test]
    fn unchanged_gui_values_preserve_erased_name_encoding_without_flash_changes() {
        let original = original();
        let unchanged = GamingDraft {
            sector: 1,
            name: String::new(),
            report_rate_hz: 1000,
            dpi: [800, 0, 0, 0, 0],
            default_dpi_slot: 0,
            shift_dpi_slot: 0,
            buttons: Vec::new(),
        };
        let edit = make_edit(&serde_json::to_string(&original).unwrap(), unchanged).unwrap();
        assert_eq!(edit.render().unwrap(), original.sectors[1]);
        assert!(edit.patch.name.is_none());
    }
}
