//! `openlogi fixture record profile` Agent-only semantic capture.

use std::future::Future;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use openlogi_core::device::{DeviceInventory, PairedDevice, StandaloneDevice};
use openlogi_core::hid::{DeviceRoute, WriteError};
use openlogi_fixture::{
    DeviceProfile, FIXTURE_SCHEMA_VERSION, ProfileDeviceSettings, ProfileSetting, ProfileSupport,
};
use openlogi_ipc::client::ConnectError;
use openlogi_ipc::{AgentClient, AgentSnapshot};
use tarpc::context;

use crate::agent::{self, CallFailure};

mod sanitize;
mod selection;

use selection::TargetLocation;

/// Arguments for one privacy-safe semantic device profile capture.
#[derive(Debug, Args)]
pub struct RecordProfileArgs {
    /// Synthetic fixture profile ID; never use a hardware serial or host identifier.
    #[arg(long)]
    pub id: String,
    /// Human-readable synthetic profile name.
    #[arg(long)]
    pub name: String,
    /// Final version-1 JSON profile path.
    #[arg(long)]
    pub output: PathBuf,
    /// Case-insensitive exact display name, or exact rendered device route.
    #[arg(long)]
    pub device: Option<String>,
    /// Replace an existing output only after capture and validation succeed.
    #[arg(long)]
    pub force: bool,
}

/// Capture only through the running Agent. This path deliberately has no
/// native-enumeration import, fallback, or process-spawn branch.
pub async fn run(args: RecordProfileArgs) -> Result<()> {
    validate_metadata(&args)?;
    super::output::ensure_output_available(&args.output, args.force)?;
    let client = connect_to_agent().await?;
    capture_connected(args, client).await
}

pub(super) struct CapturedProfile {
    pub(super) profile: DeviceProfile,
    pub(super) selected_route: DeviceRoute,
}

type ProfileCaptureParts = (
    Vec<DeviceInventory>,
    Vec<StandaloneDevice>,
    Vec<ProfileDeviceSettings>,
    DeviceRoute,
);

pub(super) async fn capture_for_contribution(
    id: String,
    name: String,
    selector: Option<&str>,
) -> Result<CapturedProfile> {
    let connection = connect_to_agent().await?;
    capture_connected_profile(&connection, selector, id, name).await
}

async fn connect_to_agent() -> Result<AgentClient> {
    agent::connect()
        .await
        .map_err(|error| safe_connect_error(&error))
}

fn safe_connect_error(error: &ConnectError) -> anyhow::Error {
    match error {
        ConnectError::Endpoint(_) => anyhow!(
            "could not reach the running OpenLogi Agent; start the Agent and retry (semantic \
             profile capture has no direct-hardware fallback)"
        ),
        ConnectError::Handshake(_) => anyhow!(
            "the running OpenLogi Agent did not complete a healthy IPC handshake; restart it and \
             retry (no profile was written)"
        ),
        ConnectError::Skew(skew) => anyhow!(
            "{skew}; update or restart OpenLogi so both processes match (no profile was written)"
        ),
        ConnectError::Timeout => anyhow!(
            "timed out reaching the running OpenLogi Agent; restart it and retry (semantic \
             profile capture has no direct-hardware fallback, and no profile was written)"
        ),
    }
}

async fn capture_connected(args: RecordProfileArgs, client: AgentClient) -> Result<()> {
    let captured =
        capture_connected_profile(&client, args.device.as_deref(), args.id, args.name).await?;
    let profile = captured.profile;
    super::output::write_json_atomically(&args.output, &profile, args.force, "device profile")?;

    println!(
        "Recorded semantic profile `{}` to {} through the running Agent.",
        profile.id,
        args.output.display()
    );
    println!(
        "The captured values are semantic review candidates, not proof of physical or protocol \
         correctness. Review the profile before committing it."
    );
    Ok(())
}

async fn capture_connected_profile(
    client: &AgentClient,
    selector: Option<&str>,
    id: String,
    name: String,
) -> Result<CapturedProfile> {
    let snapshot = agent::snapshot(client).await?;

    let captured = capture_profile(client, snapshot, selector, id, name).await?;
    captured
        .profile
        .validate()
        .context("captured semantic profile failed version-1 validation; no profile was written")?;
    Ok(captured)
}

fn validate_metadata(args: &RecordProfileArgs) -> Result<()> {
    if args.id.trim().is_empty() {
        bail!("--id must be a nonempty synthetic identifier");
    }
    if args.name.trim().is_empty() {
        bail!("--name must be a nonempty synthetic profile name");
    }
    Ok(())
}

async fn capture_profile(
    client: &AgentClient,
    snapshot: AgentSnapshot,
    selector: Option<&str>,
    id: String,
    name: String,
) -> Result<CapturedProfile> {
    // Runtime status, camera, foreground-app, and pairing facts are dropped at
    // this boundary and can never enter the serializable profile value.
    let AgentSnapshot {
        inventory,
        standalone,
        ..
    } = snapshot;
    let candidates = selection::target_candidates(&inventory, &standalone);
    let selected = selection::select_target(&candidates, selector)?;

    let (inventories, standalone, settings, selected_route) = match selected.location {
        TargetLocation::Inventory {
            inventory: inventory_index,
            device: device_index,
        } => {
            let source = inventory
                .get(inventory_index)
                .ok_or_else(|| anyhow!("the Agent snapshot changed during target selection"))?;
            capture_inventory(client, source, device_index).await?
        }
        TargetLocation::Standalone { device } => {
            let source = standalone
                .get(device)
                .ok_or_else(|| anyhow!("the Agent snapshot changed during target selection"))?;
            capture_standalone(source)?
        }
    };

    Ok(CapturedProfile {
        profile: DeviceProfile {
            schema_version: FIXTURE_SCHEMA_VERSION,
            id,
            name,
            inventories,
            standalone,
            settings,
        },
        selected_route,
    })
}

async fn capture_inventory(
    client: &AgentClient,
    source: &DeviceInventory,
    selected_device: usize,
) -> Result<ProfileCaptureParts> {
    let mut retained = source.clone();
    if retained.receiver.unique_id.is_none() {
        retained.paired = vec![source.paired.get(selected_device).cloned().ok_or_else(|| {
            anyhow!("the selected direct device is absent from the Agent snapshot")
        })?];
    }

    let source_routes = retained
        .paired
        .iter()
        .map(|device| {
            DeviceRoute::device_route_for(&retained, device.slot).ok_or_else(|| {
                anyhow!("a retained Agent inventory route is not safely addressable")
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let retained_selected = if retained.receiver.unique_id.is_none() {
        0
    } else {
        selected_device
    };
    sanitize::inventory(&mut retained)?;
    let profile_routes = retained
        .paired
        .iter()
        .map(|device| {
            DeviceRoute::device_route_for(&retained, device.slot)
                .ok_or_else(|| anyhow!("a sanitized profile route is not addressable"))
        })
        .collect::<Result<Vec<_>>>()?;
    let selected_route = profile_routes
        .get(retained_selected)
        .cloned()
        .ok_or_else(|| anyhow!("the sanitized selected profile route is absent"))?;

    let mut settings = Vec::with_capacity(retained.paired.len());
    for ((source_route, profile_route), device) in source_routes
        .iter()
        .zip(profile_routes)
        .zip(&retained.paired)
    {
        settings.push(capture_hidpp_settings(client, source_route, profile_route, device).await?);
    }
    Ok((vec![retained], Vec::new(), settings, selected_route))
}

async fn capture_hidpp_settings(
    client: &AgentClient,
    source_route: &DeviceRoute,
    profile_route: DeviceRoute,
    device: &PairedDevice,
) -> Result<ProfileDeviceSettings> {
    let capabilities = device.capabilities.ok_or_else(|| {
        anyhow!(
            "a retained HID++ route has no captured capability facts; refusing to guess setting \
             support (no profile was written)"
        )
    })?;

    let dpi = match capability_setting(device.online, capabilities.pointer) {
        Some(setting) => setting,
        None => {
            semantic_read(
                "DPI",
                agent::call(client.read_dpi(context::current(), source_route.clone())),
            )
            .await?
        }
    };
    let smartshift = if device.online {
        semantic_read(
            "SmartShift",
            agent::call(client.read_smartshift(context::current(), source_route.clone())),
        )
        .await?
    } else {
        return Err(unknown_offline_support("SmartShift"));
    };
    let wheel = match capability_setting(device.online, capabilities.hires_wheel) {
        Some(setting) => setting,
        None => {
            semantic_read(
                "wheel",
                agent::call(client.read_wheel(context::current(), source_route.clone())),
            )
            .await?
        }
    };
    // Schema v1 makes RGB lighting and keyboard backlight mutually exclusive,
    // so observed RGB capability is a sufficient negative backlight fact.
    let backlight = if capabilities.lighting {
        ProfileSetting::Unsupported
    } else if device.online {
        semantic_read(
            "backlight",
            agent::call(client.read_backlight(context::current(), source_route.clone())),
        )
        .await?
    } else {
        return Err(unknown_offline_support("backlight"));
    };

    Ok(ProfileDeviceSettings {
        route: profile_route,
        dpi,
        smartshift,
        wheel,
        backlight,
        lighting: profile_support(capabilities.lighting),
        light: ProfileSupport::Unsupported,
    })
}

fn capability_setting<T>(online: bool, supported: bool) -> Option<ProfileSetting<T>> {
    if !supported {
        Some(ProfileSetting::Unsupported)
    } else if online {
        None
    } else {
        Some(ProfileSetting::Unavailable)
    }
}

fn unknown_offline_support(family: &str) -> anyhow::Error {
    anyhow!(
        "a retained offline HID++ route has no captured {family} capability fact; refusing to \
         guess support (no profile was written)"
    )
}

async fn semantic_read<T>(
    family: &'static str,
    call: impl Future<Output = Result<Result<T, WriteError>, CallFailure>>,
) -> Result<ProfileSetting<T>> {
    let result = call.await.map_err(|_| safe_read_error(family))?;
    match result {
        Ok(value) => Ok(ProfileSetting::Supported(value)),
        Err(WriteError::FeatureUnsupported { .. }) => Ok(ProfileSetting::Unsupported),
        Err(_) => Err(safe_read_error(family)),
    }
}

fn safe_read_error(family: &str) -> anyhow::Error {
    anyhow!(
        "the running Agent could not complete the online {family} semantic read; transient, \
         transport, protocol, timeout, open, and disconnect errors abort capture (no profile was \
         written)"
    )
}

fn capture_standalone(source: &StandaloneDevice) -> Result<ProfileCaptureParts> {
    let mut retained = source.clone();
    sanitize::standalone(&mut retained)?;
    let route = retained.route();
    let light_supported = retained.light_capabilities.is_some_and(|capabilities| {
        capabilities.power
            || capabilities.brightness.is_some()
            || capabilities.temperature.is_some()
    });
    let settings = ProfileDeviceSettings {
        route: route.clone(),
        dpi: ProfileSetting::Unsupported,
        smartshift: ProfileSetting::Unsupported,
        wheel: ProfileSetting::Unsupported,
        backlight: ProfileSetting::Unsupported,
        lighting: ProfileSupport::Unsupported,
        light: profile_support(light_supported),
    };
    Ok((Vec::new(), vec![retained], vec![settings], route))
}

const fn profile_support(supported: bool) -> ProfileSupport {
    if supported {
        ProfileSupport::Supported
    } else {
        ProfileSupport::Unsupported
    }
}

#[cfg(test)]
#[path = "record_profile/tests.rs"]
mod tests;
