//! Deserialization of [`ButtonId`]-keyed tables that rejects one control
//! named twice.
//!
//! A control has more than one spelling — `KeyScreenCapture` and
//! `control:0x010a` load as the same [`ButtonId`] — so TOML's own duplicate-key
//! check cannot see the collision, and a plain map's last-wins insert would
//! drop one binding silently, then persist the loss on the next save.

use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

use serde::de::{self, Deserialize, Deserializer, MapAccess, Visitor};

use crate::binding::ButtonId;

/// Deserialize a `ButtonId`-keyed table, failing on a control that appears
/// under two spellings.
pub(super) fn deserialize_button_map<'de, D, V>(
    deserializer: D,
) -> Result<BTreeMap<ButtonId, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    deserializer.deserialize_map(ButtonMapVisitor(PhantomData))
}

/// Deserialize the per-app overlays — a table of `ButtonId`-keyed tables —
/// with the same guard on every inner table.
pub(super) fn deserialize_button_maps_by_app<'de, D, V>(
    deserializer: D,
) -> Result<BTreeMap<String, BTreeMap<ButtonId, V>>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    let maps = BTreeMap::<String, ButtonMap<V>>::deserialize(deserializer)?;
    Ok(maps.into_iter().map(|(app, map)| (app, map.0)).collect())
}

struct ButtonMap<V>(BTreeMap<ButtonId, V>);

impl<'de, V: Deserialize<'de>> Deserialize<'de> for ButtonMap<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserialize_button_map(deserializer).map(Self)
    }
}

struct ButtonMapVisitor<V>(PhantomData<V>);

impl<'de, V: Deserialize<'de>> Visitor<'de> for ButtonMapVisitor<V> {
    type Value = BTreeMap<ButtonId, V>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a table keyed by button name")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
        let mut map = BTreeMap::new();
        let mut spellings: BTreeMap<ButtonId, String> = BTreeMap::new();
        while let Some(name) = access.next_key::<String>()? {
            let button: ButtonId = name.parse().map_err(de::Error::custom)?;
            let value = access.next_value::<V>()?;
            if let Some(earlier) = spellings.insert(button, name.clone()) {
                return Err(de::Error::custom(format_args!(
                    "`{earlier}` and `{name}` name the same control; keep one"
                )));
            }
            map.insert(button, value);
        }
        Ok(map)
    }
}
