//! Validation and application of linked host transitions.

use std::sync::Arc;

use hidpp::{
    channel::HidppChannel,
    device::Device,
    feature::{CreatableFeature, change_host::ChangeHostFeature},
};
use tracing::debug;

use crate::{ChannelPool, DeviceRoute};

use super::slots::{ReportedHostSlot, reported_host_slot};
use super::{HostSwitchError, KeyboardHostTransition, open_channel, timed_device, timed_hidpp};
/// Move reachable targets to `host`, then move the keyboard last.
///
/// Returns whether the keyboard actually changed hosts, or firmware announced
/// that it had already begun departing. A ChangeHost announcement carries the
/// destination selected by the keyboard itself, so followers use their own
/// channels immediately instead of waiting for an impossible keyboard reopen.
/// Analytics-only events still require destination revalidation.
pub async fn switch_linked_hosts(
    keyboard: &DeviceRoute,
    targets: &[DeviceRoute],
    host: u8,
    keyboard_transition: KeyboardHostTransition,
    channel_pool: &ChannelPool,
) -> Result<bool, HostSwitchError> {
    let analytics_host_slot = match keyboard_transition {
        KeyboardHostTransition::AlreadyDeparting { host_slot } => {
            if host_slot == ReportedHostSlot::Empty {
                return Err(HostSwitchError::HostSlotEmpty { host });
            }
            debug!(
                route = %keyboard,
                host,
                ?host_slot,
                "keyboard departure announced; switching targets immediately"
            );
            switch_targets_directly(targets, host, channel_pool).await;
            return Ok(true);
        }
        KeyboardHostTransition::AnalyticsEvent { host_slot } => Some(host_slot),
        KeyboardHostTransition::CommandRequired => None,
    };
    let channel = match open_channel(channel_pool, keyboard, "opening keyboard channel").await {
        Ok(Some(channel)) => channel,
        Ok(None) => {
            return unreachable_transition_error(
                analytics_host_slot,
                host,
                HostSwitchError::KeyboardNotFound,
            );
        }
        Err(error) if error.is_device_unreachable() => {
            return unreachable_transition_error(analytics_host_slot, host, error);
        }
        Err(error) => return Err(error),
    };
    // Validate the keyboard's own move before touching anything: preparation is
    // read-only, but it is the step that rejects an unpaired host slot, and
    // discovering that *after* the mice have moved would strand them on a host
    // the keyboard never reaches. Applying it still happens last, because once
    // the keyboard leaves this host its channel can no longer command a mouse
    // sharing the same receiver.
    let keyboard_change = if analytics_host_slot.is_some() {
        prepare_announced_host_change_on(&channel, keyboard.device_index(), host).await
    } else {
        prepare_host_change_on(&channel, keyboard.device_index(), host).await
    };
    let keyboard_change = match keyboard_change {
        Ok(change) => change,
        Err(error) if error.is_device_unreachable() => {
            return unreachable_transition_error(analytics_host_slot, host, error);
        }
        Err(error) => return Err(error),
    };
    for target in targets {
        match prepare_host_change(target, host, keyboard, &channel, channel_pool).await {
            Ok(change) => {
                if let Err(error) = apply_host_change(change).await {
                    debug!(%error, route = %target, host, "linked device host switch failed");
                }
            }
            Err(error) => {
                debug!(%error, route = %target, host, "linked device host switch preparation failed");
            }
        }
    }
    let changed = apply_host_change(keyboard_change).await?;
    if changed {
        debug!(host, route = %keyboard, "keyboard host switched");
    }
    Ok(changed)
}

/// Switch targets through their own channels after the initiating keyboard
/// has announced that its firmware is already leaving this host.
async fn switch_targets_directly(targets: &[DeviceRoute], host: u8, channel_pool: &ChannelPool) {
    for target in targets {
        let channel = match open_channel(channel_pool, target, "opening target channel").await {
            Ok(Some(channel)) => channel,
            Ok(None) => {
                debug!(route = %target, host, "target not reachable for direct switch");
                continue;
            }
            Err(error) => {
                debug!(%error, route = %target, host, "target channel open failed");
                continue;
            }
        };
        match prepare_host_change_on(&channel, target.device_index(), host).await {
            Ok(change) => {
                if let Err(error) = apply_host_change(change).await {
                    debug!(%error, route = %target, host, "direct target host switch failed");
                }
            }
            Err(error) => {
                debug!(%error, route = %target, host, "direct target host switch preparation failed");
            }
        }
    }
}

fn unreachable_transition_error(
    host_slot: Option<ReportedHostSlot>,
    host: u8,
    unreachable: HostSwitchError,
) -> Result<bool, HostSwitchError> {
    match host_slot {
        None => Err(unreachable),
        Some(ReportedHostSlot::Empty) => Err(HostSwitchError::HostSlotEmpty { host }),
        Some(ReportedHostSlot::Unknown | ReportedHostSlot::Paired) => {
            Err(HostSwitchError::HostSlotUnverified { host })
        }
    }
}

pub(super) struct PreparedHostChange {
    feature: Arc<ChangeHostFeature>,
    device_index: u8,
    host: u8,
    pub(super) required: bool,
}

enum HostSlotRequirement {
    Advisory,
    Paired,
}

async fn prepare_announced_host_change_on(
    channel: &Arc<HidppChannel>,
    device_index: u8,
    host: u8,
) -> Result<PreparedHostChange, HostSwitchError> {
    prepare_host_change_on_with_requirement(
        channel,
        device_index,
        host,
        HostSlotRequirement::Paired,
    )
    .await
}

async fn prepare_host_change(
    target: &DeviceRoute,
    host: u8,
    keyboard: &DeviceRoute,
    keyboard_channel: &Arc<HidppChannel>,
    channel_pool: &ChannelPool,
) -> Result<PreparedHostChange, HostSwitchError> {
    if shares_channel(target, keyboard) {
        prepare_host_change_on(keyboard_channel, target.device_index(), host).await
    } else {
        let channel = open_channel(channel_pool, target, "opening linked device channel")
            .await?
            .ok_or(HostSwitchError::TargetNotFound)?;
        prepare_host_change_on(&channel, target.device_index(), host).await
    }
}

pub(super) async fn prepare_host_change_on(
    channel: &Arc<HidppChannel>,
    device_index: u8,
    host: u8,
) -> Result<PreparedHostChange, HostSwitchError> {
    prepare_host_change_on_with_requirement(
        channel,
        device_index,
        host,
        HostSlotRequirement::Advisory,
    )
    .await
}

async fn prepare_host_change_on_with_requirement(
    channel: &Arc<HidppChannel>,
    device_index: u8,
    host: u8,
    slot_requirement: HostSlotRequirement,
) -> Result<PreparedHostChange, HostSwitchError> {
    let mut device = timed_device(
        "opening host-change device",
        Device::new(Arc::clone(channel), device_index),
    )
    .await?;
    let info = timed_hidpp(
        "locating host-change feature",
        device.root().get_feature(ChangeHostFeature::ID),
    )
    .await?
    .ok_or_else(|| HostSwitchError::Hidpp("ChangeHost is unsupported".into()))?;
    let change_host = device.add_feature::<ChangeHostFeature>(info.index);
    let state = timed_hidpp("reading current host", change_host.get_host_info()).await?;
    let required = host_change_required(state.current_host, state.host_count, host)?;
    if required {
        match (
            slot_requirement,
            reported_host_slot(&mut device, host).await,
        ) {
            (_, ReportedHostSlot::Empty) => {
                return Err(HostSwitchError::HostSlotEmpty { host });
            }
            (HostSlotRequirement::Paired, ReportedHostSlot::Unknown) => {
                return Err(HostSwitchError::HostSlotUnverified { host });
            }
            (
                HostSlotRequirement::Advisory,
                ReportedHostSlot::Unknown | ReportedHostSlot::Paired,
            )
            | (HostSlotRequirement::Paired, ReportedHostSlot::Paired) => {}
        }
    }
    Ok(PreparedHostChange {
        feature: change_host,
        device_index,
        host,
        required,
    })
}

async fn apply_host_change(change: PreparedHostChange) -> Result<bool, HostSwitchError> {
    if !change.required {
        let PreparedHostChange {
            device_index, host, ..
        } = change;
        debug!(device_index, host, "device already uses requested host");
        return Ok(false);
    }
    timed_hidpp(
        "writing current host",
        change.feature.set_current_host(change.host),
    )
    .await?;
    Ok(true)
}

pub(super) fn host_change_required(
    current_host: u8,
    host_count: u8,
    requested_host: u8,
) -> Result<bool, HostSwitchError> {
    if requested_host >= host_count {
        return Err(HostSwitchError::Hidpp(format!(
            "host {requested_host} is outside device host count {host_count}"
        )));
    }
    Ok(current_host != requested_host)
}

pub(super) fn shares_channel(left: &DeviceRoute, right: &DeviceRoute) -> bool {
    left.shares_transport(right)
}
