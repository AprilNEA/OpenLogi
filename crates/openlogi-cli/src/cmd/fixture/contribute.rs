//! Resumable, privacy-safe complete fixture contribution wizard.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use openlogi_core::device::DeviceModelInfo;
use openlogi_core::hid::DeviceRoute;
use openlogi_fixture::{
    DeviceProfile, FixtureCaseBinding, FixtureManifest, HidCassette, SyntheticIdentityKind,
    classify_synthetic_profile_identity, generate_synthetic_identity,
};
use openlogi_hid::recording::{HidCassetteIdentityPlan, SanitizedIdentityKind};
use serde::{Deserialize, Serialize};

use super::record_case::{self, FixtureOperation};
use super::record_profile;

const STATE_FILE: &str = ".openlogi-contribution.json";
const PROFILE_FILE: &str = "profile.json";
const MANIFEST_FILE: &str = "manifest.json";
const CASES_DIRECTORY: &str = "cases";
const STATE_VERSION: u32 = 1;
const CASE_CHANNEL: &str = "target";

/// Arguments for a resumable privacy-safe fixture contribution.
#[derive(Debug, Args)]
pub struct ContributeArgs {
    /// Synthetic specimen ID; must equal the output directory name.
    #[arg(long)]
    pub id: String,
    /// Human-readable synthetic device name.
    #[arg(long)]
    pub name: String,
    /// Final fixture directory, named exactly after --id.
    #[arg(long)]
    pub output: PathBuf,
    /// Case-insensitive exact display name, or exact rendered device route.
    #[arg(long)]
    pub device: Option<String>,
    /// Capture only semantic Agent data and skip direct HID cassette recording.
    #[arg(long)]
    pub profile_only: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContributionState {
    version: u32,
    fixture_id: String,
    profile_id: String,
    profile_name: String,
    selected_route: DeviceRoute,
}

pub async fn run(args: ContributeArgs) -> Result<()> {
    validate_args(&args)?;
    let state_path = args.output.join(STATE_FILE);
    if !args
        .output
        .try_exists()
        .context("could not inspect output directory")?
    {
        return start(args, &state_path).await;
    }
    require_directory(&args.output, "contribution output")?;
    if !state_path
        .try_exists()
        .context("could not inspect contribution state")?
    {
        bail!(
            "{} already exists but is not an in-progress OpenLogi contribution; run `openlogi \
             fixture verify {}` to inspect it",
            args.output.display(),
            args.output.display()
        );
    }
    finish(args, &state_path).await
}

async fn start(args: ContributeArgs, state_path: &Path) -> Result<()> {
    let profile_id = format!("{}-profile", args.id);
    println!("Step 1/2: reading semantic state through the running OpenLogi Agent…");
    let captured = record_profile::capture_for_contribution(
        profile_id.clone(),
        args.name.clone(),
        args.device.as_deref(),
    )
    .await?;
    let profile = captured.profile;

    if args.profile_only || matches!(captured.selected_route, DeviceRoute::RawHid { .. }) {
        publish_profile_only(&args, &profile)?;
        return Ok(());
    }

    fs::create_dir_all(&args.output).with_context(|| {
        format!(
            "could not create contribution directory {}",
            args.output.display()
        )
    })?;
    let profile_path = args.output.join(PROFILE_FILE);
    super::output::write_json_atomically(&profile_path, &profile, false, "device profile")?;
    let state = ContributionState {
        version: STATE_VERSION,
        fixture_id: args.id,
        profile_id,
        profile_name: args.name,
        selected_route: captured.selected_route,
    };
    if let Err(error) =
        super::output::write_json_atomically(state_path, &state, false, "contribution state")
    {
        let _ = fs::remove_file(&profile_path);
        let _ = fs::remove_file(state_path);
        let _ = fs::remove_dir(&args.output);
        return Err(error);
    }

    println!(
        "Step 1/2 complete: the privacy-safe profile is in {}.",
        args.output.display()
    );
    println!(
        "Stop the OpenLogi Agent, keep the same physical device connected, then rerun the same command."
    );
    println!(
        "The second step uses this CLI process's own HID permission and records only eight read-only operations."
    );
    Ok(())
}

async fn finish(args: ContributeArgs, state_path: &Path) -> Result<()> {
    require_regular_file(state_path, "contribution state")?;
    let state: ContributionState = read_json(state_path, "contribution state")?;
    validate_state(&args, &state)?;
    let profile_path = args.output.join(PROFILE_FILE);
    require_regular_file(&profile_path, "captured device profile")?;
    let profile: DeviceProfile = read_json(&profile_path, "captured device profile")?;
    profile
        .validate()
        .context("saved contribution profile is invalid")?;
    if profile.id != state.profile_id || profile.name != state.profile_name {
        bail!("saved contribution profile does not match its resumable state");
    }

    if args.profile_only {
        publish_manifest_and_finish(&args.output, state_path, &profile, &[], &[])?;
        println!(
            "Finished a profile-only contribution at {}.",
            args.output.display()
        );
        return Ok(());
    }

    println!("Step 2/2: Agent must be stopped; selecting the same structural target…");
    let target = record_case::prepare_contribution_target(args.device.as_deref()).await?;
    require_same_structural_route(&state.selected_route, target.route())?;
    let identity_plan = identity_plan(&profile, &state.selected_route)?;

    let mut cassettes = Vec::with_capacity(FixtureOperation::ALL.len());
    for (index, operation) in FixtureOperation::ALL.into_iter().enumerate() {
        println!(
            "  recording {}/{}: {}",
            index + 1,
            FixtureOperation::ALL.len(),
            operation.slug()
        );
        cassettes.push(
            record_case::capture_for_contribution(
                operation,
                &target,
                operation.slug(),
                CASE_CHANNEL,
                record_case::DEFAULT_RECORDING_CAPACITY,
                &identity_plan,
            )
            .await?,
        );
    }
    let bindings = cassettes
        .iter()
        .map(|cassette| FixtureCaseBinding {
            name: cassette.name.clone(),
            route: state.selected_route.clone(),
        })
        .collect::<Vec<_>>();
    publish_manifest_and_finish(&args.output, state_path, &profile, &cassettes, &bindings)?;

    println!(
        "Fixture contribution is complete at {}.",
        args.output.display()
    );
    println!(
        "Self-replay and privacy verification passed; physical and semantic correctness still require maintainer review."
    );
    println!("No data was uploaded. Commit the directory in a pull request when you are ready.");
    Ok(())
}

fn publish_profile_only(args: &ContributeArgs, profile: &DeviceProfile) -> Result<()> {
    let manifest = FixtureManifest::from_assets(args.id.clone(), profile, &[], &[])
        .context("could not generate profile-only fixture manifest")?;
    super::output::write_json_atomically(
        &args.output.join(PROFILE_FILE),
        profile,
        false,
        "device profile",
    )?;
    super::output::write_json_atomically(
        &args.output.join(MANIFEST_FILE),
        &manifest,
        false,
        "fixture manifest",
    )?;
    super::verify::run(&super::verify::VerifyArgs {
        directory: args.output.clone(),
    })?;
    println!(
        "Profile-only fixture contribution is complete at {}.",
        args.output.display()
    );
    println!(
        "No data was uploaded; physical and semantic correctness still require maintainer review."
    );
    Ok(())
}

fn publish_manifest_and_finish(
    directory: &Path,
    state_path: &Path,
    profile: &DeviceProfile,
    cassettes: &[HidCassette],
    bindings: &[FixtureCaseBinding],
) -> Result<()> {
    let fixture_id = directory
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| anyhow!("fixture directory has no UTF-8 synthetic ID"))?;
    let manifest =
        FixtureManifest::from_assets(fixture_id.to_string(), profile, cassettes, bindings)
            .context("could not generate an exact fixture manifest from sanitized assets")?;
    validate_resumable_layout(directory, cassettes)?;

    if !cassettes.is_empty() {
        let cases_directory = directory.join(CASES_DIRECTORY);
        if cases_directory
            .try_exists()
            .context("could not inspect cases directory")?
        {
            require_directory(&cases_directory, "fixture cases directory")?;
        }
        for cassette in cassettes {
            let path = cases_directory.join(format!("{}.json", cassette.name));
            reject_symlink(&path, "fixture cassette")?;
            super::output::write_json_atomically(&path, cassette, true, "HID cassette")?;
        }
    }
    let manifest_path = directory.join(MANIFEST_FILE);
    reject_symlink(&manifest_path, "fixture manifest")?;
    super::output::write_json_atomically(&manifest_path, &manifest, true, "fixture manifest")?;
    fs::remove_file(state_path).with_context(|| {
        format!(
            "fixture assets were written but resumable state {} could not be removed",
            state_path.display()
        )
    })?;
    super::verify::run(&super::verify::VerifyArgs {
        directory: directory.to_path_buf(),
    })
}

fn validate_resumable_layout(directory: &Path, cassettes: &[HidCassette]) -> Result<()> {
    for entry in
        fs::read_dir(directory).context("could not inspect resumable contribution directory")?
    {
        let entry = entry.context("could not inspect resumable contribution entry")?;
        let name = entry.file_name();
        match name.to_str() {
            Some(STATE_FILE | PROFILE_FILE) => {}
            Some(MANIFEST_FILE) => require_regular_file(&entry.path(), "fixture manifest")?,
            Some(CASES_DIRECTORY) if !cassettes.is_empty() => {
                require_directory(&entry.path(), "fixture cases directory")?;
                validate_resumable_cases(&entry.path(), cassettes)?;
            }
            Some(other) => bail!(
                "in-progress contribution contains unexpected entry {other:?}; refusing to \
                 publish over it"
            ),
            None => bail!("in-progress contribution contains a non-UTF-8 entry"),
        }
    }
    Ok(())
}

fn validate_resumable_cases(directory: &Path, cassettes: &[HidCassette]) -> Result<()> {
    let expected = cassettes
        .iter()
        .map(|cassette| format!("{}.json", cassette.name))
        .collect::<Vec<_>>();
    for entry in fs::read_dir(directory).context("could not inspect resumable fixture cases")? {
        let entry = entry.context("could not inspect resumable fixture case")?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            bail!("resumable fixture cases contain a non-UTF-8 entry");
        };
        if !expected.iter().any(|expected| expected == name) {
            bail!("resumable fixture cases contain unexpected file {name:?}");
        }
        require_regular_file(&entry.path(), "fixture cassette")?;
    }
    Ok(())
}

fn identity_plan(
    profile: &DeviceProfile,
    selected_route: &DeviceRoute,
) -> Result<HidCassetteIdentityPlan> {
    let mut plan = HidCassetteIdentityPlan::default();
    let model = match selected_route {
        DeviceRoute::Bolt { receiver_uid, slot } => {
            plan.insert(
                SanitizedIdentityKind::ReceiverUniqueId,
                receiver_uid.as_bytes().to_vec(),
            )?;
            selected_model(profile, selected_route, *slot)?
        }
        DeviceRoute::Unifying { receiver_uid, slot } => {
            let ordinal = classify_synthetic_profile_identity(
                SyntheticIdentityKind::UnifyingReceiverRoute,
                receiver_uid,
            )?;
            let serial =
                generate_synthetic_identity(SyntheticIdentityKind::UnifyingReceiverSerial, ordinal)
                    .as_bytes()
                    .ok_or_else(|| anyhow!("could not derive synthetic Unifying serial"))?
                    .to_vec();
            plan.insert(SanitizedIdentityKind::ReceiverSerialNumber, serial)?;
            selected_model(profile, selected_route, *slot)?
        }
        DeviceRoute::Direct { .. } => selected_model(
            profile,
            selected_route,
            openlogi_core::hid::DIRECT_DEVICE_INDEX,
        )?,
        DeviceRoute::RawHid { .. } => {
            bail!("raw-HID devices support profile-only contributions")
        }
    };
    add_model_identities(&mut plan, model)?;
    Ok(plan)
}

fn selected_model<'a>(
    profile: &'a DeviceProfile,
    route: &DeviceRoute,
    slot: u8,
) -> Result<&'a DeviceModelInfo> {
    profile
        .inventories
        .iter()
        .find(|inventory| {
            inventory.paired.iter().any(|device| {
                DeviceRoute::device_route_for(inventory, device.slot).as_ref() == Some(route)
            })
        })
        .and_then(|inventory| inventory.paired.iter().find(|device| device.slot == slot))
        .and_then(|device| device.model_info.as_ref())
        .ok_or_else(|| anyhow!("selected profile route has no identity-bearing device model"))
}

fn add_model_identities(plan: &mut HidCassetteIdentityPlan, model: &DeviceModelInfo) -> Result<()> {
    if model.unit_id == [0; 4] && model.serial_number.is_none() {
        bail!(
            "the selected device has no stable synthetic identity for cassette relationships; \
             rerun with --profile-only"
        );
    }
    if model.unit_id != [0; 4] {
        plan.insert(SanitizedIdentityKind::DeviceUnitId, model.unit_id.to_vec())?;
    }
    if let Some(serial) = model.serial_number.as_deref() {
        plan.insert(
            SanitizedIdentityKind::DeviceSerialNumber,
            serial.as_bytes().to_vec(),
        )?;
    }
    Ok(())
}

fn require_same_structural_route(expected: &DeviceRoute, actual: &DeviceRoute) -> Result<()> {
    let matches = match (expected, actual) {
        (DeviceRoute::Bolt { slot: left, .. }, DeviceRoute::Bolt { slot: right, .. })
        | (DeviceRoute::Unifying { slot: left, .. }, DeviceRoute::Unifying { slot: right, .. }) => {
            left == right
        }
        (
            DeviceRoute::Direct {
                vendor_id: left_vendor,
                product_id: left_product,
            },
            DeviceRoute::Direct {
                vendor_id: right_vendor,
                product_id: right_product,
            },
        ) => left_vendor == right_vendor && left_product == right_product,
        (
            DeviceRoute::RawHid {
                vendor_id: left_vendor,
                product_id: left_product,
                usage_page: left_page,
                usage_id: left_usage,
                ..
            },
            DeviceRoute::RawHid {
                vendor_id: right_vendor,
                product_id: right_product,
                usage_page: right_page,
                usage_id: right_usage,
                ..
            },
        ) => {
            left_vendor == right_vendor
                && left_product == right_product
                && left_page == right_page
                && left_usage == right_usage
        }
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        bail!(
            "the selected direct-capture target does not match the profile transport and slot; \
             reconnect the same device and use the same --device selector"
        )
    }
}

fn validate_args(args: &ContributeArgs) -> Result<()> {
    if args.id.trim().is_empty() || matches!(args.id.as_str(), "." | "..") {
        bail!("--id must be a nonempty synthetic path component");
    }
    if Path::new(&args.id).file_name() != Some(OsStr::new(&args.id))
        || args.id.contains('/')
        || args.id.contains('\\')
    {
        bail!("--id must be one synthetic path component without separators");
    }
    if args.name.trim().is_empty() {
        bail!("--name must be a nonempty synthetic device name");
    }
    if args.output.file_name() != Some(OsStr::new(&args.id)) {
        bail!("--output directory name must exactly equal --id");
    }
    Ok(())
}

fn validate_state(args: &ContributeArgs, state: &ContributionState) -> Result<()> {
    if state.version != STATE_VERSION {
        bail!("unsupported contribution state version {}", state.version);
    }
    if state.fixture_id != args.id
        || state.profile_id != format!("{}-profile", args.id)
        || state.profile_name != args.name
    {
        bail!("--id and --name must match the in-progress contribution");
    }
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, asset: &str) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("could not read {asset}"))?;
    serde_json::from_slice(&bytes).with_context(|| format!("could not parse {asset}"))
}

fn require_directory(path: &Path, asset: &str) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("could not inspect {asset}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("{asset} must be a non-symlink directory");
    }
    Ok(())
}

fn require_regular_file(path: &Path, asset: &str) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("could not inspect {asset}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{asset} must be a non-symlink regular file");
    }
    Ok(())
}

fn reject_symlink(path: &Path, asset: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("{asset} output must not be a symlink")
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("could not inspect {asset} output")),
    }
}

#[cfg(test)]
mod tests {
    use openlogi_fixture::CANONICAL_DEVICE_PROFILE_JSON;

    use super::*;

    #[test]
    fn output_directory_must_match_the_synthetic_id() {
        let args = ContributeArgs {
            id: "fixture-001".to_string(),
            name: "Synthetic mouse".to_string(),
            output: PathBuf::from("fixtures/devices/other"),
            device: None,
            profile_only: false,
        };
        assert!(validate_args(&args).is_err());
    }

    #[test]
    fn structural_routes_ignore_only_receiver_identity() {
        let expected = DeviceRoute::Bolt {
            receiver_uid: "OL-BOLT-UID-0001".to_string(),
            slot: 2,
        };
        let same_target = DeviceRoute::Bolt {
            receiver_uid: "private original is never persisted".to_string(),
            slot: 2,
        };
        let wrong_slot = DeviceRoute::Bolt {
            receiver_uid: "another".to_string(),
            slot: 3,
        };
        require_same_structural_route(&expected, &same_target).expect("identity is ignored");
        require_same_structural_route(&expected, &wrong_slot).expect_err("slot is retained");
    }

    #[test]
    fn resumable_state_contains_only_synthetic_route_data() {
        let state = ContributionState {
            version: STATE_VERSION,
            fixture_id: "fixture-001".to_string(),
            profile_id: "fixture-001-profile".to_string(),
            profile_name: "Synthetic mouse".to_string(),
            selected_route: DeviceRoute::Bolt {
                receiver_uid: "OL-BOLT-UID-0001".to_string(),
                slot: 1,
            },
        };
        let encoded = serde_json::to_string(&state).expect("state serializes");
        assert!(!encoded.contains("selector"));
        assert!(!encoded.contains("path"));
        assert_eq!(
            serde_json::from_str::<ContributionState>(&encoded).expect("state round trips"),
            state
        );
    }

    #[test]
    fn profile_identity_plan_is_derived_from_sanitized_profile() {
        let profile: DeviceProfile =
            serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");
        let route = profile
            .settings
            .iter()
            .map(|settings| &settings.route)
            .find(|route| !matches!(route, DeviceRoute::RawHid { .. }))
            .expect("canonical profile has a HID++ route");

        identity_plan(&profile, route).expect("canonical identity plan is accepted");
    }

    #[test]
    fn cassette_capture_requires_a_stable_profile_device_identity() {
        let mut profile: DeviceProfile =
            serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");
        let route = DeviceRoute::Direct {
            vendor_id: profile.inventories[1].receiver.vendor_id,
            product_id: profile.inventories[1].receiver.product_id,
        };
        let model = profile.inventories[1].paired[0]
            .model_info
            .as_mut()
            .expect("canonical direct device has model info");
        model.unit_id = [0; 4];
        model.serial_number = None;

        let error = identity_plan(&profile, &route)
            .expect_err("a cassette case needs an identity-bearing device principal");
        assert!(error.to_string().contains("--profile-only"));
    }

    #[test]
    fn profile_only_publication_generates_a_strict_fixture() {
        let profile: DeviceProfile =
            serde_json::from_str(CANONICAL_DEVICE_PROFILE_JSON).expect("canonical profile parses");
        let parent = tempfile::tempdir().expect("tempdir");
        let output = parent.path().join("fixture-001");
        let args = ContributeArgs {
            id: "fixture-001".to_string(),
            name: "Synthetic mouse".to_string(),
            output: output.clone(),
            device: None,
            profile_only: true,
        };

        publish_profile_only(&args, &profile).expect("profile-only fixture is published");

        assert!(output.join(PROFILE_FILE).is_file());
        assert!(output.join(MANIFEST_FILE).is_file());
        assert!(!output.join(STATE_FILE).exists());
    }
}
