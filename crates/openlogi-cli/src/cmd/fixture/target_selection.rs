//! Strict fixture target selection with privacy-safe diagnostics.

use std::fmt::Write as _;

use anyhow::{Result, anyhow, bail};
use openlogi_core::hid::DeviceRoute;

pub(super) trait FixtureTarget: Clone {
    fn route(&self) -> &DeviceRoute;
    fn display_name(&self) -> &str;
}

pub(super) fn select_target<T: FixtureTarget>(candidates: &[T], query: Option<&str>) -> Result<T> {
    if candidates.is_empty() {
        bail!("no addressable physical device candidate was found");
    }

    let mut matches = match query {
        Some(query) => candidates
            .iter()
            .filter(|candidate| {
                candidate.display_name().eq_ignore_ascii_case(query)
                    || candidate.route().to_string() == query
            })
            .cloned()
            .collect::<Vec<_>>(),
        None if candidates.len() == 1 => candidates.to_owned(),
        None => Vec::new(),
    };
    if matches.len() != 1 {
        let message = match (query, matches.len()) {
            (Some(_), 0) => "no candidate exactly matched --device",
            (Some(_), _) => "--device matched more than one candidate",
            (None, _) => "more than one physical candidate is available; pass --device",
        };
        return Err(selection_error(message, candidates));
    }

    let selected = matches.remove(0);
    if candidates
        .iter()
        .filter(|candidate| candidate.route() == selected.route())
        .count()
        != 1
    {
        return Err(selection_error(
            "the selected route is duplicated and cannot identify one target",
            candidates,
        ));
    }
    Ok(selected)
}

fn selection_error<T: FixtureTarget>(message: &str, candidates: &[T]) -> anyhow::Error {
    let mut list = String::new();
    for candidate in candidates {
        let _ = write!(
            list,
            "\n  - {:?} ({})",
            candidate.display_name(),
            safe_route_label(candidate.route())
        );
    }
    anyhow!("{message}. Candidates:{list}")
}

fn safe_route_label(route: &DeviceRoute) -> String {
    match route {
        DeviceRoute::Bolt { slot, .. } => format!("Bolt receiver slot {slot}"),
        DeviceRoute::Unifying { slot, .. } => format!("Unifying receiver slot {slot}"),
        DeviceRoute::Direct {
            vendor_id,
            product_id,
        } => format!("direct {vendor_id:04x}:{product_id:04x}"),
        DeviceRoute::RawHid {
            vendor_id,
            product_id,
            usage_page,
            usage_id,
            ..
        } => format!(
            "standalone {vendor_id:04x}:{product_id:04x} usage {usage_page:04x}:{usage_id:04x}"
        ),
    }
}
