//! Dump one thumb wheel's raw `0x2150` `thumbwheelEvent` reports.
//!
//! The wheel reports rotation, a capacitive `single_tap`, and where the report
//! sits in the life cycle of a roll (`rotation_status`, byte 4) — and firmware
//! does not always populate all three. This prints the wire bytes next to
//! their decoded form so a device's actual behavior can be checked against the
//! spec before trusting a field.
//!
//! Quit OpenLogi first: one HID node cannot serve two channels, and the agent
//! holds one whenever it captures.
//!
//! ```sh
//! cargo run -p openlogi-hid --example thumbwheel_trace -- <receiver-uid> <slot>
//! # e.g. the receiver id and slot printed by `openlogi list`
//! ```

use std::sync::Arc;
use std::time::Duration;

use openlogi_hid::thumbwheel::{self, Thumbwheel, WheelDirection};
use openlogi_hid::{ChannelPool, DeviceRoute, host};

/// Seconds the wheel stays diverted while reports are printed.
const WATCH: Duration = Duration::from_secs(30);

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(receiver_uid), Some(slot)) = (args.next(), args.next()) else {
        return Err("usage: thumbwheel_trace <receiver-uid> <slot>".into());
    };
    let slot: u8 = slot.parse()?;
    let route = DeviceRoute::Bolt { receiver_uid, slot };

    let pool = ChannelPool::with_backend(host::backend());
    let Some(chan) = pool.open(&route).await? else {
        return Err("no device on that route".into());
    };
    let device = hidpp::device::Device::new(Arc::clone(&chan), slot)
        .await
        .map_err(|e| format!("device did not answer HID++: {e:?}"))?;
    let Some(feature) = device
        .root()
        .get_feature(thumbwheel::FEATURE_ID)
        .await
        .map_err(|e| format!("{e:?}"))?
    else {
        return Err("this device exposes no 0x2150 thumb wheel".into());
    };

    let tw = Thumbwheel::new(Arc::clone(&chan), slot, feature.index);
    match tw.get_info().await {
        Ok(info) => println!("getThumbwheelInfo: {info:?}"),
        Err(e) => println!("getThumbwheelInfo failed: {e:?}"),
    }

    println!("0x2150 resolved at feature index {}", feature.index);
    let feature_index = feature.index;
    let listener = chan.add_msg_listener_guarded(move |raw, matched| {
        let msg = hidpp::protocol::v20::Message::from(raw);
        let header = msg.header();
        if header.feature_index != feature_index
            || header.software_id.to_lo() != 0
            || header.function_id.to_lo() != 0
        {
            // Everything else on this channel, so "no reports" can be told
            // apart from "reports arriving under something unexpected".
            println!(
                "   other: dev={} feat={} fn={} sw={} matched={matched}",
                header.device_index,
                header.feature_index,
                header.function_id.to_lo(),
                header.software_id.to_lo(),
            );
            return;
        }
        let p = msg.extend_payload();
        let decoded = thumbwheel::decode_event(&msg, slot, feature_index);
        println!(
            "rot={:>5}  byte4={:#04x}  byte5={:#04x}  tap={:<5} touch={:<5} proxy={:<5} → {:?}",
            i16::from_be_bytes([p[0], p[1]]),
            p[4],
            p[5],
            p[5] & 0x08 != 0,
            p[5] & 0x02 != 0,
            p[5] & 0x04 != 0,
            decoded.map(|e| e.rotation_status),
        );
    });

    tw.divert(WheelDirection::Default)
        .await
        .map_err(|e| format!("could not divert the wheel: {e:?}"))?;
    println!("\n>>> diverted — roll the wheel, then lift your thumb off it ({WATCH:?})\n");
    let mut elapsed = Duration::ZERO;
    while elapsed < WATCH {
        tokio::time::sleep(Duration::from_secs(5)).await;
        elapsed += Duration::from_secs(5);
        println!("   [{}s]", elapsed.as_secs());
    }

    drop(listener);
    if let Err(e) = tw.undivert().await {
        println!("could not restore native reporting: {e:?}");
    }
    println!("\n<<< native reporting restored");
    Ok(())
}
