//! Process-wide last-known sensor DPI, keyed by [`DeviceRoute`].
//!
//! HID++ `getSensorDpi` is the live value hold-mode uses to size its
//! millimetre deadzone. The read is async; capture-plan derivation is not.
//! Every successful DPI read or write updates this cache so the next plan
//! rebuild sees the device rather than an empty preset list.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::channel::route::DeviceRoute;

use super::Dpi;

static CACHE: OnceLock<Mutex<HashMap<String, Dpi>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, Dpi>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(route: &DeviceRoute) -> String {
    match route {
        DeviceRoute::Bolt { receiver_uid, slot } => {
            format!("bolt:{}:{slot}", receiver_uid.to_ascii_lowercase())
        }
        DeviceRoute::Unifying { receiver_uid, slot } => {
            format!("unifying:{}:{slot}", receiver_uid.to_ascii_lowercase())
        }
        DeviceRoute::Direct {
            vendor_id,
            product_id,
        } => format!("direct:{vendor_id:04x}:{product_id:04x}"),
        DeviceRoute::RawHid { identity, .. } => format!("raw:{identity}"),
    }
}

/// Last successful `getSensorDpi` / DPI write for `route`, if any.
#[must_use]
pub fn cached_sensor_dpi(route: &DeviceRoute) -> Option<Dpi> {
    cache().lock().ok()?.get(&cache_key(route)).copied()
}

/// Record a live sensor reading (or a write we just confirmed) for `route`.
pub fn remember_sensor_dpi(route: &DeviceRoute, dpi: Dpi) {
    if let Ok(mut guard) = cache().lock() {
        guard.insert(cache_key(route), dpi);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(product_id: u16) -> DeviceRoute {
        DeviceRoute::Direct {
            vendor_id: 0x046d,
            product_id,
        }
    }

    #[test]
    fn remember_then_lookup_is_per_route() {
        let a = route(0x0101);
        let b = route(0x0102);
        remember_sensor_dpi(&a, Dpi::new(800));
        remember_sensor_dpi(&b, Dpi::new(1600));
        assert_eq!(cached_sensor_dpi(&a), Some(Dpi::new(800)));
        assert_eq!(cached_sensor_dpi(&b), Some(Dpi::new(1600)));
        remember_sensor_dpi(&a, Dpi::new(1200));
        assert_eq!(
            cached_sensor_dpi(&a),
            Some(Dpi::new(1200)),
            "a later write must replace the cached reading"
        );
        assert_eq!(
            cached_sensor_dpi(&b),
            Some(Dpi::new(1600)),
            "updating one route must not clobber another"
        );
    }

    #[test]
    fn bolt_and_unifying_slots_do_not_share_an_entry() {
        let bolt = DeviceRoute::Bolt {
            receiver_uid: "Cafe".into(),
            slot: 2,
        };
        let unifying = DeviceRoute::Unifying {
            receiver_uid: "cafe".into(),
            slot: 2,
        };
        remember_sensor_dpi(&bolt, Dpi::new(400));
        remember_sensor_dpi(&unifying, Dpi::new(2000));
        assert_eq!(cached_sensor_dpi(&bolt), Some(Dpi::new(400)));
        assert_eq!(cached_sensor_dpi(&unifying), Some(Dpi::new(2000)));
    }
}
