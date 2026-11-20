//! `openlogi diag g600` — read a G600's active profile and the DPI stages of
//! its onboard profiles, or change its active DPI stage or a profile's DPI
//! stages. Profiles and stages are numbered from 1, like the mouse's own
//! software.
//!
//! It refuses to run while the OpenLogi agent runs, and keeps the agent from
//! starting until it exits: the mouse corrupts a profile it is storing when
//! another request reaches it, and only one process can space them out.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use openlogi_hid::{
    DeviceRoute, G600_DPI_STAGE_COUNT, G600_DRIVER_ID, G600_PROFILE_COUNT, G600ActiveSelection,
    G600DpiSettings, G600DpiStage, G600DpiValue, G600Profile, G600ProfileDpi, G600StageAfterStore,
};

use crate::cmd::diag::exclusive_hardware;
use crate::cmd::standalone::select;

#[derive(Debug, Args)]
pub struct G600Args {
    /// Run against the G600 whose name or identity contains this string.
    #[arg(long, value_name = "NAME", global = true)]
    pub device: Option<String>,
    /// Change a setting, then print the mouse's state; without one, only print it.
    #[command(subcommand)]
    pub action: Option<G600Action>,
}

/// A change `openlogi diag g600` makes before printing the mouse's state.
#[derive(Debug, Subcommand)]
pub enum G600Action {
    /// Switch the active profile to DPI stage N.
    Stage {
        /// Stage to switch to.
        #[arg(value_name = "N", value_parser = parse_stage, help = stage_help())]
        stage: G600DpiStage,
    },
    /// Store every DPI stage of an onboard profile and the stage it starts on.
    Dpi {
        /// Profile to change.
        #[arg(long, value_parser = parse_profile, help = profile_help())]
        profile: G600Profile,
        /// Each stage's DPI, or `off`, comma-separated.
        #[arg(long, value_name = "S1,S2,...", value_parser = parse_stages, help = stages_help())]
        stages: [Option<G600DpiValue>; G600_DPI_STAGE_COUNT],
        /// Stage the profile starts on; it must be enabled.
        #[arg(long, value_name = "N", value_parser = parse_stage, help = default_help())]
        default: G600DpiStage,
        /// Stage of the active profile to use once it is stored.
        #[arg(long, value_name = "N", value_parser = parse_stage, help = use_stage_help())]
        use_stage: Option<G600DpiStage>,
    },
}

fn stage_range() -> String {
    format!("from 1 to {G600_DPI_STAGE_COUNT}")
}

fn dpi_range() -> String {
    format!(
        "from {} to {} in steps of {}",
        G600DpiValue::MIN,
        G600DpiValue::MAX,
        G600DpiValue::STEP
    )
}

fn stage_help() -> String {
    format!("Stage to switch to, {}", stage_range())
}

fn profile_range() -> String {
    format!("from 1 to {G600_PROFILE_COUNT}")
}

fn profile_help() -> String {
    format!("Profile to change, {}", profile_range())
}

fn stages_help() -> String {
    format!(
        "{G600_DPI_STAGE_COUNT} comma-separated stage DPIs, each {} or `off`",
        dpi_range()
    )
}

fn default_help() -> String {
    format!(
        "Stage the profile starts on, {}; it must be enabled",
        stage_range()
    )
}

fn use_stage_help() -> String {
    format!(
        "Stage to use once the profile is stored, {}; it must be enabled, and the profile \
         must be the active one. Without it the mouse stays on its stage, or takes the \
         default stage when that one is turned off",
        stage_range()
    )
}

/// A 1-based stage number.
fn parse_stage(value: &str) -> Result<G600DpiStage, String> {
    value
        .parse()
        .ok()
        .and_then(G600DpiStage::from_number)
        .ok_or_else(|| format!("expected a stage {}", stage_range()))
}

/// A 1-based profile number.
fn parse_profile(value: &str) -> Result<G600Profile, String> {
    value
        .parse()
        .ok()
        .and_then(G600Profile::from_number)
        .ok_or_else(|| format!("expected a profile {}", profile_range()))
}

/// One DPI per stage, comma-separated, `off` for a disabled stage.
fn parse_stages(value: &str) -> Result<[Option<G600DpiValue>; G600_DPI_STAGE_COUNT], String> {
    let entries: [&str; G600_DPI_STAGE_COUNT] = value
        .split(',')
        .map(str::trim)
        .collect::<Vec<_>>()
        .try_into()
        .map_err(|entries: Vec<_>| {
            format!(
                "expected {G600_DPI_STAGE_COUNT} comma-separated stages, got {}",
                entries.len()
            )
        })?;
    let mut stages = [None; G600_DPI_STAGE_COUNT];
    for ((number, entry), stage) in (1..).zip(entries).zip(&mut stages) {
        if entry.eq_ignore_ascii_case("off") {
            continue;
        }
        let dpi = entry.parse().ok().and_then(G600DpiValue::new);
        *stage = Some(dpi.ok_or_else(|| {
            format!(
                "stage {number}: expected a DPI {} or `off`, got `{entry}`",
                dpi_range()
            )
        })?);
    }
    Ok(stages)
}

/// A validated [`G600Action`].
enum Change {
    Stage(G600DpiStage),
    ProfileDpi(G600Profile, G600DpiSettings, G600StageAfterStore),
}

impl TryFrom<G600Action> for Change {
    type Error = anyhow::Error;

    fn try_from(action: G600Action) -> Result<Self> {
        Ok(match action {
            G600Action::Stage { stage } => Self::Stage(stage),
            G600Action::Dpi {
                profile,
                stages,
                default,
                use_stage,
            } => Self::ProfileDpi(
                profile,
                G600DpiSettings::new(stages, default)
                    .context("these DPI settings cannot be stored")?,
                use_stage.map_or(G600StageAfterStore::Keep, G600StageAfterStore::Use),
            ),
        })
    }
}

pub async fn run(args: G600Args) -> Result<()> {
    // Refuse invalid settings before touching any device.
    let change = args.action.map(Change::try_from).transpose()?;
    // A request reaching the mouse while it stores a profile corrupts that
    // profile, so this must not interleave with the agent's G600 requests,
    // reads included.
    let _agent_lock = exclusive_hardware().await?;
    let devices: Vec<_> = openlogi_hid::enumerate_standalone()
        .await
        .context("enumerate standalone HID devices")?
        .into_iter()
        .filter(|device| device.driver_id == G600_DRIVER_ID)
        .collect();
    let device = select(&devices, args.device.as_deref(), "G600")?;
    let route = device.route();
    println!("device: {} ({route})", device.display_name);

    match change {
        None => {}
        Some(Change::Stage(stage)) => {
            openlogi_hid::set_g600_active_stage(&route, stage)
                .await
                .with_context(|| format!("select DPI stage {}", stage.number()))?;
        }
        Some(Change::ProfileDpi(profile, settings, after)) => {
            openlogi_hid::set_g600_profile_dpi(&route, profile, settings, after)
                .await
                .with_context(|| format!("store DPI settings in profile {}", profile.number()))?;
        }
    }
    print_state(&route).await
}

/// Read the G600 on `route` and print its active selection and profiles.
async fn print_state(route: &DeviceRoute) -> Result<()> {
    let dpi = openlogi_hid::read_g600_dpi(route)
        .await
        .context("read G600 DPI reports")?;
    println!(
        "active: profile {}, DPI stage {}",
        dpi.active.profile.number(),
        dpi.active.dpi_stage.number()
    );
    for (profile, settings) in G600Profile::ALL.into_iter().zip(&dpi.profiles) {
        println!("  {}", describe_profile(profile, settings, dpi.active));
    }
    Ok(())
}

/// One profile's DPI settings as a line, numbered from 1 like the mouse's own
/// software.
fn describe_profile(
    profile: G600Profile,
    settings: &G600ProfileDpi,
    active: G600ActiveSelection,
) -> String {
    let dpi = |value: Option<u16>| value.map_or_else(|| "off".to_owned(), |dpi| dpi.to_string());
    let stages = settings
        .stages
        .iter()
        .map(|stage| dpi(*stage))
        .collect::<Vec<_>>()
        .join(", ");
    let default = settings
        .default_stage
        .map_or_else(|| "none".to_owned(), |stage| stage.number().to_string());
    let marker = if active.profile == profile {
        "  [active]"
    } else {
        ""
    };
    format!(
        "profile {}: stages {stages}; default stage {default}; shift {}{marker}",
        profile.number(),
        dpi(settings.shift)
    )
}

#[cfg(test)]
mod tests {
    use super::{
        Change, G600Action, G600ActiveSelection, G600DpiStage, G600DpiValue, G600Profile,
        G600ProfileDpi, G600StageAfterStore, describe_profile,
    };

    #[test]
    fn a_profile_write_keeps_the_stage_unless_one_is_named() {
        let stage = |number| G600DpiStage::from_number(number).expect("a stage");
        let after = |use_stage| {
            let action = G600Action::Dpi {
                profile: G600Profile::ALL[0],
                stages: [G600DpiValue::new(400), G600DpiValue::new(800), None, None],
                default: stage(1),
                use_stage,
            };
            match Change::try_from(action).expect("storable settings") {
                Change::ProfileDpi(_, _, after) => after,
                Change::Stage(_) => panic!("a profile write"),
            }
        };

        assert_eq!(after(None), G600StageAfterStore::Keep);
        assert_eq!(after(Some(stage(2))), G600StageAfterStore::Use(stage(2)));
    }

    // Unix socket paths honor XDG_CONFIG_HOME; Windows uses a fixed pipe name
    // and cannot isolate this endpoint test from a real agent that way.
    #[cfg(unix)]
    #[test]
    fn the_agent_keeps_this_off_the_mouse() {
        use openlogi_core::single_instance::{self, InstanceError, Role};

        use super::{G600Args, run};

        const TEST: &str = "cmd::diag::g600::tests::the_agent_keeps_this_off_the_mouse";
        const MODE: &str = "OPENLOGI_DIAG_G600_EXCLUSION_TEST";
        if std::env::var_os(MODE).is_none() {
            let home = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env(MODE, "exercise")
                .env("XDG_CONFIG_HOME", home.path())
                .env_remove("XDG_RUNTIME_DIR")
                .env(
                    openlogi_core::env::PROFILE,
                    openlogi_core::paths::Profile::Production.env_value(),
                )
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        // Even if the guard let a call through, no G600 matches this name,
        // so nothing would reach a mouse.
        let args = || G600Args {
            device: Some("no such G600 in this test".to_owned()),
            action: None,
        };
        openlogi_core::worker::runtime().unwrap().block_on(async {
            {
                // An agent that has taken its lock but not yet bound its socket.
                let _agent = single_instance::acquire(Role::Agent).unwrap();
                let error = run(args()).await.expect_err("the agent holds its lock");
                assert!(
                    matches!(
                        error.downcast_ref::<InstanceError>(),
                        Some(InstanceError::AlreadyRunning { .. })
                    ),
                    "{error:#}"
                );
            }
            // An agent serving its endpoint, without its lock held here.
            let _listener = openlogi_ipc::transport::bind().unwrap();
            let error = run(args()).await.expect_err("the agent's endpoint is up");
            assert_eq!(
                error.to_string(),
                "the OpenLogi agent is running; quit it, then run this again"
            );
        });
    }

    #[test]
    fn a_profile_line_numbers_from_one_and_names_disabled_values() {
        let profile = G600ProfileDpi {
            stages: [Some(800), None, Some(3200), None],
            default_stage: G600DpiStage::from_number(1),
            shift: None,
        };
        let [first, second, _] = G600Profile::ALL;
        let active = G600ActiveSelection {
            profile: second,
            dpi_stage: G600DpiStage::from_number(1).expect("first stage"),
        };

        assert_eq!(
            describe_profile(second, &profile, active),
            "profile 2: stages 800, off, 3200, off; default stage 1; shift off  [active]"
        );
        let profile = G600ProfileDpi {
            default_stage: None,
            shift: Some(400),
            ..profile
        };
        assert_eq!(
            describe_profile(first, &profile, active),
            "profile 1: stages 800, off, 3200, off; default stage none; shift 400"
        );
    }
}
