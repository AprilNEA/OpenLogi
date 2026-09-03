//! Legacy HID++ `Gestures2` (feature `0x6501`) discovery used by older MX mice
//! and touchpads.
//!
//! MX Master 2S exposes its horizontal thumb wheel as gesture id 46 under
//! `0x6501`, not through the newer dedicated `0x2150 Thumbwheel` feature.

use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// Gestures2 gesture id for the horizontal thumb wheel.
pub const THUMBWHEEL_GESTURE_ID: u8 = 46;

/// Maximum descriptor fields accepted before treating a malformed table as an
/// unsupported response. Real device tables are tiny; the bound prevents a
/// broken device from causing an unbounded probe loop.
const MAX_DESCRIPTOR_FIELDS: u16 = 1024;

/// Descriptor metadata needed to divert one gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GestureDiversion {
    /// The gesture id this record was resolved for.
    pub gesture_id: u8,
    /// Sequential index among gestures that advertise the divertable bit.
    /// `None` means the gesture exists but cannot be diverted.
    pub diversion_index: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DescriptorScan {
    Found(GestureDiversion),
    End,
    Continue { next_diversion_index: u16 },
}

fn scan_descriptor_page(payload: &[u8], mut diversion_index: u16, target: u8) -> DescriptorScan {
    let (fields, _partial) = payload.as_chunks::<2>();
    for &[high, low] in fields.iter().take(8) {
        if high == 0x01 {
            return DescriptorScan::End;
        }
        if high & 0x80 == 0 {
            continue;
        }

        let divertable = high & 0x02 != 0;
        if low == target {
            return DescriptorScan::Found(GestureDiversion {
                gesture_id: target,
                diversion_index: divertable.then_some(diversion_index),
            });
        }
        if divertable {
            diversion_index = diversion_index.saturating_add(1);
        }
    }
    DescriptorScan::Continue {
        next_diversion_index: diversion_index,
    }
}

fn diversion_address(index: u16) -> Result<(u8, u8), Hidpp20Error> {
    let offset = u8::try_from(index >> 3).map_err(|_| Hidpp20Error::UnsupportedResponse)?;
    let mask = 1u8 << u32::from(index & 7);
    Ok((offset, mask))
}

fn diversion_write_payload(index: u16, diverted: bool) -> Result<[u8; 16], Hidpp20Error> {
    let (offset, mask) = diversion_address(index)?;
    let mut payload = [0u8; 16];
    payload[..4].copy_from_slice(&[offset, 0x01, mask, if diverted { mask } else { 0 }]);
    Ok(payload)
}

/// Typed accessor for legacy `Gestures2` descriptor discovery.
#[derive(Clone, Feature)]
#[creatable(id = 0x6501, version = 0)]
pub struct Gestures2Feature {
    endpoint: FeatureEndpoint,
}

impl Gestures2Feature {
    /// Find one gesture id and its diversion index. Merely exposing `0x6501`
    /// is not enough: gesture devices differ in which ids their descriptor
    /// table carries.
    pub async fn gesture(&self, gesture_id: u8) -> Result<Option<GestureDiversion>, Hidpp20Error> {
        let mut index = 0u16;
        let mut diversion_index = 0u16;
        while index < MAX_DESCRIPTOR_FIELDS {
            let [hi, lo] = index.to_be_bytes();
            let payload = self.endpoint.call(0, [hi, lo, 0]).await?.extend_payload();
            match scan_descriptor_page(&payload, diversion_index, gesture_id) {
                DescriptorScan::Found(gesture) => return Ok(Some(gesture)),
                DescriptorScan::End => return Ok(None),
                DescriptorScan::Continue {
                    next_diversion_index,
                } => {
                    diversion_index = next_diversion_index;
                    index = index.saturating_add(8);
                }
            }
        }
        Err(Hidpp20Error::UnsupportedResponse)
    }

    /// Return whether this device's descriptor table contains gesture id 46.
    pub async fn has_thumbwheel(&self) -> Result<bool, Hidpp20Error> {
        Ok(self.thumbwheel().await?.is_some())
    }

    /// Find gesture id 46 (Thumbwheel) and its diversion index.
    pub async fn thumbwheel(&self) -> Result<Option<GestureDiversion>, Hidpp20Error> {
        self.gesture(THUMBWHEEL_GESTURE_ID).await
    }

    /// Read the current diversion state for one gesture id. `None` means the
    /// gesture is absent or present but not divertable.
    pub async fn gesture_diverted(&self, gesture_id: u8) -> Result<Option<bool>, Hidpp20Error> {
        let Some(index) = self
            .gesture(gesture_id)
            .await?
            .and_then(|g| g.diversion_index)
        else {
            return Ok(None);
        };
        let (offset, mask) = diversion_address(index)?;
        let payload = self
            .endpoint
            .call(3, [offset, 0x01, mask])
            .await?
            .extend_payload();
        Ok(Some(payload[0] & mask != 0))
    }

    /// Divert or restore one gesture id. Returns `false` when the gesture is
    /// absent or not divertable. Function 4 is the `0x40` Gestures2 diversion
    /// write; its four-byte body requires a long HID++ report.
    pub async fn set_gesture_diverted(
        &self,
        gesture_id: u8,
        diverted: bool,
    ) -> Result<bool, Hidpp20Error> {
        let Some(index) = self
            .gesture(gesture_id)
            .await?
            .and_then(|g| g.diversion_index)
        else {
            return Ok(false);
        };
        self.endpoint
            .call_long(4, diversion_write_payload(index, diverted)?)
            .await?;
        Ok(true)
    }

    /// Read the current diversion state for gesture id 46. `None` means the
    /// thumb wheel is absent or present but not divertable.
    pub async fn thumbwheel_diverted(&self) -> Result<Option<bool>, Hidpp20Error> {
        self.gesture_diverted(THUMBWHEEL_GESTURE_ID).await
    }

    /// Divert or restore gesture id 46. Returns `false` when the thumb wheel is
    /// absent or not divertable.
    pub async fn set_thumbwheel_diverted(&self, diverted: bool) -> Result<bool, Hidpp20Error> {
        self.set_gesture_diverted(THUMBWHEEL_GESTURE_ID, diverted)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_page_detects_target_and_end_marker() {
        let mut payload = [0u8; 16];
        payload[0] = 0x83; // gesture + enabled + divertable
        payload[1] = THUMBWHEEL_GESTURE_ID;
        assert_eq!(
            scan_descriptor_page(&payload, 0, THUMBWHEEL_GESTURE_ID),
            DescriptorScan::Found(GestureDiversion {
                gesture_id: THUMBWHEEL_GESTURE_ID,
                diversion_index: Some(0)
            })
        );

        let mut end = [0u8; 16];
        end[0] = 0x01;
        assert_eq!(
            scan_descriptor_page(&end, 0, THUMBWHEEL_GESTURE_ID),
            DescriptorScan::End
        );
    }

    #[test]
    fn descriptor_page_ignores_other_gestures() {
        let mut payload = [0u8; 16];
        payload[0] = 0x83;
        payload[1] = 45; // natural scrolling
        assert_eq!(
            scan_descriptor_page(&payload, 0, THUMBWHEEL_GESTURE_ID),
            DescriptorScan::Continue {
                next_diversion_index: 1
            }
        );
    }

    #[test]
    fn descriptor_page_counts_divertable_gestures_before_target() {
        let mut payload = [0u8; 16];
        payload[0] = 0x82; // divertable gesture
        payload[1] = 40;
        payload[2] = 0x80; // gesture, not divertable
        payload[3] = 41;
        payload[4] = 0x82; // divertable gesture
        payload[5] = THUMBWHEEL_GESTURE_ID;

        assert_eq!(
            scan_descriptor_page(&payload, 3, THUMBWHEEL_GESTURE_ID),
            DescriptorScan::Found(GestureDiversion {
                gesture_id: THUMBWHEEL_GESTURE_ID,
                diversion_index: Some(4)
            })
        );
    }

    #[test]
    fn diversion_write_payload_uses_offset_mask_and_value() {
        let enabled = diversion_write_payload(9, true).unwrap();
        assert_eq!(&enabled[..4], &[1, 1, 2, 2]);

        let disabled = diversion_write_payload(9, false).unwrap();
        assert_eq!(&disabled[..4], &[1, 1, 2, 0]);
    }
}
