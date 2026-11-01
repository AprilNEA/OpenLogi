//! Presentation code shared by OpenLogi's two GPUI processes: the settings app
//! (`openlogi-desktop`) and the Actions Ring overlay helper (`openlogi-overlay`).
//!
//! Only what both genuinely link belongs here. The overlay is a pure IPC client
//! with no settings UI, and this crate is what keeps that true: it depends on
//! `gpui` but not on `gpui-component`, so nothing the settings app pulls in can
//! reach the overlay through the back door.
//!
//! The agent links it too, with the default `gpui` feature off: it draws the
//! same action icons onto a keypad's LCD keys, so it reads the embedded glyph
//! bytes ([`action_icons::embedded_icon`]) without the GPUI asset source.

pub mod action_icons;
#[cfg(feature = "gpui")]
pub mod color;
