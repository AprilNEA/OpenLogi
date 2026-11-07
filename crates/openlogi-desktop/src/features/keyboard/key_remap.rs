//! Remapping a keyboard's keys on its own photo, per layer, for a keyboard
//! with HID++ `0x1b05` key customisation whose keys OpenLogi knows (the G515
//! LS TKL).
//!
//! Clicking a key picks it; the buttons under the photo choose what it sends
//! on the chosen layer: another key (picked by clicking it next), a media
//! key, G-Shift, or its own key back. Each change is persisted through
//! [`AppState::commit_key_remap`] and written to the keyboard by the agent.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gpui::{
    Context, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Render, Role,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, img,
    prelude::FluentBuilder as _, px,
};
use gpui_kit::component::{
    Disableable as _, Icon, Selectable as _, Sizable as _,
    button::{Button, ButtonGroup},
    h_flex, v_flex,
};
use openlogi_assets::KeyRegion;
use openlogi_core::config::{KeyRemap, KeyTarget, LightingKey, MediaKey};
use openlogi_core::device::DeviceKind;

use crate::state::{AppState, DeviceRecord, StateEvent};
use crate::ui::components::PanelCard;
use crate::ui::theme::{self, Palette, SelectableStyle as _, Typography as _};

/// The width the photo is drawn at.
const PHOTO_W: f32 = 800.;
/// The tint over a cap whose key sends something else on the shown layer.
const TINT_OPACITY: f32 = 0.35;

/// The keyboard's three layers, in the order the keyboard stores them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    Base,
    Fn,
    GShift,
}

impl Layer {
    const ALL: [Self; 3] = [Self::Base, Self::Fn, Self::GShift];

    fn label(self) -> String {
        match self {
            Self::Base => tr!("key_remap.layer_base"),
            Self::Fn => tr!("key_remap.layer_fn"),
            Self::GShift => tr!("key_remap.layer_g_shift"),
        }
        .to_string()
    }

    fn of(self, remap: &KeyRemap) -> &BTreeMap<LightingKey, KeyTarget> {
        match self {
            Self::Base => &remap.base,
            Self::Fn => &remap.fn_layer,
            Self::GShift => &remap.g_shift,
        }
    }

    fn of_mut(self, remap: &mut KeyRemap) -> &mut BTreeMap<LightingKey, KeyTarget> {
        match self {
            Self::Base => &mut remap.base,
            Self::Fn => &mut remap.fn_layer,
            Self::GShift => &mut remap.g_shift,
        }
    }
}

/// The keyboard photo and where each key sits on it.
struct RemapPhoto {
    image_path: PathBuf,
    /// The photo's size in the depot's pixels, which the key boxes use.
    origin: (f32, f32),
    /// Keys in paint order: outline boxes first, so a plain key over one
    /// takes the click.
    keys: Vec<(LightingKey, KeyRegion)>,
}

/// The photo for `record`, or `None` when it is not a keyboard or its depot
/// does not draw its keys.
fn remap_photo(record: &DeviceRecord) -> Option<RemapPhoto> {
    if record.kind != DeviceKind::Keyboard {
        return None;
    }
    let asset = record.asset.as_ref()?;
    let origin = asset.metadata.origin()?;
    let width = f32::from(u16::try_from(origin.width).ok().filter(|w| *w > 0)?);
    let height = f32::from(u16::try_from(origin.height).ok().filter(|h| *h > 0)?);
    let mut keys: Vec<_> = asset
        .metadata
        .key_regions()
        .into_iter()
        .filter_map(|region| Some((LightingKey::from_usage(region.usage)?, region)))
        .collect();
    if keys.is_empty() {
        return None;
    }
    keys.sort_by_key(|(_, region)| !region.outline);
    Some(RemapPhoto {
        image_path: asset.image_path.clone(),
        origin: (width, height),
        keys,
    })
}

/// A short label for what a key sends, drawn at the foot of its cap: the
/// key's name, or a symbol for a media key or G-Shift, so it fits the cap.
fn cap_label(target: KeyTarget) -> &'static str {
    match target {
        KeyTarget::Key(key) => key.name(),
        KeyTarget::Media(MediaKey::Mute) => "🔇",
        KeyTarget::Media(MediaKey::VolumeUp) => "🔊",
        KeyTarget::Media(MediaKey::VolumeDown) => "🔉",
        KeyTarget::Media(MediaKey::PlayPause) => "⏯",
        KeyTarget::Media(MediaKey::NextTrack) => "⏭",
        KeyTarget::Media(MediaKey::PreviousTrack) => "⏮",
        KeyTarget::GShift => "G⇧",
    }
}

fn media_label(media: MediaKey) -> String {
    match media {
        MediaKey::Mute => tr!("key_remap.mute"),
        MediaKey::VolumeUp => tr!("key_remap.volume_up"),
        MediaKey::VolumeDown => tr!("key_remap.volume_down"),
        MediaKey::PlayPause => tr!("key_remap.play_pause"),
        MediaKey::NextTrack => tr!("key_remap.next_track"),
        MediaKey::PreviousTrack => tr!("key_remap.previous_track"),
    }
    .to_string()
}

pub struct KeyRemapPanel {
    layer: Layer,
    /// The key picked on the photo, whose target the buttons set.
    selected: Option<LightingKey>,
    /// Whether the next click on the photo picks the selected key's target.
    picking: bool,
    /// The device the picks were made on, so a device switch starts clean.
    device: Option<String>,
    _state_obs: Subscription,
}

impl KeyRemapPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs = AppState::repaint_on(cx, |event| {
            matches!(event, StateEvent::DeviceConfigChanged(_))
        });
        Self {
            layer: Layer::Base,
            selected: None,
            picking: false,
            device: None,
            _state_obs: state_obs,
        }
    }

    /// A click on `key` in the photo: its target while picking one, else
    /// the key to remap (or none, when it was already picked).
    fn click(&mut self, key: LightingKey, cx: &mut Context<Self>) {
        if let (true, Some(source)) = (self.picking, self.selected) {
            self.picking = false;
            self.set(source, Some(KeyTarget::Key(key)), cx);
        } else {
            self.picking = false;
            self.selected = (self.selected != Some(key)).then_some(key);
        }
    }

    /// Make `source` send `target` on the shown layer, or its own key with
    /// `None`.
    fn set(&self, source: LightingKey, target: Option<KeyTarget>, cx: &mut Context<Self>) {
        let layer = self.layer;
        AppState::apply(cx, |state| {
            let mut remap = state.current_key_remap();
            let keys = layer.of_mut(&mut remap);
            match target {
                // A key sent as itself is no remap at all.
                Some(KeyTarget::Key(key)) if key == source => {
                    keys.remove(&source);
                }
                Some(target) => {
                    keys.insert(source, target);
                }
                None => {
                    keys.remove(&source);
                }
            }
            state.commit_key_remap(remap)
        });
    }
}

impl Render for KeyRemapPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let state = AppState::try_read(cx);
        let record = state.and_then(AppState::current_record);
        let device = record.map(|record| record.config_key.clone());
        if self.device != device {
            self.device = device;
            self.selected = None;
            self.picking = false;
        }
        let photo = state
            .filter(|state| state.current_key_remap_supported())
            .and(record)
            .and_then(remap_photo);
        let Some(photo) = photo else {
            return div().into_any_element();
        };
        let remap = state.map(AppState::current_key_remap).unwrap_or_default();
        let layer = self.layer.of(&remap).clone();

        let tabs = ButtonGroup::new("key-remap-layers")
            .outline()
            .children(Layer::ALL.map(|layer| {
                let count = layer.of(&remap).len();
                let label = if count == 0 {
                    layer.label()
                } else {
                    format!("{} ({count})", layer.label())
                };
                let selected = self.layer == layer;
                Button::new(("key-remap-layer", layer as usize))
                    .small()
                    .label(label)
                    .selected(selected)
                    .selected_segment(selected, cx)
                    .on_click(cx.listener(move |panel, _event, _window, cx| {
                        panel.layer = layer;
                        panel.picking = false;
                        cx.notify();
                    }))
            }));

        let hint = match (self.selected, self.picking) {
            (Some(key), true) => tr!("key_remap.pick_hint", key => key.name()).to_string(),
            (Some(key), false) => tr!("key_remap.sends", key => key.name()).to_string(),
            (None, _) => tr!("key_remap.hint").to_string(),
        };

        let content = v_flex()
            .gap_3()
            .child(tabs)
            .child(div().text_caption().text_color(pal.text_muted).child(hint))
            .child(photo_view(&photo, &layer, self.selected, pal, cx))
            .children(
                self.selected
                    .map(|key| target_buttons(key, self.layer, &layer, self.picking, cx)),
            )
            .child(
                h_flex().child(
                    Button::new("key-remap-reset-all")
                        .small()
                        .label(tr!("key_remap.reset_all"))
                        .disabled(remap.is_empty())
                        .on_click(cx.listener(|panel, _event, _window, cx| {
                            panel.selected = None;
                            panel.picking = false;
                            AppState::apply(cx, |state| {
                                state.commit_key_remap(KeyRemap::default())
                            });
                        })),
                ),
            );

        PanelCard::new(
            tr!("key_remap.title"),
            Icon::empty().path("action-icons/keyboard.svg"),
            content,
        )
        .into_any_element()
    }
}

/// The photo with a click target over each key: an accent border on the
/// picked key, and a tint and label on keys that send something else.
fn photo_view(
    photo: &RemapPhoto,
    layer: &BTreeMap<LightingKey, KeyTarget>,
    selected: Option<LightingKey>,
    pal: Palette,
    cx: &mut Context<KeyRemapPanel>,
) -> impl IntoElement {
    let scale = PHOTO_W / photo.origin.0;
    let height = photo.origin.1 * scale;
    let targets: Vec<_> = photo
        .keys
        .iter()
        .map(|&(key, region)| {
            let rect = region.rect;
            let is_selected = selected == Some(key);
            let target = layer.get(&key).copied();
            div()
                .id(("key-remap", usize::from(key.usage())))
                .debug_selector(move || format!("key-remap-{}", key.name()))
                .role(Role::Button)
                .aria_label(key.name())
                .absolute()
                .left(px(rect.x * scale))
                .top(px(rect.y * scale))
                .w(px(rect.width * scale))
                .h(px(rect.height * scale))
                .rounded(px(4.))
                .cursor_pointer()
                .overflow_hidden()
                .flex()
                .flex_col()
                .justify_end()
                .border_2()
                .border_color(if is_selected {
                    theme::accent()
                } else {
                    gpui::transparent_black()
                })
                .when_some(target, |cap, target| {
                    cap.bg(accent_tint()).child(
                        div()
                            .w_full()
                            .px(px(1.))
                            .bg(pal.panel)
                            .text_size(px(9.))
                            .line_height(px(11.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(pal.text_primary)
                            .text_center()
                            .truncate()
                            .child(cap_label(target)),
                    )
                })
                .when(!is_selected, |cap| {
                    cap.hover(|style| style.border_color(theme::accent_tint_hover()))
                })
                .on_click(cx.listener(move |panel, _event, _window, cx| {
                    panel.click(key, cx);
                    cx.notify();
                }))
        })
        .collect();

    div()
        .relative()
        .flex_none()
        .w(px(PHOTO_W))
        .h(px(height))
        .child(
            img(photo.image_path.clone())
                .absolute()
                .top_0()
                .left_0()
                .w(px(PHOTO_W))
                .h(px(height)),
        )
        .children(targets)
}

fn accent_tint() -> Hsla {
    let mut tint = theme::accent();
    tint.a = TINT_OPACITY;
    tint
}

/// One button that makes `key` send `target` (its own key with `None`).
fn target_button(
    id: impl Into<gpui::ElementId>,
    label: String,
    selected: bool,
    key: LightingKey,
    target: Option<KeyTarget>,
    cx: &mut Context<KeyRemapPanel>,
) -> Button {
    Button::new(id)
        .small()
        .label(label)
        .selected(selected)
        .selected_segment(selected, cx)
        .on_click(cx.listener(move |panel, _event, _window, cx| {
            panel.picking = false;
            panel.set(key, target, cx);
            cx.notify();
        }))
}

/// What the picked key can send on the shown layer.
fn target_buttons(
    key: LightingKey,
    layer: Layer,
    keys: &BTreeMap<LightingKey, KeyTarget>,
    picking: bool,
    cx: &mut Context<KeyRemapPanel>,
) -> impl IntoElement {
    let current = keys.get(&key).copied();
    let original = target_button(
        "key-remap-original",
        tr!("key_remap.original").to_string(),
        current.is_none(),
        key,
        None,
        cx,
    );
    let pick = Button::new("key-remap-pick")
        .small()
        .label(match current {
            Some(KeyTarget::Key(target)) => {
                tr!("key_remap.key_target", key => target.name()).to_string()
            }
            _ => tr!("key_remap.pick_key").to_string(),
        })
        .selected(picking || matches!(current, Some(KeyTarget::Key(_))))
        .selected_segment(picking || matches!(current, Some(KeyTarget::Key(_))), cx)
        .on_click(cx.listener(|panel, _event, _window, cx| {
            panel.picking = !panel.picking;
            cx.notify();
        }));
    let mut media = Vec::new();
    for item in MediaKey::all() {
        let target = KeyTarget::Media(item);
        media.push(target_button(
            ("key-remap-media", item as usize),
            media_label(item),
            current == Some(target),
            key,
            Some(target),
            cx,
        ));
    }
    // G-Shift is a modifier: only a key on the base layer can be it.
    let g_shift = (layer == Layer::Base).then(|| {
        target_button(
            "key-remap-g-shift",
            tr!("key_remap.g_shift").to_string(),
            current == Some(KeyTarget::GShift),
            key,
            Some(KeyTarget::GShift),
            cx,
        )
    });
    h_flex()
        .gap_2()
        .flex_wrap()
        .child(original)
        .child(pick)
        .children(media)
        .children(g_shift)
}
