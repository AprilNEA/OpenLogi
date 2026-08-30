//! Agent-owned raw-touchpad and native-event trace capture.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use openlogi_core::config::Config;
use openlogi_core::device_order::{DeviceIdentity, DeviceStableId};
use openlogi_hid::DeviceRoute;
use openlogi_ipc::{
    AgentClient, AgentSnapshot, ClientKind, MonitorEvent, PROTOCOL_VERSION, TouchpadMonitorBatch,
    TouchpadMonitorEvent, TouchpadMonitorRecord, TouchpadRawModeConflict, client,
};
use serde_json::json;
use tarpc::context;
use tokio::time::{Instant, MissedTickBehavior};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const RPC_TIMEOUT: Duration = Duration::from_secs(2);
const INVENTORY_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Args)]
pub struct TouchpadArgs {
    /// Trace duration in seconds (1–300).
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=300))]
    pub seconds: u64,
    /// Match one raw-touchpad device by name or stable key; the match must be unique.
    #[arg(long)]
    pub device: Option<String>,
}

#[derive(Default)]
struct Summary {
    frames: u64,
    ends: u64,
    cancels: u64,
    logical_frame_drops: u64,
    buffer_drops: u64,
    native_events: u64,
}

impl Summary {
    fn observe_touchpad(&mut self, event: &TouchpadMonitorEvent) {
        match event {
            TouchpadMonitorEvent::Frame { .. } => self.frames += 1,
            TouchpadMonitorEvent::End => self.ends += 1,
            TouchpadMonitorEvent::Cancel => self.cancels += 1,
            TouchpadMonitorEvent::DroppedFrames { count } => {
                self.logical_frame_drops = self.logical_frame_drops.saturating_add(*count);
            }
        }
    }
}

/// Capture normalized `0x6100` records and simultaneous native button/scroll
/// events as JSON Lines. All HID ownership stays in the running Agent.
pub async fn run(args: TouchpadArgs) -> Result<()> {
    let connection = tokio::time::timeout(CONNECT_TIMEOUT, client::connect())
        .await
        .context("timed out connecting to the OpenLogi Agent")??;
    if connection.version != PROTOCOL_VERSION {
        bail!(
            "the agent speaks protocol v{}, but this CLI expects v{PROTOCOL_VERSION}",
            connection.version
        );
    }
    call(
        connection
            .client
            .declare_client(context::current(), ClientKind::Diagnostic),
        "declaring the diagnostic client",
    )
    .await?;

    let device_key = wait_for_device_key(&connection.client, args.device.as_deref()).await?;
    eprintln!(
        "capturing Agent-owned touchpad and native-event traces for {} s ({device_key})",
        args.seconds
    );

    let deadline = Instant::now() + Duration::from_secs(args.seconds);
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut summary = Summary::default();
    let mut conflicts = BTreeSet::new();
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            _ = ticker.tick() => {
                let batch = poll_touchpad(&connection.client, &device_key).await?;
                consume_batch(batch, &mut summary, &mut conflicts)?;
                for event in poll_native(&connection.client).await? {
                    emit_json(&json!({ "source": "native", "event": event }))?;
                    summary.native_events = summary.native_events.saturating_add(1);
                }
            }
        }
    }

    eprintln!(
        "summary: {} frames, {} ends, {} cancels, {} logical-frame drops, {} buffer drops, {} native events, {} raw-mode conflicts",
        summary.frames,
        summary.ends,
        summary.cancels,
        summary.logical_frame_drops,
        summary.buffer_drops,
        summary.native_events,
        conflicts.len(),
    );
    if summary.frames == 0 && conflicts.is_empty() {
        eprintln!(
            "note: no touchpad frames arrived; keep the device awake and confirm the Agent reports the 0x6100 capability"
        );
    }
    Ok(())
}

async fn call<T>(
    rpc: impl std::future::Future<Output = Result<T, tarpc::client::RpcError>>,
    operation: &'static str,
) -> Result<T> {
    tokio::time::timeout(RPC_TIMEOUT, rpc)
        .await
        .with_context(|| format!("timed out {operation}"))?
        .with_context(|| format!("agent disconnected while {operation}"))
}

async fn poll_touchpad(client: &AgentClient, device_key: &str) -> Result<TouchpadMonitorBatch> {
    call(
        client
            .clone()
            .poll_touchpad_monitor(context::current(), device_key.to_string()),
        "polling the touchpad monitor",
    )
    .await
}

async fn poll_native(client: &AgentClient) -> Result<Vec<MonitorEvent>> {
    call(
        client.clone().poll_event_monitor(context::current()),
        "polling the native-event monitor",
    )
    .await
}

fn consume_batch(
    batch: TouchpadMonitorBatch,
    summary: &mut Summary,
    seen_conflicts: &mut BTreeSet<(String, u8, u8)>,
) -> Result<()> {
    summary.buffer_drops = summary.buffer_drops.saturating_add(batch.dropped_events);
    for record in batch.events {
        summary.observe_touchpad(&record.event);
        emit_touchpad(&record)?;
    }
    for conflict in batch.conflicts {
        let identity = (
            conflict.device_key.clone(),
            conflict.expected,
            conflict.actual,
        );
        if seen_conflicts.insert(identity) {
            eprintln!(
                "raw-mode conflict on {}: expected {:#04x}, observed {:#04x}; the Agent will not overwrite it",
                conflict.device_key, conflict.expected, conflict.actual
            );
            emit_conflict(&conflict)?;
        }
    }
    Ok(())
}

fn emit_touchpad(record: &TouchpadMonitorRecord) -> Result<()> {
    emit_json(&json!({
        "source": "touchpad",
        "device_key": record.device_key,
        "event": record.event,
    }))
}

fn emit_conflict(conflict: &TouchpadRawModeConflict) -> Result<()> {
    emit_json(&json!({
        "source": "raw_mode_conflict",
        "device_key": conflict.device_key,
        "expected": conflict.expected,
        "actual": conflict.actual,
    }))
}

fn emit_json(value: &serde_json::Value) -> Result<()> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    serde_json::to_writer(&mut output, value).context("serializing diagnostic trace")?;
    writeln!(output).context("writing diagnostic trace")
}

async fn wait_for_device_key(client: &AgentClient, query: Option<&str>) -> Result<String> {
    let config = Config::load_or_default().context("loading config for device selection")?;
    let deadline = Instant::now() + INVENTORY_TIMEOUT;
    loop {
        let snapshot = call(
            client.clone().snapshot(context::current()),
            "reading inventory",
        )
        .await?;
        let candidates = touchpad_candidates(&snapshot, &config);
        if let Some(query) = query {
            if let Some(key) = match_device_query(&candidates, query)? {
                return Ok(key.to_string());
            }
        } else if let Some((_, key)) = config
            .selected_device
            .as_deref()
            .and_then(|selected| candidates.iter().find(|(_, key)| key == selected))
            .or_else(|| (candidates.len() == 1).then(|| &candidates[0]))
        {
            return Ok(key.clone());
        }
        if Instant::now() >= deadline || !candidates.is_empty() {
            let available = candidates
                .iter()
                .map(|(name, key)| format!("{name} ({key})"))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(if available.is_empty() {
                anyhow!("no online raw-touchpad device was reported by the Agent")
            } else if let Some(query) = query {
                anyhow!("no raw-touchpad device matches `--device {query}`; available: {available}")
            } else {
                anyhow!(
                    "multiple raw-touchpad devices are online; select one with `--device`: {available}"
                )
            });
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn match_device_query<'a>(
    candidates: &'a [(String, String)],
    query: &str,
) -> Result<Option<&'a str>> {
    let needle = query.to_lowercase();
    let matches = candidates
        .iter()
        .filter(|(name, key)| {
            name.to_lowercase().contains(&needle) || key.to_lowercase().contains(&needle)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [(_, key)] => Ok(Some(key)),
        _ => {
            let available = matches
                .iter()
                .map(|(name, key)| format!("{name} ({key})"))
                .collect::<Vec<_>>()
                .join(", ");
            bail!("`--device {query}` is ambiguous; matches: {available}")
        }
    }
}

fn touchpad_candidates(snapshot: &AgentSnapshot, config: &Config) -> Vec<(String, String)> {
    let mut candidates = BTreeMap::new();
    for inventory in &snapshot.inventory {
        for paired in inventory.paired.iter().filter(|paired| {
            paired.online
                && paired
                    .capabilities
                    .is_some_and(|capabilities| capabilities.touchpad_raw_xy)
        }) {
            let Some(model) = paired.model_info.as_ref() else {
                continue;
            };
            let route = DeviceRoute::device_route_for(inventory, paired.slot);
            let stable = DeviceStableId::from_parts(
                route.as_ref(),
                paired.slot,
                model.serial_number.as_deref(),
                model.unit_id,
            );
            let identity =
                DeviceIdentity::from_parts(model.serial_number.as_deref(), model.unit_id);
            let Some(key) = config.resolve_device_key(&stable, Some(&identity)) else {
                continue;
            };
            candidates.entry(key.into_string()).or_insert_with(|| {
                paired
                    .codename
                    .clone()
                    .unwrap_or_else(|| format!("Slot {}", paired.slot))
            });
        }
    }
    candidates
        .into_iter()
        .map(|(key, name)| (name, key))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_query_must_select_exactly_one_touchpad() {
        let candidates = vec![
            ("Casa Touch Office".to_string(), "serial:office".to_string()),
            ("Casa Touch Travel".to_string(), "serial:travel".to_string()),
        ];

        assert_eq!(
            match_device_query(&candidates, "travel").expect("unique match"),
            Some("serial:travel")
        );
        assert!(
            match_device_query(&candidates, "Casa Touch")
                .expect_err("ambiguous name must not silently pick a device")
                .to_string()
                .contains("is ambiguous")
        );
    }
}
