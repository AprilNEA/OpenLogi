//! HID++ `Gestures2` (feature `0x6501`) touchpad and legacy thumb-wheel controls.
//!
//! MX Master 2S exposes its horizontal thumb wheel as gesture id 46 under
//! `0x6501`, not through the newer dedicated `0x2150 Thumbwheel` feature.

use openlogi_hidpp_derive::Feature;

use crate::{feature::FeatureEndpoint, protocol::v20::Hidpp20Error};

/// Gestures2 gesture id for the horizontal thumb wheel.
pub const THUMBWHEEL_GESTURE_ID: u8 = 46;

// Gesture id and enable commands documented by Solaar's reverse engineering
// of Gestures2 (logitech_receiver/hidpp20_constants.py and hidpp20.py).
const NATURAL_SCROLL_GESTURE_ID: u8 = 45;

/// Maximum descriptor fields accepted before treating a malformed table as an
/// unsupported response. Real device tables are tiny; the bound prevents a
/// broken device from causing an unbounded probe loop.
const MAX_DESCRIPTOR_FIELDS: u16 = 1024;

/// Descriptor metadata needed to divert the legacy thumb wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThumbwheelGesture {
    /// Sequential index among gestures that advertise the divertable bit.
    /// `None` means gesture 46 exists but cannot be diverted.
    pub diversion_index: Option<u16>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DescriptorScan {
    Found(GestureIndices),
    End,
    Continue(DescriptorIndices),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DescriptorIndices {
    enable: u16,
    diversion: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GestureIndices {
    writable_enable: Option<u16>,
    diversion: Option<u16>,
}

fn scan_descriptor_page(payload: &[u8], id: u8, mut indices: DescriptorIndices) -> DescriptorScan {
    let (fields, _partial) = payload.as_chunks::<2>();
    for &[high, low] in fields.iter().take(8) {
        if high == 0x01 {
            return DescriptorScan::End;
        }
        if high & 0x80 == 0 {
            continue;
        }

        let writable = high & 0x01 != 0;
        let divertable = high & 0x02 != 0;
        let default_enabled = high & 0x20 != 0;
        if low == id {
            return DescriptorScan::Found(GestureIndices {
                writable_enable: writable.then_some(indices.enable),
                diversion: divertable.then_some(indices.diversion),
            });
        }
        if writable || default_enabled {
            indices.enable += 1;
        }
        if divertable {
            indices.diversion += 1;
        }
    }
    DescriptorScan::Continue(indices)
}

fn bit_address(index: u16) -> Result<(u8, u8), Hidpp20Error> {
    let offset = u8::try_from(index >> 3).map_err(|_| Hidpp20Error::UnsupportedResponse)?;
    let mask = 1u8 << u32::from(index & 7);
    Ok((offset, mask))
}

fn bit_write_payload(index: u16, value: bool) -> Result<[u8; 16], Hidpp20Error> {
    let (offset, mask) = bit_address(index)?;
    let mut payload = [0u8; 16];
    payload[..4].copy_from_slice(&[offset, 0x01, mask, if value { mask } else { 0 }]);
    Ok(payload)
}

/// Typed accessor for legacy `Gestures2` descriptor discovery.
#[derive(Clone, Feature)]
#[creatable(id = 0x6501, version = 0)]
pub struct Gestures2Feature {
    endpoint: FeatureEndpoint,
}

impl Gestures2Feature {
    /// Find gesture id 46 (Thumbwheel) and its diversion index. Merely exposing
    /// `0x6501` is not enough: touchpads and other gesture devices can expose
    /// the feature without a thumb wheel.
    pub async fn thumbwheel(&self) -> Result<Option<ThumbwheelGesture>, Hidpp20Error> {
        Ok(self
            .find_gesture(THUMBWHEEL_GESTURE_ID)
            .await?
            .map(|gesture| ThumbwheelGesture {
                diversion_index: gesture.diversion,
            }))
    }

    async fn find_gesture(&self, id: u8) -> Result<Option<GestureIndices>, Hidpp20Error> {
        let mut index = 0u16;
        let mut indices = DescriptorIndices::default();
        while index < MAX_DESCRIPTOR_FIELDS {
            let [hi, lo] = index.to_be_bytes();
            let payload = self.endpoint.call(0, [hi, lo, 0]).await?.extend_payload();
            match scan_descriptor_page(&payload, id, indices) {
                DescriptorScan::Found(gesture) => return Ok(Some(gesture)),
                DescriptorScan::End => return Ok(None),
                DescriptorScan::Continue(next) => {
                    indices = next;
                    index += 8;
                }
            }
        }
        Err(Hidpp20Error::UnsupportedResponse)
    }

    async fn natural_scroll_index(&self) -> Result<Option<u16>, Hidpp20Error> {
        Ok(self
            .find_gesture(NATURAL_SCROLL_GESTURE_ID)
            .await?
            .and_then(|gesture| gesture.writable_enable))
    }

    /// Read native natural scrolling. `None` means it cannot be changed.
    pub async fn natural_scroll(&self) -> Result<Option<bool>, Hidpp20Error> {
        let Some(index) = self.natural_scroll_index().await? else {
            return Ok(None);
        };
        self.read_bit(1, index).await.map(Some)
    }

    /// Change and verify native natural scrolling. Returns `false` if unsupported.
    pub async fn set_natural_scroll(&self, enabled: bool) -> Result<bool, Hidpp20Error> {
        let Some(index) = self.natural_scroll_index().await? else {
            return Ok(false);
        };
        if self.read_bit(1, index).await? != enabled {
            self.endpoint
                .call_long(2, bit_write_payload(index, enabled)?)
                .await?;
            if self.read_bit(1, index).await? != enabled {
                return Err(Hidpp20Error::UnsupportedResponse);
            }
        }
        Ok(true)
    }

    async fn read_bit(&self, function: u8, index: u16) -> Result<bool, Hidpp20Error> {
        let (offset, mask) = bit_address(index)?;
        let payload = self.endpoint.call(function, [offset, 0x01, mask]).await?;
        Ok(payload.extend_payload()[0] & mask != 0)
    }

    /// Return whether this device's descriptor table contains gesture id 46.
    pub async fn has_thumbwheel(&self) -> Result<bool, Hidpp20Error> {
        Ok(self.thumbwheel().await?.is_some())
    }

    /// Read the current diversion state for gesture id 46. `None` means the
    /// thumb wheel is absent or present but not divertable.
    pub async fn thumbwheel_diverted(&self) -> Result<Option<bool>, Hidpp20Error> {
        let Some(index) = self.thumbwheel().await?.and_then(|g| g.diversion_index) else {
            return Ok(None);
        };
        self.read_bit(3, index).await.map(Some)
    }

    /// Divert or restore gesture id 46. Returns `false` when the thumb wheel is
    /// absent or not divertable. Function 4 is the `0x40` Gestures2 diversion
    /// write; its four-byte body requires a long HID++ report.
    pub async fn set_thumbwheel_diverted(&self, diverted: bool) -> Result<bool, Hidpp20Error> {
        let Some(index) = self.thumbwheel().await?.and_then(|g| g.diversion_index) else {
            return Ok(false);
        };
        self.endpoint
            .call_long(4, bit_write_payload(index, diverted)?)
            .await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_page_detects_thumbwheel_and_end_marker() {
        let mut payload = [0u8; 16];
        payload[0] = 0x83; // gesture + enabled + divertable
        payload[1] = THUMBWHEEL_GESTURE_ID;
        assert_eq!(
            scan_descriptor_page(
                &payload,
                THUMBWHEEL_GESTURE_ID,
                DescriptorIndices::default()
            ),
            DescriptorScan::Found(GestureIndices {
                writable_enable: Some(0),
                diversion: Some(0)
            })
        );

        let mut end = [0u8; 16];
        end[0] = 0x01;
        assert_eq!(
            scan_descriptor_page(&end, THUMBWHEEL_GESTURE_ID, DescriptorIndices::default()),
            DescriptorScan::End
        );
    }

    #[test]
    fn descriptor_page_ignores_other_gestures() {
        let mut payload = [0u8; 16];
        payload[0] = 0x83;
        payload[1] = 45; // natural scrolling
        assert_eq!(
            scan_descriptor_page(
                &payload,
                THUMBWHEEL_GESTURE_ID,
                DescriptorIndices::default()
            ),
            DescriptorScan::Continue(DescriptorIndices {
                enable: 1,
                diversion: 1
            })
        );
    }

    #[test]
    fn descriptor_page_counts_divertable_gestures_before_thumbwheel() {
        let mut payload = [0u8; 16];
        payload[0] = 0x82; // divertable gesture
        payload[1] = 40;
        payload[2] = 0x80; // gesture, not divertable
        payload[3] = 41;
        payload[4] = 0x82; // divertable gesture
        payload[5] = THUMBWHEEL_GESTURE_ID;

        assert_eq!(
            scan_descriptor_page(
                &payload,
                THUMBWHEEL_GESTURE_ID,
                DescriptorIndices {
                    enable: 0,
                    diversion: 3
                }
            ),
            DescriptorScan::Found(GestureIndices {
                writable_enable: None,
                diversion: Some(4)
            })
        );
    }

    #[test]
    fn bit_write_payload_uses_offset_mask_and_value() {
        let enabled = bit_write_payload(9, true).unwrap();
        assert_eq!(&enabled[..4], &[1, 1, 2, 2]);

        let disabled = bit_write_payload(9, false).unwrap();
        assert_eq!(&disabled[..4], &[1, 1, 2, 0]);
    }

    #[test]
    fn natural_scroll_counts_default_enabled_gestures_separately_from_diversion() {
        let payload = [
            0xa0,
            20, // default-enabled, read-only
            0x82,
            44, // divertable only
            0x81,
            NATURAL_SCROLL_GESTURE_ID,
        ];
        assert_eq!(
            scan_descriptor_page(
                &payload,
                NATURAL_SCROLL_GESTURE_ID,
                DescriptorIndices {
                    enable: 7,
                    diversion: 3
                }
            ),
            DescriptorScan::Found(GestureIndices {
                writable_enable: Some(8),
                diversion: None
            })
        );
    }
}
