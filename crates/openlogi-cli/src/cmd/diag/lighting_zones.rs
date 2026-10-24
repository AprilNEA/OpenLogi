//! `openlogi diag lighting-zones` — list the `0x8070` `ColorLedEffects`
//! zones of an RGB device and the effects each zone offers. Reads only.

use anyhow::Result;
use clap::Args;

use super::select_device;

const COLOR_LED_EFFECTS: &[u16] = &[0x8070];

#[derive(Debug, Args)]
pub struct LightingZonesArgs {
    /// Run against the online device whose name contains this string
    /// (case-insensitive). Useful when several devices are connected.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,
}

pub async fn run(args: LightingZonesArgs) -> Result<()> {
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
