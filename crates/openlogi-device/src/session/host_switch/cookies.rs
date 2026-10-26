//! The link that Logi Options+ stores between a keyboard and the pointing
//! devices that follow it, in the `ChangeHost` (`0x1814`) cookies of each
//! pointing device.
//!
//! REVERSE-ENGINEERED from Logi Options+ 2.8 (`feature_1814_change_host`):
//!
//! - Options+ links a pointing device to a keyboard: it writes the
//!   [`LeadCookie`] of the keyboard into every host slot cookie of the
//!   pointing device (`setCookie`, function 3). It unlinks the device: it
//!   writes 0 into every slot.
//! - When the keyboard announces a host change, Options+ moves the pointing
//!   device only if every cookie of the device equals the keyboard's byte.
//! - When the pointing device connects and some cookies are 0 while others
//!   are not, Options+ copies the non-zero byte into the zero slots. The
//!   firmware clears the cookie of a slot that is paired to a new host.
//!
//! OpenLogi moves linked devices for its own config, not for cookies. It
//! writes the same bytes so that Options+ on the keyboard's other hosts moves
//! the same devices. Unlike Options+, OpenLogi never replaces the byte of
//! another keyboard, and on unlink it clears only the byte of the keyboard
//! that the config no longer links.

use std::{num::NonZeroU8, sync::Arc};

use hidpp::{
    device::Device,
    feature::{
        CreatableFeature,
        change_host::{ANNOUNCEMENT_MIN_VERSION, ChangeHostFeature},
    },
};

use super::{HostSwitchError, timed_hidpp};
use crate::SharedChannel;

/// The byte that Logi Options+ writes into the cookies of a pointing device
/// to link it to one keyboard.
///
/// The byte is an 8-bit hash of the keyboard's serial number: the unit ID
/// from `DeviceInformation` (`0x0003`) as a big-endian `u32`, written in
/// decimal. Each ASCII digit is XORed into the hash, and the hash is then
/// rotated left by 3 bits (reverse-engineered, `assist_couple_switch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeadCookie(NonZeroU8);

impl LeadCookie {
    /// The byte for the keyboard whose `0x0003` unit ID is `unit_id`.
    ///
    /// Returns `None` for a zero unit ID, which names no keyboard, and for a
    /// unit ID whose hash is 0: Options+ reads a zero cookie as "not linked".
    #[must_use]
    pub fn for_unit_id(unit_id: [u8; 4]) -> Option<Self> {
        let serial = u32::from_be_bytes(unit_id);
        if serial == 0 {
            return None;
        }
        let hash = serial
            .to_string()
            .bytes()
            .fold(0_u8, |hash, digit| (hash ^ digit).rotate_left(3));
        NonZeroU8::new(hash).map(Self)
    }

    /// The cookie byte.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0.get()
    }
}

/// What [`link_follower_cookies_on`] found and wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowerLink {
    /// Every cookie already held the keyboard's byte.
    AlreadyLinked,
    /// OpenLogi wrote the keyboard's byte into this many slots that held 0.
    Linked {
        /// The number of cookies written.
        slots: usize,
    },
    /// A cookie holds the byte of another keyboard, so OpenLogi wrote
    /// nothing.
    LinkedToAnotherKeyboard {
        /// The byte that the device holds.
        cookie: u8,
    },
    /// The device's `ChangeHost` is older than version 2. Options+ asks for a
    /// firmware update before it links such a device, so OpenLogi writes
    /// nothing.
    Unsupported,
}

/// What [`release_follower_cookies_on`] found and wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowerRelease {
    /// No cookie held the keyboard's byte.
    NotLinked,
    /// OpenLogi wrote 0 into this many slots that held the keyboard's byte.
    Released {
        /// The number of cookies written.
        slots: usize,
    },
    /// The device's `ChangeHost` is older than version 2.
    Unsupported,
}

/// Link the pointing device on `shared` to the keyboard of `lead`.
///
/// The keyboard's byte goes into each cookie that holds 0, and only when no
/// cookie holds the byte of another keyboard. A cookie that already holds the
/// byte is not written again.
///
/// # Errors
///
/// Returns [`HostSwitchError`] when the device does not answer, a HID++
/// request fails, or a request takes longer than its budget. Writes made
/// before the failure stay on the device.
pub async fn link_follower_cookies_on(
    shared: &SharedChannel,
    lead: LeadCookie,
) -> Result<FollowerLink, HostSwitchError> {
    let Some(cookies) = FollowerCookies::open(shared).await? else {
        return Ok(FollowerLink::Unsupported);
    };
    let slots = match link_slots(&cookies.values, lead) {
        LinkPlan::Linked => return Ok(FollowerLink::AlreadyLinked),
        LinkPlan::LinkedToAnotherKeyboard(cookie) => {
            return Ok(FollowerLink::LinkedToAnotherKeyboard { cookie });
        }
        LinkPlan::Fill(slots) => slots,
    };
    cookies.write(&slots, lead.get()).await?;
    Ok(FollowerLink::Linked { slots: slots.len() })
}

/// Clear the link between the pointing device on `shared` and the keyboard
/// of `lead`: each cookie that holds the keyboard's byte gets 0. Cookies of
/// other keyboards stay.
///
/// # Errors
///
/// As [`link_follower_cookies_on`].
pub async fn release_follower_cookies_on(
    shared: &SharedChannel,
    lead: LeadCookie,
) -> Result<FollowerRelease, HostSwitchError> {
    let Some(cookies) = FollowerCookies::open(shared).await? else {
        return Ok(FollowerRelease::Unsupported);
    };
    let slots = release_slots(&cookies.values, lead);
    if slots.is_empty() {
        return Ok(FollowerRelease::NotLinked);
    }
    cookies.write(&slots, 0).await?;
    Ok(FollowerRelease::Released { slots: slots.len() })
}

/// The cookies of one pointing device, read once.
struct FollowerCookies {
    feature: Arc<ChangeHostFeature>,
    values: Vec<u8>,
}

impl FollowerCookies {
    /// `None` when the device has no `ChangeHost` of version 2 or later.
    async fn open(shared: &SharedChannel) -> Result<Option<Self>, HostSwitchError> {
        let mut device = timed_hidpp(
            "opening linked device",
            Device::new(Arc::clone(shared.channel()), shared.device_index()),
        )
        .await?;
        let info = timed_hidpp(
            "locating host-change feature",
            device.root().get_feature(ChangeHostFeature::ID),
        )
        .await?;
        let Some(info) = info.filter(|info| info.version >= ANNOUNCEMENT_MIN_VERSION) else {
            return Ok(None);
        };
        let feature = device.add_feature::<ChangeHostFeature>(info.index);
        let hosts = timed_hidpp("reading host count", feature.get_host_info()).await?;
        let values = timed_hidpp(
            "reading follower cookies",
            feature.get_cookies(hosts.host_count),
        )
        .await?;
        Ok(Some(Self { feature, values }))
    }

    async fn write(&self, slots: &[u8], cookie: u8) -> Result<(), HostSwitchError> {
        for &slot in slots {
            timed_hidpp(
                "writing follower cookie",
                self.feature.set_cookie(slot, cookie),
            )
            .await?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
enum LinkPlan {
    Linked,
    LinkedToAnotherKeyboard(u8),
    Fill(Vec<u8>),
}

fn link_slots(cookies: &[u8], lead: LeadCookie) -> LinkPlan {
    if let Some(&other) = cookies
        .iter()
        .find(|&&cookie| cookie != 0 && cookie != lead.get())
    {
        return LinkPlan::LinkedToAnotherKeyboard(other);
    }
    let empty = slots_holding(cookies, 0);
    if empty.is_empty() {
        LinkPlan::Linked
    } else {
        LinkPlan::Fill(empty)
    }
}

fn release_slots(cookies: &[u8], lead: LeadCookie) -> Vec<u8> {
    slots_holding(cookies, lead.get())
}

fn slots_holding(cookies: &[u8], value: u8) -> Vec<u8> {
    (0..=u8::MAX)
        .zip(cookies)
        .filter(|&(_, &cookie)| cookie == value)
        .map(|(slot, _)| slot)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{LeadCookie, LinkPlan, link_slots, release_slots};

    fn lead(byte: u8) -> LeadCookie {
        LeadCookie(std::num::NonZeroU8::new(byte).expect("test cookies are non-zero"))
    }

    #[test]
    fn hashes_the_decimal_unit_id_as_options_plus_does() {
        // Expected bytes computed with the decompiled Options+ loop
        // `h = ((c ^ h) >> 5 & 7) | ((c ^ h) << 3)` over the decimal digits.
        for (unit_id, byte) in [
            ([0x00, 0x00, 0x00, 0x01], 137),
            ([0x12, 0x34, 0x56, 0x78], 91),
            ([0xde, 0xad, 0xbe, 0xef], 112),
            ([0x8a, 0x3f, 0x10, 0x02], 80),
        ] {
            assert_eq!(
                LeadCookie::for_unit_id(unit_id).map(LeadCookie::get),
                Some(byte),
                "{unit_id:02x?}"
            );
        }
    }

    #[test]
    fn a_unit_id_that_cannot_be_told_from_unlinked_has_no_cookie() {
        assert_eq!(LeadCookie::for_unit_id([0; 4]), None, "zero unit ID");
        // 275 hashes to 0, which Options+ reads as "not linked".
        assert_eq!(LeadCookie::for_unit_id([0, 0, 1, 19]), None, "zero hash");
    }

    #[test]
    fn linking_fills_only_zero_slots() {
        assert_eq!(
            link_slots(&[0, 0, 0], lead(7)),
            LinkPlan::Fill(vec![0, 1, 2])
        );
        assert_eq!(link_slots(&[7, 0, 7], lead(7)), LinkPlan::Fill(vec![1]));
        assert_eq!(link_slots(&[7, 7, 7], lead(7)), LinkPlan::Linked);
    }

    #[test]
    fn linking_never_replaces_another_keyboard() {
        assert_eq!(
            link_slots(&[0, 9, 7], lead(7)),
            LinkPlan::LinkedToAnotherKeyboard(9)
        );
        assert_eq!(
            link_slots(&[9, 9, 9], lead(7)),
            LinkPlan::LinkedToAnotherKeyboard(9)
        );
    }

    #[test]
    fn releasing_clears_only_the_keyboard_byte() {
        assert_eq!(release_slots(&[7, 9, 7], lead(7)), vec![0, 2]);
        assert_eq!(release_slots(&[0, 9, 0], lead(7)), Vec::<u8>::new());
    }
}
