//! SKUs the published asset index lists under a different PID or name.
//!
//! Logitech's Options+ payloads often group hardware revisions into one
//! depot (G502 Proteus Core / Spectrum / Hero share `g502_core`) and key
//! the per-SKU renders on a **name** (`g502_spectrum`, `g513_ext5`) rather
//! than the hex PID the index uses (`c07d`, `c33c`). The published
//! `modelIds` list frequently only carries the first SKU, so a live HID++
//! PID would otherwise miss the depot entirely.

use crate::manifest::variant_model_id;

/// Depot the published index should have listed `pid` on, when it only
/// names an earlier SKU of the same Options+ payload.
///
/// G502 Proteus Spectrum (`0xc332`) and Hero (`0xc08b`) share the
/// `g502_core` depot, whose index `modelIds` only contain Proteus Core
/// (`c07d`).
#[must_use]
pub fn shared_depot_for_pid(pid: u16) -> Option<&'static str> {
    match pid {
        0xc332 | 0xc08b => Some("g502_core"),
        _ => None,
    }
}

/// Manifest `modelId` for a named (non-`_extN`) SKU variant.
///
/// Most colourways use [`variant_model_id`] (`{base}_ext{N}`). G502
/// Spectrum / Hero use a sibling entry (`g502_spectrum`) instead of
/// `c07d_ext1`.
#[must_use]
pub fn named_variant_for_pid(pid: u16) -> Option<&'static str> {
    match pid {
        0xc332 | 0xc08b => Some("g502_spectrum"),
        _ => None,
    }
}

/// Manifest `modelId` keys to try for this device, most-specific first.
///
/// Order:
/// 1. A named SKU sibling ([`named_variant_for_pid`]) when the live PID
///    is not the depot's primary.
/// 2. `{depot}_ext{N}` / `{depot}` — G-series manifests key colourways
///    on the depot name (`g513_ext5`), not the hex PID (`c33c_ext5`).
/// 3. `{index_id}_ext{N}` / `{index_id}` — MX-class depots key on the
///    bolt PID the index lists.
#[must_use]
pub fn variant_lookup_ids(
    depot: &str,
    index_ids: &[&str],
    hidpp_pids: &[u16],
    ext: u8,
) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let mut push = |key: String| {
        if !keys
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&key))
        {
            keys.push(key);
        }
    };
    for pid in hidpp_pids.iter().copied().filter(|&pid| pid != 0) {
        if let Some(named) = named_variant_for_pid(pid) {
            push(named.to_string());
        }
    }
    push(variant_model_id(depot, ext));
    for id in index_ids {
        push(variant_model_id(id, ext));
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectrum_and_hero_share_the_core_depot() {
        assert_eq!(shared_depot_for_pid(0xc332), Some("g502_core"));
        assert_eq!(shared_depot_for_pid(0xc08b), Some("g502_core"));
        assert_eq!(shared_depot_for_pid(0xc07d), None);
        assert_eq!(shared_depot_for_pid(0xc33c), None);
    }

    #[test]
    fn spectrum_pid_prefers_the_named_manifest_entry() {
        let keys = variant_lookup_ids("g502_core", &["c07d"], &[0xc332], 0);
        assert_eq!(keys, ["g502_spectrum", "g502_core", "c07d"]);
    }

    #[test]
    fn core_pid_uses_the_depot_name_not_a_named_sibling() {
        let keys = variant_lookup_ids("g502_core", &["c07d"], &[0xc07d], 0);
        assert_eq!(keys, ["g502_core", "c07d"]);
    }

    #[test]
    fn g513_colourway_keys_on_the_depot_name() {
        // Live G513 reports ext=05; the manifest entry is `g513_ext5`,
        // not `c33c_ext5`.
        let keys = variant_lookup_ids("g513", &["c33c"], &[0xc33c], 5);
        assert_eq!(keys, ["g513_ext5", "c33c_ext5"]);
    }
}
