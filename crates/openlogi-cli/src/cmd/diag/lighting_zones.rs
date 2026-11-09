//! `openlogi diag lighting-zones` — list the `0x8070` `ColorLedEffects`
//! zones of an RGB device and the effects each zone offers. Reads only, but
//! opens the device directly, so the agent must be off it.

use std::future::Future;

use anyhow::Result;
use clap::Args;

use super::{exclude_agent, select_device};

const COLOR_LED_EFFECTS: &[u16] = &[0x8070];

#[derive(Debug, Args)]
pub struct LightingZonesArgs {
    /// Run against the online device whose name contains this string
    /// (case-insensitive). Useful when several devices are connected.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: LightingZonesArgs) -> Result<()> {
    // Boxed: the agent handshake and the device walk together make a future
    // too large to keep on the stack.
    Box::pin(list_zones(args, exclude_agent())).await
}

/// The listing, once `admission` has kept the agent off the device. Nothing
/// touches the hardware before it is granted, and its guard is held until
/// the last read.
async fn list_zones<G>(
    args: LightingZonesArgs,
    admission: impl Future<Output = Result<G>>,
) -> Result<()> {
    let _agent = admission.await?;
    let (route, name) = select_device(args.device.as_deref(), COLOR_LED_EFFECTS).await?;
    let (info, zones) = openlogi_hid::dump_color_led_zones(&route).await?;
    println!("device: {name} ({route})");
    println!(
        "  0x8070: {} zone(s), nv_caps={:?} ext_caps={:?}",
        info.zone_count, info.nv_capabilities, info.ext_capabilities
    );
    for zone in &zones {
        match zone.info {
            Some(zone_info) => println!(
                "  zone {}: {:?}, {} effect(s), persistency={:?}",
                zone.zone, zone_info.location, zone_info.effects_number, zone_info.persistency
            ),
            None => println!("  zone {}: unmodelled location", zone.zone),
        }
        for effect in &zone.effects {
            println!(
                "    effect {:>2}: {:?} (capabilities={:#06x}, period={} ms)",
                effect.zone_effect_index,
                effect.effect_id,
                effect.effect_capabilities,
                effect.effect_period
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::future::{Ready, ready};
    use std::io;
    use std::path::PathBuf;

    use openlogi_core::single_instance::InstanceError;
    use openlogi_ipc::client::ConnectError;

    use super::*;
    use crate::cmd::diag::admit_direct_run;

    fn args() -> LightingZonesArgs {
        LightingZonesArgs {
            device: Some("no device has this name".into()),
        }
    }

    fn lock_held_elsewhere() -> std::result::Result<(), InstanceError> {
        Err(InstanceError::AlreadyRunning {
            path: PathBuf::from("agent.lock"),
        })
    }

    fn endpoint(
        answer: std::result::Result<(), ConnectError>,
    ) -> Ready<std::result::Result<(), ConnectError>> {
        ready(answer)
    }

    /// Both refusals end the command before device discovery: the error is
    /// the admission's own, not a failed device lookup.
    #[tokio::test]
    async fn lighting_zones_agent_exclusion() {
        let refused = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let starting_agent = list_zones(
                args(),
                admit_direct_run::<(), ()>(lock_held_elsewhere(), async {
                    panic!("the endpoint is probed only once the lock is held")
                }),
            )
            .await;
            let running_agent =
                list_zones(args(), admit_direct_run(Ok(()), endpoint(Ok(())))).await;
            (starting_agent, running_agent)
        })
        .await
        .expect("a refusal answers within 5 s");

        let starting_agent = refused.0.expect_err("a held lock refuses the run");
        assert!(
            format!("{starting_agent:#}").contains("instance lock"),
            "{starting_agent:#}"
        );
        let running_agent = refused.1.expect_err("an answering agent refuses the run");
        assert!(
            running_agent.to_string().contains("agent is running"),
            "{running_agent:#}"
        );
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_admits_the_run_with_the_lock_held() {
        let unreachable = ConnectError::Endpoint(io::ErrorKind::NotFound.into());
        let admitted = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            admit_direct_run(Ok("guard"), endpoint(Err(unreachable))),
        )
        .await
        .expect("the admission answers within 5 s");
        assert_eq!(admitted.expect("no agent, so the run goes ahead"), "guard");
    }
}
