//! Character → key-stroke lookup for `TypeText`.
//!
//! uinput carries key codes; the compositor turns them into characters with
//! its own keymap and active layout. So the reverse map is built from that
//! same keymap, fetched from the Wayland compositor (`wl_keyboard.keymap`),
//! with the active layout index read from KDE's `org.kde.keyboard` service
//! (layout 0 elsewhere). Without a Wayland keymap it falls back to US QWERTY.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::os::fd::OwnedFd;

use evdev::KeyCode;
use wayland_client::protocol::wl_keyboard::{self, KeymapFormat, WlKeyboard};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use xkbcommon::xkb;

use super::{KEY_CAPABILITIES, SESSION_BUS};

/// Modifier keys to hold, then the key to press.
pub(super) type Stroke = (&'static [KeyCode], KeyCode);

const NONE: &[KeyCode] = &[];
const SHIFT: &[KeyCode] = &[KeyCode::KEY_LEFTSHIFT];
const ALTGR: &[KeyCode] = &[KeyCode::KEY_RIGHTALT];
const SHIFT_ALTGR: &[KeyCode] = &[KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_RIGHTALT];

/// X keycodes are evdev codes offset by 8.
const EVDEV_OFFSET: u32 = 8;

pub(super) enum Layout {
    Xkb(HashMap<char, Stroke>),
    UsQwerty,
}

impl Layout {
    /// The compositor's current layout, re-read on every call so a layout
    /// switch takes effect on the next `TypeText`.
    pub(super) fn current() -> Self {
        if let Some(map) = compositor_layout() {
            tracing::debug!(chars = map.len(), "TypeText using the compositor keymap");
            Self::Xkb(map)
        } else {
            tracing::debug!("TypeText falling back to US QWERTY");
            Self::UsQwerty
        }
    }

    pub(super) fn stroke(&self, ch: char) -> Option<Stroke> {
        // Return/Tab produce control characters (`\r`, `\t`) the reverse map
        // skips, so text's line breaks are mapped directly.
        match ch {
            '\n' => return Some((NONE, KeyCode::KEY_ENTER)),
            '\t' => return Some((NONE, KeyCode::KEY_TAB)),
            _ => {}
        }
        match self {
            Self::Xkb(map) => map.get(&ch).copied(),
            Self::UsQwerty => us_qwerty(ch),
        }
    }
}

fn compositor_layout() -> Option<HashMap<char, Stroke>> {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    let keymap = wayland_keymap(&context)?;
    let layout = kde_active_layout()
        .filter(|&index| index < keymap.num_layouts())
        .unwrap_or(0);
    Some(reverse_map(&keymap, layout))
}

/// Every character the virtual device can produce in `layout`, with the
/// fewest modifiers that produce it.
fn reverse_map(keymap: &xkb::Keymap, layout: xkb::LayoutIndex) -> HashMap<char, Stroke> {
    let mut state = xkb::State::new(keymap);
    let mask = |name: &str| match keymap.mod_get_index(name) {
        xkb::MOD_INVALID => None,
        index => Some(1 << index),
    };
    let shift = mask(xkb::MOD_NAME_SHIFT);
    let level3 = mask(xkb::MOD_NAME_ISO_LEVEL3_SHIFT);

    // AltGr strokes hold KEY_RIGHTALT, so only offer them when that key
    // really is the level-3 shift in this layout.
    state.update_mask(0, 0, 0, 0, 0, layout);
    let altgr_is_level3 =
        state.key_get_one_sym(xkb_keycode(KeyCode::KEY_RIGHTALT)) == xkb::Keysym::ISO_Level3_Shift;
    let level3 = level3.filter(|_| altgr_is_level3);

    let combos = [
        (NONE, Some(0)),
        (SHIFT, shift),
        (ALTGR, level3),
        (SHIFT_ALTGR, shift.zip(level3).map(|(s, l)| s | l)),
    ];

    let mut map = HashMap::new();
    for (mods, mask) in combos {
        let Some(mask) = mask else { continue };
        state.update_mask(mask, 0, 0, 0, 0, layout);
        for &key in KEY_CAPABILITIES {
            let Some(ch) = char::from_u32(state.key_get_utf32(xkb_keycode(key))) else {
                continue;
            };
            if ch != '\0' && !ch.is_control() {
                map.entry(ch).or_insert((mods, key));
            }
        }
    }
    map
}

fn xkb_keycode(key: KeyCode) -> xkb::Keycode {
    xkb::Keycode::new(u32::from(key.0) + EVDEV_OFFSET)
}

/// KWin's active layout index; `None` off KDE.
fn kde_active_layout() -> Option<u32> {
    let bus = SESSION_BUS.as_ref()?;
    let reply = bus
        .call_method(
            Some("org.kde.keyboard"),
            "/Layouts",
            Some("org.kde.KeyboardLayouts"),
            "getLayout",
            &(),
        )
        .ok()?;
    reply.body().deserialize::<u32>().ok()
}

#[derive(Default)]
struct KeymapFetch {
    seat_bound: bool,
    keymap: Option<(OwnedFd, u32)>,
}

/// The compositor's keymap as sent to any `wl_keyboard`. Needs no focus or
/// surface: the keymap event follows `get_keyboard` unconditionally.
fn wayland_keymap(context: &xkb::Context) -> Option<xkb::Keymap> {
    let connection = Connection::connect_to_env()
        .map_err(|e| tracing::debug!("no Wayland connection for TypeText keymap: {e}"))
        .ok()?;
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());

    let mut fetch = KeymapFetch::default();
    // First roundtrip delivers the globals (binding the seat and requesting
    // its keyboard); the second delivers the keyboard's keymap.
    queue.roundtrip(&mut fetch).ok()?;
    queue.roundtrip(&mut fetch).ok()?;

    let (fd, size) = fetch.keymap?;
    let mut bytes = Vec::with_capacity(size as usize);
    File::from(fd)
        .take(u64::from(size))
        .read_to_end(&mut bytes)
        .ok()?;
    // The keymap is NUL-terminated; CString conversion rejects interior NULs.
    if let Some(end) = bytes.iter().position(|&b| b == 0) {
        bytes.truncate(end);
    }
    let text = String::from_utf8(bytes).ok()?;
    xkb::Keymap::new_from_string(
        context,
        text,
        xkb::KEYMAP_FORMAT_TEXT_V1,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
}

impl Dispatch<WlRegistry, ()> for KeymapFetch {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == "wl_seat"
            && !state.seat_bound
        {
            state.seat_bound = true;
            let seat: WlSeat = registry.bind(name, version.min(5), qh, ());
            seat.get_keyboard(qh, ());
        }
    }
}

impl Dispatch<WlSeat, ()> for KeymapFetch {
    fn event(
        _: &mut Self,
        _: &WlSeat,
        _: <WlSeat as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlKeyboard, ()> for KeymapFetch {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Keymap {
            format: WEnum::Value(KeymapFormat::XkbV1),
            fd,
            size,
        } = event
        {
            state.keymap = Some((fd, size));
        }
    }
}

/// The US QWERTY stroke for `ch`.
fn us_qwerty(ch: char) -> Option<Stroke> {
    const LETTERS: [KeyCode; 26] = [
        KeyCode::KEY_A,
        KeyCode::KEY_B,
        KeyCode::KEY_C,
        KeyCode::KEY_D,
        KeyCode::KEY_E,
        KeyCode::KEY_F,
        KeyCode::KEY_G,
        KeyCode::KEY_H,
        KeyCode::KEY_I,
        KeyCode::KEY_J,
        KeyCode::KEY_K,
        KeyCode::KEY_L,
        KeyCode::KEY_M,
        KeyCode::KEY_N,
        KeyCode::KEY_O,
        KeyCode::KEY_P,
        KeyCode::KEY_Q,
        KeyCode::KEY_R,
        KeyCode::KEY_S,
        KeyCode::KEY_T,
        KeyCode::KEY_U,
        KeyCode::KEY_V,
        KeyCode::KEY_W,
        KeyCode::KEY_X,
        KeyCode::KEY_Y,
        KeyCode::KEY_Z,
    ];
    const DIGITS: [KeyCode; 10] = [
        KeyCode::KEY_0,
        KeyCode::KEY_1,
        KeyCode::KEY_2,
        KeyCode::KEY_3,
        KeyCode::KEY_4,
        KeyCode::KEY_5,
        KeyCode::KEY_6,
        KeyCode::KEY_7,
        KeyCode::KEY_8,
        KeyCode::KEY_9,
    ];
    // Shifted digit row: `)` is Shift+0, `!` is Shift+1, …
    const SHIFTED_DIGITS: &str = ")!@#$%^&*(";

    if ch.is_ascii_lowercase() {
        return Some((NONE, LETTERS[usize::from(ch as u8 - b'a')]));
    }
    if ch.is_ascii_uppercase() {
        return Some((SHIFT, LETTERS[usize::from(ch as u8 - b'A')]));
    }
    if ch.is_ascii_digit() {
        return Some((NONE, DIGITS[usize::from(ch as u8 - b'0')]));
    }
    if let Some(i) = SHIFTED_DIGITS.find(ch) {
        return Some((SHIFT, DIGITS[i]));
    }
    let (shifted, key) = match ch {
        ' ' => (false, KeyCode::KEY_SPACE),
        '-' => (false, KeyCode::KEY_MINUS),
        '_' => (true, KeyCode::KEY_MINUS),
        '=' => (false, KeyCode::KEY_EQUAL),
        '+' => (true, KeyCode::KEY_EQUAL),
        '[' => (false, KeyCode::KEY_LEFTBRACE),
        '{' => (true, KeyCode::KEY_LEFTBRACE),
        ']' => (false, KeyCode::KEY_RIGHTBRACE),
        '}' => (true, KeyCode::KEY_RIGHTBRACE),
        '\\' => (false, KeyCode::KEY_BACKSLASH),
        '|' => (true, KeyCode::KEY_BACKSLASH),
        ';' => (false, KeyCode::KEY_SEMICOLON),
        ':' => (true, KeyCode::KEY_SEMICOLON),
        '\'' => (false, KeyCode::KEY_APOSTROPHE),
        '"' => (true, KeyCode::KEY_APOSTROPHE),
        '`' => (false, KeyCode::KEY_GRAVE),
        '~' => (true, KeyCode::KEY_GRAVE),
        ',' => (false, KeyCode::KEY_COMMA),
        '<' => (true, KeyCode::KEY_COMMA),
        '.' => (false, KeyCode::KEY_DOT),
        '>' => (true, KeyCode::KEY_DOT),
        '/' => (false, KeyCode::KEY_SLASH),
        '?' => (true, KeyCode::KEY_SLASH),
        _ => return None,
    };
    Some((if shifted { SHIFT } else { NONE }, key))
}

#[cfg(test)]
mod tests {
    use evdev::KeyCode;
    use xkbcommon::xkb;

    use super::{ALTGR, Layout, NONE, SHIFT, reverse_map};

    fn keymap(layout: &str, variant: &str) -> xkb::Keymap {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_names(
            &context,
            "evdev",
            "pc105",
            layout,
            variant,
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("system xkb data compiles the keymap")
    }

    #[test]
    fn us_qwerty_fallback_maps_characters_with_shift_state() {
        let us = Layout::UsQwerty;
        assert_eq!(us.stroke('a'), Some((NONE, KeyCode::KEY_A)));
        assert_eq!(us.stroke('Z'), Some((SHIFT, KeyCode::KEY_Z)));
        assert_eq!(us.stroke('!'), Some((SHIFT, KeyCode::KEY_1)));
        assert_eq!(us.stroke(')'), Some((SHIFT, KeyCode::KEY_0)));
        assert_eq!(us.stroke('?'), Some((SHIFT, KeyCode::KEY_SLASH)));
        assert_eq!(us.stroke('\n'), Some((NONE, KeyCode::KEY_ENTER)));
        assert_eq!(us.stroke('é'), None);
    }

    #[test]
    fn xkb_reverse_map_follows_the_layout() {
        let fr = Layout::Xkb(reverse_map(&keymap("fr", ""), 0));
        // AZERTY swaps A/Q, puts digits behind Shift, and € behind AltGr.
        assert_eq!(fr.stroke('a'), Some((NONE, KeyCode::KEY_Q)));
        assert_eq!(fr.stroke('1'), Some((SHIFT, KeyCode::KEY_1)));
        assert_eq!(fr.stroke('é'), Some((NONE, KeyCode::KEY_2)));
        assert_eq!(fr.stroke('€'), Some((ALTGR, KeyCode::KEY_E)));
    }

    #[test]
    fn xkb_reverse_map_selects_the_active_layout() {
        let map = reverse_map(&keymap("us,de", ","), 1);
        assert_eq!(map.get(&'z'), Some(&(NONE, KeyCode::KEY_Y)));
        assert_eq!(map.get(&'ß'), Some(&(NONE, KeyCode::KEY_MINUS)));
    }
}
