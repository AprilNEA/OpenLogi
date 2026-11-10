//! The keyboard key remapper view — the Keys tab body.
//!
//! A two-pane inspector model (the "pro-tool" layout): the keyboard photo,
//! with an icon callout over each key, beside a fixed inspector column. With
//! no key selected the column lists every key by its full name with what it
//! does now — the callouts have one key pitch of room, the list has the
//! column's. Clicking a key **selects** it (no popover) and the column fades
//! in its config panel in place, so the board never moves under the pointer.
//! Only one key is selected at a time.
//!
//! Which keys appear is the asset's call (see [`key_points::key_surface`]): a
//! depot with control markers shows the keyboard's own HID++ controls on the
//! photo, each bound per device through [`AppState::commit_binding`] and
//! diverted by the agent while bound; a keypad whose depot marks its keys,
//! or whose render is the stock one OpenLogi measured, has its keys drawn on
//! the device render itself, the way Options+ shows it; any other keypad
//! shows its own keycaps drawn from the device model beside the render, bound
//! the same per-device way; any other markerless device shows the OS-hook F-row,
//! bound globally through [`AppState::commit_keyboard_binding`]. The panel
//! lists the same action catalog the mouse picker uses, plus a Power User
//! section.

#![expect(
    clippy::needless_pass_by_value,
    reason = "GPUI builders take owned Copy palette values"
)]

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, FontWeight, Hsla,
    InteractiveElement, IntoElement, ParentElement, PathBuilder, Refineable as _, Render,
    RenderOnce, Role, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled,
    Subscription, TextStyle, Window, canvas, div, hsla, linear_color_stop, linear_gradient, point,
    prelude::FluentBuilder as _, px, rems, rgb, svg,
};
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    Selectable as _, h_flex, input::InputState, scroll::ScrollableElement as _, v_flex,
};
use openlogi_core::binding::{
    Action, ActionRingIcon, ButtonId, GKeyProfile, KeypadPage, WorkflowStep,
};
use openlogi_core::config::GamingKeyMode;

use super::editors::{
    PowerUserKind, text_editor_placeholder, text_editor_seed, workflow_editor_seed,
};
use crate::app::{glow_canvas, keyboard_glow};
use crate::features::binding_editor::{
    PickFn, action_icon_path, action_rows, compact_panel, divider, editor_scroll_list,
    editor_section,
};
use crate::features::mouse::geometry::asset_dimensions_for_png;
use crate::features::profiles::profile_name;
use crate::services::assets::{GlowGeometry, ResolvedAsset};
use crate::state::{AppState, DeviceKey, DeviceRecord, GamingKeys, StateEvent, StateEvents};
use crate::ui::action::localized_action_label;
use crate::ui::components::{MenuRow, control_button};
use crate::ui::theme::{
    self, ACCENT_BLUE, Palette, PressableStyle as _, SCREEN_PAD, Typography as _,
};
use gpui::{Animation, AnimationExt, ease_out_quint, img};
use openlogi_core::hid::KeypadDescriptor;

mod callout_layout;
mod gaming;
mod key_points;
mod keypad;

use callout_layout::{CalloutLayout, CardRect, callout_layout, side_bands_width};
use gaming::gaming_bar;
pub(crate) use key_points::KeyTarget;
#[cfg(test)]
use key_points::{EVEN_SPACING_END, EVEN_SPACING_START, key_x_fractions};
use key_points::{KeySlotLayout, KeySurface, StoredAs, key_surface, keypad_canvas, shows_g_keys};
use keypad::{KeypadPane, KeypadPhotoPane, LastPage, keypad_page_bar, keypad_photo_render_size};

/// Narrowest width of the inspector column beside the device: the key
/// overview, or the selected key's config panel. The column grows past it
/// to keep every key's name and binding on one line (see
/// [`inspector_width`]).
const PANEL_W: f32 = 320.;
/// Widest the inspector column grows; past this a long name wraps instead.
/// The board gives up what the column takes past [`PANEL_W`].
const PANEL_MAX_W: f32 = 440.;
/// Space between the device pane and the inspector column.
const INSPECTOR_GAP: f32 = 20.;
/// What the narrowest inspector column takes from the row.
const INSPECTOR_W: f32 = PANEL_W + INSPECTOR_GAP;
/// The Keys tab's content width: the widest board beside the narrowest
/// inspector column.
const KEYS_CONTENT_W: f32 = KEYBOARD_W + INSPECTOR_W;
/// The inspector column's height while the key overview needs no more: room
/// for the key panel's action list. The column keeps one height whatever it
/// shows, so swapping the overview for a panel never resizes the row; a
/// longer overview grows it up to the height the Keys tab has (see
/// [`inspector_height`]).
const INSPECTOR_H: f32 = 540.;
/// The least the column shrinks to in a short window, which then scrolls the
/// tab instead.
const INSPECTOR_MIN_H: f32 = 240.;
/// Height of the fade over the overview's cut-off bottom edge, the Actions
/// Ring library's treatment. The list then ends with as much padding, so its
/// last row can scroll clear of the fade.
const OVERVIEW_FADE_H: f32 = 24.;
/// An overview row's binding icon, and the gap after it, in rems.
const OVERVIEW_ICON: f32 = 0.875;
const OVERVIEW_ICON_GAP: f32 = 0.375;
/// What an overview row adds around its key name and binding, in rems: the
/// card's padding and the row's either side (`compact_panel`'s `p_1p5`,
/// `MenuRow`'s `px_2`), the row's `gap_2` between the two, and the binding's
/// icon and gap.
const OVERVIEW_ROW_CHROME: f32 = 2. * 0.375 + 2. * 0.5 + 0.5 + OVERVIEW_ICON + OVERVIEW_ICON_GAP;
/// What it adds in pixels: the card's border either side, and the pixel each
/// of the two texts can lose when layout snaps it to whole pixels.
const OVERVIEW_ROW_PX: f32 = 2. + 2.;
/// How long the config panel takes to fade in when a key is selected.
const PANEL_ENTER: std::time::Duration = std::time::Duration::from_millis(160);
/// How far the config panel travels while it fades in: out of the gap beside
/// the board, never over the board itself.
const PANEL_ENTER_SHIFT: f32 = 12.;
/// Maximum keyboard render width in the Keys inspector — with the inspector
/// column, the Keys tab's full content width.
const KEYBOARD_W: f32 = 700.;
/// Narrowest the board pane gets to keep the inspector beside it; a narrower
/// window wraps the inspector under the board instead.
const KEYBOARD_MIN_BESIDE_W: f32 = 520.;
/// Render size when no asset resolved: the placeholder box.
const FALLBACK_KEYBOARD_SIZE: (f32, f32) = (KEYBOARD_W, 220.);
/// Space above the keyboard reserved for function-key callouts.
const CALLOUT_BAND_H: f32 = 84.;
/// Vertical chrome around the keyboard pane (header, tab strip, screen
/// padding, footer) — the viewport height minus this and the callout band is
/// what the render may occupy before it scales down to fit.
const KEYS_VERTICAL_RESERVE: f32 = 224.;
/// Horizontal chrome around the Keys tab body: the device rail and the tab's
/// padding.
const KEYS_HORIZONTAL_RESERVE: f32 = crate::ui::theme::DETAIL_RAIL_W + 48.;
/// Floor on the render height so a tiny window still shows a usable model.
const KEYBOARD_MIN_IMG_H: f32 = 160.;
/// A callout carries one icon: a key pitch holds no readable name.
const KEY_CALLOUT_H: f32 = 28.;
const KEY_CALLOUT_ICON: f32 = 14.;
/// The quiet mark a callout carries while its key keeps its own function —
/// the glyph printed on the key already says what that is.
const KEY_CALLOUT_DOT: f32 = 5.;
const KEY_CALLOUT_TOP_UPPER: f32 = 2.;
const KEY_CALLOUT_TOP_LOWER: f32 = KEY_CALLOUT_TOP_UPPER + KEY_CALLOUT_H + 8.;
const KEY_TARGET_W: f32 = 30.;
const KEY_TARGET_H: f32 = 30.;
const KEY_HOTSPOT_DOT: f32 = 12.;
/// The glyph for a key on its own function, as the panel's Native function
/// row shows it.
const NATIVE_FUNCTION_ICON: &str = "action-icons/keyboard.svg";

/// The function-row remapper view.
pub struct FunctionRowView {
    /// The single selected key index (0 = Esc), or `None` when nothing is
    /// selected (no panel shown).
    selected_key: Option<usize>,
    /// The hovered function-row key index, shared by callout bubbles, key hit
    /// zones, and leader lines.
    hovered_key: Option<usize>,
    /// Which power-user editor is showing in the panel, if any.
    active_editor: Option<PowerUserKind>,
    /// Lazily-created [`InputState`] for the text editors.
    text_state: Option<Entity<InputState>>,
    /// Draft copy of the Workflow steps under edit.
    workflow_draft: Vec<WorkflowStep>,
    /// Whether the open single-field editor rejected its input: a blank Open
    /// Application path, or a Custom Shortcut that does not parse.
    editor_invalid: bool,
    /// The keypad binding page the grid keys are being edited on — this
    /// editor's own choice, independent of the page the keypad is showing.
    keypad_page: KeypadPage,
    /// The M profile whose G-key bindings are being edited — this editor's
    /// own choice, independent of the profile the keyboard is on.
    g_profile: GKeyProfile,
    /// Whether the selected key is an M1–M3 card in profile mode, as of the
    /// last render: such a card makes its profile the edited one.
    selected_profile_card: bool,
    /// The gaming-key mode of the last render. A mode change reorders the
    /// G-series keys (nine-button mode adds MR before G1–G5), so the selected
    /// index and any open draft would land on another key.
    gaming_mode: GamingKeyMode,
    /// The device and app profile being edited: another device starts back
    /// on page 1 and profile M1, and either change drops the open editor.
    edit_scope: EditScope,
    /// The height the Keys tab leaves the inspector column, and the height
    /// the key overview took in it, as the last frame laid them out (see
    /// [`inspector_height`]).
    inspector_room: Option<f32>,
    overview_h: Option<f32>,
    overview_scroll: ScrollHandle,
    _state_obs: Subscription,
}

/// Which device's bindings, in which app profile, the view edits — beside
/// the key and the page, what a draft was started for.
#[derive(Debug, Default, PartialEq, Eq)]
struct EditScope {
    device: Option<DeviceKey>,
    /// The open app profile, `None` for the device's global one.
    profile: Option<String>,
}

impl FunctionRowView {
    /// Create the view.
    pub fn new(cx: &mut Context<Self>) -> Self {
        let state_obs =
            AppState::repaint_on(cx, |event| matches!(event, StateEvent::BindingsChanged(_)));
        Self {
            selected_key: None,
            hovered_key: None,
            active_editor: None,
            text_state: None,
            workflow_draft: Vec::new(),
            editor_invalid: false,
            keypad_page: KeypadPage::FIRST,
            g_profile: GKeyProfile::M1,
            selected_profile_card: false,
            gaming_mode: GamingKeyMode::default(),
            edit_scope: EditScope::default(),
            inspector_room: None,
            overview_h: None,
            overview_scroll: ScrollHandle::new(),
            _state_obs: state_obs,
        }
    }

    /// Select a key (or deselect with `None`), opening/closing the panel.
    pub(crate) fn select_key(&mut self, idx: Option<usize>, cx: &mut Context<Self>) {
        // Changing selection also drops any open editor + its drafts.
        if self.selected_key != idx {
            self.drop_editor();
        }
        self.selected_key = idx;
        cx.notify();
    }

    /// Close the open editor and discard its drafts. Every change of the key
    /// being edited — another key, page, device, or app profile — calls
    /// this, or a draft started for one would save onto the next.
    fn drop_editor(&mut self) {
        self.active_editor = None;
        self.text_state = None;
        self.workflow_draft.clear();
        self.editor_invalid = false;
    }

    /// Edit the grid keys on `page`, dropping any open editor when it moves.
    fn move_keypad_page(&mut self, page: KeypadPage) {
        if self.keypad_page != page {
            self.keypad_page = page;
            self.drop_editor();
        }
    }

    /// Toggle a key selection from a click on either its callout or key hit
    /// target.
    pub(crate) fn click_key(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.select_key(next_selection_after_click(self.selected_key, idx), cx);
    }

    pub(crate) fn set_hovered_key(&mut self, idx: Option<usize>, cx: &mut Context<Self>) {
        if self.hovered_key != idx {
            self.hovered_key = idx;
            cx.notify();
        }
    }

    /// Keep `height`, which the last frame measured into `slot`, and lay the
    /// row out again when it moved.
    fn remeasure(
        &mut self,
        slot: fn(&mut Self) -> &mut Option<f32>,
        height: f32,
        cx: &mut Context<Self>,
    ) {
        let slot = slot(self);
        if slot.is_none_or(|old| (old - height).abs() > 0.5) {
            *slot = Some(height);
            cx.notify();
        }
    }

    /// The inspector column's content and height. The column keeps its place
    /// and size either way, so selecting a key never moves the board: it
    /// lists every key until one is selected, then holds that key's panel.
    fn inspector(
        &self,
        selected: Option<usize>,
        slots: &[KeySlot],
        view: &Entity<Self>,
        pal: &Palette,
        cx: &mut Context<Self>,
    ) -> (Inspector, f32) {
        let room = self
            .inspector_room
            .map_or(INSPECTOR_H, |room| room.max(INSPECTOR_MIN_H));
        let inspector = match selected {
            Some(selected) => Inspector::Panel(self.config_panel(selected, slots, view, cx)),
            None => Inspector::Overview(key_overview(
                slots,
                self.hovered_key,
                &self.overview_scroll,
                room,
                view,
                pal,
            )),
        };
        (inspector, inspector_height(self.overview_h, room))
    }

    /// Follow the selected device and the open app profile — dropping a
    /// draft started for another, and starting a new device on page 1 — keep
    /// the edited page on the device's pages (clamped when pages were
    /// removed), and answer the device's last page and whether removing it
    /// would drop bindings (so removal asks).
    fn sync_edit_scope(&mut self, state: Option<&AppState>) -> LastPage {
        let scope = EditScope {
            device: state
                .and_then(AppState::current_record)
                .map(DeviceRecord::device_key),
            profile: state.and_then(AppState::editing_app).map(str::to_owned),
        };
        if self.edit_scope != scope {
            if self.edit_scope.device != scope.device {
                self.move_keypad_page(KeypadPage::FIRST);
                self.g_profile = GKeyProfile::M1;
            }
            self.drop_editor();
            self.edit_scope = scope;
        }
        let last = state.map_or(KeypadPage::FIRST, AppState::current_keypad_pages);
        self.move_keypad_page(self.keypad_page.min(last));
        // Every profile's keys, not just the open one's: removal drops them all.
        let binds_keys = state.is_some_and(|state| state.current_keypad_page_binds(last));
        LastPage {
            page: last,
            binds_keys,
        }
    }

    /// Edit the grid keys on `page`.
    pub(crate) fn set_keypad_page(&mut self, page: KeypadPage, cx: &mut Context<Self>) {
        if self.keypad_page != page {
            self.move_keypad_page(page);
            cx.notify();
        }
    }

    /// The keys `layouts` places, as this view edits them: on its keypad page
    /// and G-key profile. A selected profile key in profile mode makes its
    /// profile the edited one first.
    fn edited_slots(
        &mut self,
        layouts: Vec<KeySlotLayout>,
        state: Option<&AppState>,
        gaming: Option<GamingKeys>,
    ) -> Vec<KeySlot> {
        let gaming_mode = gaming.map_or_else(GamingKeyMode::default, |gaming| gaming.mode);
        if gaming_mode != self.gaming_mode {
            self.gaming_mode = gaming_mode;
            self.selected_key = None;
            self.drop_editor();
        }
        let card = self
            .selected_key
            .and_then(|idx| layouts.get(idx))
            .and_then(|layout| profile_key(&layout.target, gaming_mode));
        self.selected_profile_card = card.is_some();
        if let Some(profile) = card
            && profile != self.g_profile
        {
            self.g_profile = profile;
            self.drop_editor();
        }
        key_slots_from_layouts(
            layouts,
            state,
            KeyEditScope {
                page: self.keypad_page,
                g_profile: self.g_profile,
                gaming_mode,
            },
        )
    }

    /// The bar over the device: a keypad's binding pages, or a G-series
    /// keyboard's gaming cluster.
    fn device_bar(
        &self,
        keypad: bool,
        last_page: LastPage,
        gaming: Option<GamingKeys>,
        view: &Entity<Self>,
        pal: &Palette,
        cx: &App,
    ) -> Option<AnyElement> {
        if keypad {
            return Some(
                keypad_page_bar(self.keypad_page, last_page, view, pal, cx).into_any_element(),
            );
        }
        gaming.map(|gaming| gaming_bar(gaming, self.g_profile, view, pal, cx).into_any_element())
    }

    /// Edit the G-keys of `profile`, dropping any open editor when it moves.
    /// A selected M1–M3 card is deselected: it names the profile it selects,
    /// and would switch the editor straight back on the next render.
    pub(crate) fn set_g_profile(&mut self, profile: GKeyProfile, cx: &mut Context<Self>) {
        if self.g_profile != profile {
            if self.selected_profile_card {
                self.selected_key = None;
                self.selected_profile_card = false;
            }
            self.g_profile = profile;
            self.drop_editor();
            cx.notify();
        }
    }

    pub(crate) fn open_editor(&mut self, kind: PowerUserKind, cx: &mut Context<Self>) {
        self.active_editor = Some(kind);
        self.text_state = None;
        self.workflow_draft.clear();
        self.editor_invalid = false;
        cx.notify();
    }

    pub(crate) fn close_editor(&mut self, cx: &mut Context<Self>) {
        self.drop_editor();
        cx.notify();
    }

    pub(crate) fn mark_editor_invalid(&mut self, cx: &mut Context<Self>) {
        self.editor_invalid = true;
        cx.notify();
    }

    pub(crate) fn text_state(&self) -> Option<Entity<InputState>> {
        self.text_state.clone()
    }

    pub(crate) fn new_text_state(
        &mut self,
        seed: String,
        placeholder: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let state = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(placeholder);
            if !seed.is_empty() {
                s.set_value(seed, window, cx);
            }
            s
        });
        self.text_state = Some(state.clone());
        state
    }

    pub(crate) fn workflow_draft(&self) -> &[WorkflowStep] {
        &self.workflow_draft
    }

    pub(crate) fn push_workflow_step(&mut self, step: WorkflowStep, cx: &mut Context<Self>) {
        self.workflow_draft.push(step);
        cx.notify();
    }

    pub(crate) fn remove_workflow_step(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx < self.workflow_draft.len() {
            self.workflow_draft.remove(idx);
            cx.notify();
        }
    }
}

impl Render for FunctionRowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = AppState::try_read(cx);
        let asset = state.and_then(|state| state.current_record()?.asset.as_ref());
        // Without a record there is no device surface to trust, and the F-row
        // fallback is as honest as anything that shows.
        let known_keypad = state.and_then(AppState::current_keypad);
        let glow = state.and_then(|state| {
            state
                .current_record()
                .and_then(|record| keyboard_glow(state, record))
        });

        let viewport = window.viewport_size();
        let viewport_h = f32::from(viewport.height);
        let image_path = asset.map(|asset| asset.image_path.clone());
        let (surface, gaming) = device_keys(state, asset, known_keypad);
        let keypad = matches!(surface, KeySurface::Keypad(_));
        // A keypad on its photo draws its keys on the render itself: the
        // keyboard's callout band cannot lay eleven bubbles across a
        // portrait render, and the keys sit under the user's eyes already.
        let keypad_photo = matches!(surface, KeySurface::Photo(_)) && known_keypad.is_some();
        // The schematic keeps photo and keycaps on one alignment line.
        let photo = asset
            .filter(|asset| asset.png_height > 0)
            .map_or((0., 0.), |asset| {
                asset_dimensions_for_png(asset, keypad_canvas().1, 360.)
            });
        let last_page = self.sync_edit_scope(state);
        let pal = theme::palette(cx);
        let slots = self.edited_slots(surface.slots(), state, gaming);
        let panel_w = inspector_width(&slots, window);
        let render_size = if keypad_photo {
            keypad_photo_render_size(asset, viewport_h)
        } else {
            let markers: Vec<(f32, f32)> = slots
                .iter()
                .map(|slot| (slot.x_frac, slot.y_frac))
                .collect();
            keyboard_render_size(
                asset,
                viewport_h,
                keyboard_max_width(
                    f32::from(viewport.width),
                    side_bands_width(&markers),
                    panel_w,
                ),
            )
        };

        // A stale selection can outlive a device switch to a shorter F-row;
        // drop it instead of indexing past the new slot list.
        if self.selected_key.is_some_and(|idx| idx >= slots.len()) {
            self.selected_key = None;
            self.drop_editor();
        }
        let selected = self.selected_key;
        let hovered = self.hovered_key;
        let active_editor = self.active_editor;
        if let (Some(selected_idx), Some(kind)) = (selected, active_editor)
            && let Some(slot) = slots.get(selected_idx)
        {
            let current_action = state.and_then(|state| bound_action(state, &slot.target));
            match kind {
                PowerUserKind::Workflow => {
                    if self.workflow_draft.is_empty() {
                        self.workflow_draft = workflow_editor_seed(current_action.as_ref());
                    }
                }
                _ => {
                    if let Some(state) = self.text_state.clone() {
                        crate::ui::components::localize_placeholder(
                            &state,
                            text_editor_placeholder(kind),
                            window,
                            cx,
                        );
                    } else {
                        self.new_text_state(
                            text_editor_seed(current_action.as_ref(), kind),
                            text_editor_placeholder(kind),
                            window,
                            cx,
                        );
                    }
                }
            }
        }
        let view = cx.entity();
        // The same selection, panel, and editor machinery on either surface:
        // the keyboard's photo with callouts, or the keypad's photo beside
        // drawn keycaps.
        let device = if keypad {
            KeypadPane::new(slots.clone(), image_path, photo, view.clone())
                .selected(selected)
                .hovered(hovered)
                .into_any_element()
        } else if keypad_photo {
            KeypadPhotoPane::new(slots.clone(), image_path, render_size, view.clone())
                .selected(selected)
                .hovered(hovered)
                .into_any_element()
        } else {
            KeyboardPane::new(slots.clone(), image_path, glow, render_size, view.clone())
                .selected(selected)
                .hovered(hovered)
                .into_any_element()
        };
        let (inspector, column_h) = self.inspector(selected, &slots, &view, &pal, cx);
        let page_bar = self.device_bar(keypad || keypad_photo, last_page, gaming, &view, &pal, cx);

        let row = InspectorRow::new(device, inspector, (panel_w, column_h));
        keys_tab_content(page_bar, row, &view)
    }
}

/// The Keys tab's content — a keypad's page bar over the device and its
/// inspector column — reporting to `view` the room the tab's visible height
/// leaves the column once this frame laid it out: the tab body clips to that
/// height, inset by its padding. The next frame fits the column to it.
fn keys_tab_content(
    page_bar: Option<impl IntoElement>,
    row: InspectorRow,
    view: &Entity<FunctionRowView>,
) -> gpui::Div {
    let column_h = row.column.1;
    let view = view.clone();
    div().w_full().child(
        v_flex()
            .w_full()
            .items_center()
            .gap_3()
            .children(page_bar)
            .child(row)
            .on_prepaint(move |content, window, cx| {
                let visible = window.content_mask().bounds.size.height;
                let pad = SCREEN_PAD.rems().to_pixels(window.rem_size());
                let free = f32::from(visible - pad * 2. - content.size.height);
                view.update(cx, |view, cx| {
                    view.remeasure(|view| &mut view.inspector_room, column_h + free, cx);
                });
            }),
    )
}

/// The keys the selected device shows, and its gaming cluster when the
/// render places its G-keys.
fn device_keys(
    state: Option<&AppState>,
    asset: Option<&ResolvedAsset>,
    keypad: Option<&KeypadDescriptor>,
) -> (KeySurface, Option<GamingKeys>) {
    let gaming = state
        .and_then(AppState::current_gaming_keys)
        .filter(|_| asset.is_some_and(shows_g_keys));
    (key_surface(asset, keypad, gaming), gaming)
}

fn binding_line(
    binding: SharedString,
    icon: Option<&'static str>,
    max_w: f32,
    highlighted: bool,
    pal: &Palette,
) -> gpui::Div {
    h_flex()
        .items_center()
        .justify_center()
        .gap(px(2.))
        .max_w(px(max_w))
        .when_some(icon, |row, icon| {
            row.child(
                svg()
                    .path(icon)
                    .size(px(9.))
                    .flex_none()
                    .text_color(if highlighted {
                        rgb(ACCENT_BLUE).into()
                    } else {
                        pal.text_muted
                    }),
            )
        })
        .child(
            div()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_caption()
                .text_color(if highlighted {
                    rgb(ACCENT_BLUE).into()
                } else {
                    pal.text_muted
                })
                .child(binding),
        )
}

/// The widest the render may get. The whole pane — the render and any band
/// of cards beside the board — keeps to what [`KEYS_CONTENT_W`] leaves beside
/// a `column_w` inspector column, so the column always fits beside it in the
/// Keys tab, and shrinks further to keep the inspector beside it in a
/// narrower window. Below [`KEYBOARD_MIN_BESIDE_W`] the inspector wraps
/// under the board instead, which then keeps [`KEYBOARD_W`].
fn keyboard_max_width(viewport_w: f32, side_bands_w: f32, column_w: f32) -> f32 {
    let inspector_w = column_w + INSPECTOR_GAP;
    let beside =
        (viewport_w - KEYS_HORIZONTAL_RESERVE - inspector_w).min(KEYS_CONTENT_W - inspector_w);
    let pane_w = if beside >= KEYBOARD_MIN_BESIDE_W {
        beside
    } else {
        KEYBOARD_W
    };
    pane_w - side_bands_w
}

/// The inspector column's width: what the widest overview row — key name,
/// binding icon and binding — takes on one line, from [`PANEL_W`] up to
/// [`PANEL_MAX_W`]. Measured in the rows' own text style, so a language's
/// long key names widen the column instead of being cut off.
fn inspector_width(slots: &[KeySlot], window: &Window) -> f32 {
    let mut style = window.text_style();
    style.refine(div().text_caption().text_style());
    let rem = window.rem_size();
    let font_size = style.font_size.to_pixels(rem);
    let text_w = |text: &SharedString, font_weight: FontWeight| {
        let run = TextStyle {
            font_weight,
            ..style.clone()
        }
        .to_run(text.len());
        let line = window
            .text_system()
            .shape_line(text.clone(), font_size, &[run], None);
        f32::from(line.width)
    };
    let widest = slots
        .iter()
        .map(|slot| {
            let binding_weight = if slot.bound {
                FontWeight::MEDIUM
            } else {
                style.font_weight
            };
            text_w(&slot.label, style.font_weight) + text_w(&slot.binding, binding_weight)
        })
        .fold(0., f32::max);
    let chrome = f32::from(rems(OVERVIEW_ROW_CHROME).to_pixels(rem)) + OVERVIEW_ROW_PX;
    (widest + chrome).clamp(PANEL_W, PANEL_MAX_W)
}

/// The inspector column's height: what the key overview took when it last
/// showed, no less than [`INSPECTOR_H`] for the key panel, and no more than
/// the `room` the Keys tab leaves it.
fn inspector_height(overview_h: Option<f32>, room: f32) -> f32 {
    overview_h.unwrap_or(0.).max(INSPECTOR_H).min(room)
}

/// The keyboard render size: the actual PNG aspect at up to `max_w` wide,
/// shrunk to fit the viewport height. Sizing off the real aspect keeps
/// `ObjectFit::Contain` from letterboxing and keeps the marker overlays
/// registered with the rendered keys — the G513 render (with wrist rest) is
/// nearly twice as tall as an MX Keys render at the same width.
fn keyboard_render_size(asset: Option<&ResolvedAsset>, viewport_h: f32, max_w: f32) -> (f32, f32) {
    let Some(asset) = asset.filter(|a| a.png_height > 0) else {
        return FALLBACK_KEYBOARD_SIZE;
    };
    let target_h = (viewport_h - KEYS_VERTICAL_RESERVE - CALLOUT_BAND_H).max(KEYBOARD_MIN_IMG_H);
    asset_dimensions_for_png(asset, target_h, max_w)
}

/// One key with its resolved layout + binding.
#[derive(Clone)]
struct KeySlot {
    idx: usize,
    label: SharedString,
    target: KeyTarget,
    x_frac: f32,
    y_frac: f32,
    /// What the key does now: the bound action's name, or what it does
    /// unbound (see [`unbound_binding`]).
    binding: SharedString,
    /// The icon for [`Self::binding`].
    binding_icon: &'static str,
    /// Whether an action is bound, rather than the key doing what it does
    /// unbound.
    bound: bool,
    /// The glyph the keypad's LCD shows for this key's binding — the agent
    /// paints exactly this icon on the device, so the render mirrors it.
    lcd_icon: Option<&'static str>,
}

impl KeySlot {
    /// The key's name with what it does, for a tooltip or an accessible
    /// label: "Brightness Down Key · Native function".
    fn summary(&self) -> SharedString {
        format!("{} · {}", self.label, self.binding).into()
    }
}

/// Which binding of a key with several the view edits: a keypad grid key's
/// page, and a G-key's M profile.
#[derive(Clone, Copy, Debug)]
struct KeyEditScope {
    page: KeypadPage,
    g_profile: GKeyProfile,
    /// What M1–M3 do on a G-series keyboard.
    gaming_mode: GamingKeyMode,
}

/// One [`KeySlot`] per displayed key, each carrying the action it is bound
/// to right now — the panes and the config panel render the same truth
/// from the same source. Grid keys are read on the scope's page, G-keys in
/// its profile.
fn key_slots_from_layouts(
    layouts: Vec<KeySlotLayout>,
    state: Option<&AppState>,
    scope: KeyEditScope,
) -> Vec<KeySlot> {
    let key_actions = state.map(AppState::keypad_key_actions).unwrap_or_default();
    // Nine-button presses run the M1 slots whatever profile was last edited.
    let g_profile = match scope.gaming_mode {
        GamingKeyMode::Profiles => scope.g_profile,
        GamingKeyMode::NineButtons => GKeyProfile::M1,
    };
    layouts
        .into_iter()
        .enumerate()
        .map(|(idx, mut layout)| {
            layout.target = layout.target.on_page(scope.page).on_profile(g_profile);
            if matches!(layout.target, KeyTarget::GKey(_)) {
                layout.legend = layout.target.label();
            }
            // In profile mode M1–M3 switch profiles and bind nothing; the
            // one being edited reads as the active one.
            if let Some(profile) = profile_key(&layout.target, scope.gaming_mode) {
                return KeySlot {
                    idx,
                    label: layout.legend.into(),
                    x_frac: layout.point.x_frac,
                    y_frac: layout.point.y_frac,
                    binding: state
                        .and_then(|state| state.current_g_key_profile_name(profile))
                        .map_or_else(
                            || tr!("keyboard.g_key_profile", profile => profile.label()),
                            |name| SharedString::from(name.to_owned()),
                        ),
                    binding_icon: NATIVE_FUNCTION_ICON,
                    bound: profile == scope.g_profile,
                    lcd_icon: None,
                    target: layout.target,
                };
            }
            let bound = match &layout.target {
                // The face the keypad shows, which a long press can set
                // without a press action.
                KeyTarget::Grid(slot) => key_actions.get(&ButtonId::Grid(*slot)).cloned(),
                target => state.and_then(|state| bound_action(state, target)),
            };
            let (binding, binding_icon) = match &bound {
                Some(action) => (localized_action_label(action), action_icon_path(action)),
                None => unbound_binding(&layout.target),
            };
            KeySlot {
                idx,
                label: layout.legend.into(),
                x_frac: layout.point.x_frac,
                y_frac: layout.point.y_frac,
                binding,
                binding_icon,
                bound: bound.is_some(),
                lcd_icon: bound
                    .as_ref()
                    .map(|action| ActionRingIcon::for_action(action).asset_path()),
                target: layout.target,
            }
        })
        .collect()
}

/// The action `target` is bound to in the selected device's open profile:
/// a per-device binding (a control or keypad key) or the global F-row map
/// (a function key). `None` when the key is on its native function. A
/// control bound to [`Action::None`] is suppressed, so that is its action; a
/// keypad key is only ever observed, so [`Action::None`] there does nothing.
pub(super) fn bound_action(state: &AppState, target: &KeyTarget) -> Option<Action> {
    match target {
        KeyTarget::Grid(slot) => state
            .button_bindings()
            .get(&ButtonId::Grid(*slot))
            .filter(|action| **action != Action::None)
            .cloned(),
        _ => match target.stored_as() {
            StoredAs::Device(button) => state.button_bindings().get(&button).cloned(),
            StoredAs::Global(trigger) => state.keyboard_bindings().get(trigger).cloned(),
        },
    }
}

/// The profile `target` switches to when it is M1–M3 in profile mode, where
/// it binds nothing.
fn profile_key(target: &KeyTarget, mode: GamingKeyMode) -> Option<GKeyProfile> {
    match target {
        KeyTarget::ModeKey(key) if mode == GamingKeyMode::Profiles => key.profile(),
        _ => None,
    }
}

/// Persist `action` for `target`: controls and keypad keys bind per device
/// (and per open app profile) like any mouse button; a function key binds
/// globally. `None` removes the assignment in this scope (inheriting the
/// device default in an app profile); for a function key it clears the global
/// trigger.
pub(super) fn commit_key_action(
    state: &mut AppState,
    target: &KeyTarget,
    action: Option<Action>,
) -> StateEvents {
    match (target.stored_as(), action) {
        (StoredAs::Device(button), Some(action)) => state.commit_binding(button, action),
        (StoredAs::Device(button), None) => state.clear_binding(button),
        (StoredAs::Global(trigger), action) => {
            state.commit_keyboard_binding(trigger.clone(), action)
        }
    }
}

/// The explicit action in the scope edited by the inspector. In an app profile,
/// an inherited action is not selected; `None` selects the default-profile row.
fn selected_key_action(state: &AppState, target: &KeyTarget) -> Option<Action> {
    let StoredAs::Device(button) = target.stored_as() else {
        return bound_action(state, target);
    };
    if state.editing_app().is_some() {
        state
            .editing_app_overrides()
            .and_then(|overrides| overrides.get(&button))
            .cloned()
    } else {
        bound_action(state, target)
    }
}

/// What the inspector column beside the device shows.
enum Inspector {
    /// Every key and what it does, while no key is selected.
    Overview(gpui::Div),
    /// The selected key's config panel.
    Panel(gpui::Div),
}

/// The two-pane row: keyboard photo (or keypad keycaps) beside the inspector
/// column. The column keeps its size whatever it shows, so the device never
/// moves; a window too narrow for both wraps the column under the device.
#[derive(IntoElement)]
struct InspectorRow {
    device: AnyElement,
    inspector: Inspector,
    /// The column's width and height (see [`inspector_width`] and
    /// [`inspector_height`]).
    column: (f32, f32),
}

impl InspectorRow {
    fn new(device: impl Into<AnyElement>, inspector: Inspector, column: (f32, f32)) -> Self {
        Self {
            device: device.into(),
            inspector,
            column,
        }
    }
}

impl RenderOnce for InspectorRow {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let content = match self.inspector {
            // The resting state appears at once, the way the panel leaves.
            Inspector::Overview(overview) => overview.into_any_element(),
            // The panel fades in where it will stay, drifting the last few
            // pixels out of the gap beside the board. Only opacity and an
            // offset that leaves layout alone move; selecting another key
            // while it is open keeps this element, so it does not replay.
            Inspector::Panel(panel) => div()
                .relative()
                .size_full()
                .child(panel)
                .with_animation(
                    "key-panel-enter",
                    Animation::new(PANEL_ENTER).with_easing(ease_out_quint()),
                    |panel, delta| {
                        panel
                            .opacity(delta)
                            .left(px(-PANEL_ENTER_SHIFT * (1. - delta)))
                    },
                )
                .into_any_element(),
        };
        let (column_w, column_h) = self.column;
        h_flex()
            .w_full()
            .flex_wrap()
            .items_center()
            .justify_center()
            .gap(px(INSPECTOR_GAP))
            .child(self.device)
            .child(
                v_flex()
                    .flex_none()
                    .w(px(column_w))
                    .h(px(column_h))
                    .child(content),
            )
    }
}

/// Every key the tab shows with what it does now, by full name — the room
/// the callouts over the board do not have. A row shares the hover highlight
/// with its key's callout and leader, and opens the key like a click on
/// either.
///
/// The list takes the height its rows need up to `room`, reporting what it
/// took back to `view` for the column (see [`inspector_height`]); past
/// `room` it scrolls, its cut-off edge fading into the card.
fn key_overview(
    slots: &[KeySlot],
    hovered: Option<usize>,
    scroll: &ScrollHandle,
    room: f32,
    view: &Entity<FunctionRowView>,
    pal: &Palette,
) -> gpui::Div {
    let rows = slots
        .iter()
        .map(|slot| overview_row(slot, hovered == Some(slot.idx), view, pal));
    let overflows = scroll.max_offset().y > px(0.);
    let scroll_for_measure = scroll.clone();
    let view_measure = view.clone();
    compact_panel(*pal)
        .debug_selector(|| "key-overview".into())
        .max_h(px(room))
        .child(inspector_title(tr!("device.keys"), pal))
        .child(divider(*pal))
        .child(
            v_flex()
                .relative()
                .min_h_0()
                .child(
                    v_flex()
                        .id("key-overview-scroll")
                        .min_h_0()
                        .track_scroll(scroll)
                        .overflow_y_scroll()
                        .child(
                            v_flex()
                                .children(rows)
                                .when(overflows, |list| list.pb(px(OVERVIEW_FADE_H))),
                        ),
                )
                .when(overflows, |list| {
                    list.child(
                        div()
                            .debug_selector(|| "key-overview-fade".into())
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .h(px(OVERVIEW_FADE_H))
                            .bg(linear_gradient(
                                180.,
                                linear_color_stop(pal.panel.opacity(0.), 0.),
                                linear_color_stop(pal.panel, 1.),
                            )),
                    )
                })
                .vertical_scrollbar(scroll),
        )
        // The card's height is what the column grows to; whether the list
        // scrolls decides its fade, so a change in either lays out again.
        .on_prepaint(move |bounds, _window, cx| {
            let scrolls = scroll_for_measure.max_offset().y > px(0.);
            view_measure.update(cx, |view, cx| {
                view.remeasure(|view| &mut view.overview_h, f32::from(bounds.size.height), cx);
                if scrolls != overflows {
                    cx.notify();
                }
            });
        })
}

/// One key's overview row: its name, and its binding's icon and name.
fn overview_row(
    slot: &KeySlot,
    hovered: bool,
    view: &Entity<FunctionRowView>,
    pal: &Palette,
) -> MenuRow {
    let idx = slot.idx;
    let view_hover = view.clone();
    let view_click = view.clone();
    let binding_color = if slot.bound {
        pal.text_primary
    } else {
        pal.text_muted
    };
    // The column is as wide as the widest row needs, so both texts keep one
    // line; a name too long even for the widest column shares the squeeze
    // with the binding and wraps rather than losing its end.
    MenuRow::new(("key-overview-row", idx))
        .debug_selector(move || format!("key-overview-row-{idx}"))
        .selected(hovered)
        .aria_label(slot.summary())
        .child(
            div()
                .flex_auto()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .line_clamp(2)
                .text_caption()
                .debug_selector(move || format!("key-overview-name-{idx}"))
                .child(slot.label.clone()),
        )
        .child(
            h_flex()
                .min_w_0()
                .items_center()
                .gap(rems(OVERVIEW_ICON_GAP))
                .child(
                    svg()
                        .path(slot.binding_icon)
                        .size(rems(OVERVIEW_ICON))
                        .flex_none()
                        .text_color(binding_color),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .line_clamp(2)
                        .text_caption()
                        .text_color(binding_color)
                        .when(slot.bound, |text| text.font_weight(FontWeight::MEDIUM))
                        .debug_selector(move || format!("key-overview-binding-{idx}"))
                        .child(slot.binding.clone()),
                ),
        )
        .on_hover(move |hovered, _window, cx| {
            let next = (*hovered).then_some(idx);
            view_hover.update(cx, |v, vcx| v.set_hovered_key(next, vcx));
        })
        .on_click(move |_ev, _window, cx| {
            // The key's panel replaces the overview, so this row never
            // sees its hover end.
            view_click.update(cx, |v, vcx| {
                v.set_hovered_key(None, vcx);
                v.click_key(idx, vcx);
            });
        })
}

/// The keyboard photo with callout bubbles above each function key, leader
/// lines, and invisible click-targets over the real keys.
#[derive(IntoElement)]
struct KeyboardPane {
    slots: Vec<KeySlot>,
    image_path: Option<std::path::PathBuf>,
    glow: Option<(Arc<GlowGeometry>, Hsla)>,
    render_size: (f32, f32),
    selected: Option<usize>,
    hovered: Option<usize>,
    view: Entity<FunctionRowView>,
}

impl KeyboardPane {
    fn new(
        slots: Vec<KeySlot>,
        image_path: Option<std::path::PathBuf>,
        glow: Option<(Arc<GlowGeometry>, Hsla)>,
        render_size: (f32, f32),
        view: Entity<FunctionRowView>,
    ) -> Self {
        Self {
            slots,
            image_path,
            glow,
            render_size,
            selected: None,
            hovered: None,
            view,
        }
    }

    #[must_use]
    fn selected(mut self, selected: impl Into<Option<usize>>) -> Self {
        self.selected = selected.into();
        self
    }

    #[must_use]
    fn hovered(mut self, hovered: impl Into<Option<usize>>) -> Self {
        self.hovered = hovered.into();
        self
    }
}

impl RenderOnce for KeyboardPane {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (img_w, img_h) = self.render_size;
        let img_path = self.image_path;
        let view_clone = self.view;
        let selected = self.selected;
        let hovered = self.hovered;
        let pal = theme::palette(cx);
        let markers: Vec<(f32, f32)> = self
            .slots
            .iter()
            .map(|slot| (slot.x_frac, slot.y_frac))
            .collect();
        let layout = callout_layout(&markers, img_w, img_h);
        let image_left = layout.image_left;

        div()
            .relative()
            .w(px(layout.pane_w))
            .h(px(CALLOUT_BAND_H + img_h))
            .child(
            div()
                .absolute()
                .top(px(CALLOUT_BAND_H))
                .left(px(image_left))
                .w(px(img_w))
                .h(px(img_h))
                // The keyboard's RGB paints *behind* the render, so the opaque
                // keys occlude it and the colour only reads through the
                // inter-key gaps — same treatment as the home gallery and the
                // mouse model.
                    .when_some(self.glow, |this, (geom, color)| {
                    this.child(glow_canvas(geom, color))
                })
                    .child(image_or_fallback(img_path, img_w, img_h, &pal)),
            )
            .child(keyboard_leader_canvas(&layout, selected, hovered, img_h))
            .children({
                let view_for_callouts = view_clone.clone();
                let cards = layout.cards.clone();
                self.slots.iter().cloned().map(move |slot| {
                    let highlighted = key_is_highlighted(slot.idx, selected, hovered);
                    KeyCallout {
                        card: cards[slot.idx],
                        slot,
                        highlighted,
                        view: view_for_callouts.clone(),
                    }
                })
            })
            // Click-targets overlay, centered on each key's marker point.
            .child(
                div()
                    .absolute()
                    .top(px(CALLOUT_BAND_H))
                    .left(px(image_left))
                    .w(px(img_w))
                    .h(px(img_h))
                    .children(self.slots.into_iter().map(|slot| {
                    let highlighted = key_is_highlighted(slot.idx, selected, hovered);
                    key_click_target(slot, highlighted, (img_w, img_h), &view_clone)
                })),
            )
    }
}

/// One callout, above its key or beside it (see [`callout_layout`]): the icon
/// of the action bound to the key, or a muted dot while the key keeps its own
/// function. A key pitch has no room for a readable name, so the name and
/// binding live in the tooltip and the inspector's overview.
#[derive(IntoElement)]
struct KeyCallout {
    slot: KeySlot,
    card: CardRect,
    highlighted: bool,
    view: Entity<FunctionRowView>,
}

impl RenderOnce for KeyCallout {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let pal = theme::palette(cx);
        let idx = self.slot.idx;
        let CardRect { left, top, width } = self.card;
        let view_hover = self.view.clone();
        let view_click = self.view;
        let highlighted = self.highlighted;
        let summary = self.slot.summary();
        let tooltip = summary.clone();
        let icon_color = if highlighted {
            rgb(ACCENT_BLUE).into()
        } else if self.slot.bound {
            pal.text_primary
        } else {
            pal.text_muted
        };

        div()
            .id(("key-callout", idx))
            .role(Role::Button)
            .aria_label(summary)
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .absolute()
            .top(px(top))
            .left(px(left))
            .w(px(width))
            .h(px(KEY_CALLOUT_H))
            .flex()
            .justify_center()
            .items_center()
            .rounded_md()
            .border_1()
            .border_color(if highlighted {
                rgb(ACCENT_BLUE).into()
            } else {
                pal.border
            })
            .bg(if highlighted {
                theme::accent_tint()
            } else {
                pal.control
            })
            .cursor_pointer()
            .pressable_fill(highlighted, pal)
            .child(if self.slot.bound {
                svg()
                    .path(self.slot.binding_icon)
                    .size(px(KEY_CALLOUT_ICON))
                    .flex_none()
                    .text_color(icon_color)
                    .into_any_element()
            } else {
                div()
                    .size(px(KEY_CALLOUT_DOT))
                    .rounded_full()
                    .bg(icon_color)
                    .into_any_element()
            })
            .on_hover(move |hovered, _window, cx| {
                let next = (*hovered).then_some(idx);
                view_hover.update(cx, |v, vcx| v.set_hovered_key(next, vcx));
            })
            .on_click(move |_ev, _window, cx| {
                view_click.update(cx, |v, vcx| v.click_key(idx, vcx));
            })
    }
}

/// One invisible click-target over a function key. Selecting it opens the
/// panel; hover/selection draws only a subtle keycap ring on the photo.
fn key_click_target(
    slot: KeySlot,
    highlighted: bool,
    (img_w, img_h): (f32, f32),
    view: &Entity<FunctionRowView>,
) -> impl IntoElement {
    let idx = slot.idx;
    let x_frac = slot.x_frac;
    let y_frac = slot.y_frac;
    let view_hover = view.clone();
    let view_click = view.clone();
    let left = key_target_left_px(x_frac, img_w, KEY_TARGET_W);
    let top = key_target_top_px(y_frac, img_h, KEY_TARGET_H);

    div()
        .id(("key-target", idx))
        .debug_selector(move || format!("key-target-{idx}"))
        .absolute()
        .top(px(top))
        .left(px(left))
        .w(px(KEY_TARGET_W))
        .h(px(KEY_TARGET_H))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(highlighted, |el| {
            el.child(
                div()
                    .w_full()
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(KEY_HOTSPOT_DOT))
                            .h(px(KEY_HOTSPOT_DOT))
                            .rounded_full()
                            .border_1()
                            .border_color(gpui::Hsla::from(rgb(ACCENT_BLUE)))
                            .bg(gpui::Hsla::from(rgb(ACCENT_BLUE))),
                    )
                    .rounded_full()
                    .border_1()
                    .border_color(theme::accent_tint_hover())
                    .bg(theme::accent_tint()),
            )
        })
        .on_hover(move |hovered, _window, cx| {
            let next = (*hovered).then_some(idx);
            view_hover.update(cx, |v, vcx| v.set_hovered_key(next, vcx));
        })
        .on_click(move |_ev, _window, cx| {
            view_click.update(cx, |v, vcx| v.click_key(idx, vcx));
        })
}

/// What an unbound key does, as its binding line reads: a keyboard key keeps
/// its own function, the way the panel's Native function row puts it — not
/// "Off", which reads as if every key were switched off; a keypad grid key
/// has no function of its own and does nothing until something is bound.
fn unbound_binding(target: &KeyTarget) -> (SharedString, &'static str) {
    match target {
        // A G-key or M key OpenLogi runs has no onboard action left.
        KeyTarget::Grid(_) | KeyTarget::GKey(_) | KeyTarget::ModeKey(_) => {
            (tr!("common.off"), action_icon_path(&Action::None))
        }
        KeyTarget::Control(_) | KeyTarget::FunctionKey(_) => {
            (tr!("keyboard.native_function"), NATIVE_FUNCTION_ICON)
        }
    }
}

/// The panel row that removes a key's assignment: back to its native
/// function, or — for a G-key or M key OpenLogi runs, which has none left —
/// off.
fn resting_row(target: &KeyTarget) -> (SharedString, &'static str) {
    match target {
        KeyTarget::GKey(_) | KeyTarget::ModeKey(_) => unbound_binding(target),
        KeyTarget::Control(_) | KeyTarget::Grid(_) | KeyTarget::FunctionKey(_) => {
            (tr!("keyboard.native_function"), NATIVE_FUNCTION_ICON)
        }
    }
}

fn keyboard_leader_canvas(
    layout: &CalloutLayout,
    selected: Option<usize>,
    hovered: Option<usize>,
    img_h: f32,
) -> impl IntoElement {
    let leaders = layout.leaders.clone();
    canvas(
        move |_bounds, _, _| (leaders, selected, hovered),
        move |bounds, payload, window, _app| {
            let (leaders, selected, hovered) = payload;
            paint_keyboard_leaders(bounds, &leaders, selected, hovered, window);
        },
    )
    .absolute()
    .inset_0()
    .w(px(layout.pane_w))
    .h(px(CALLOUT_BAND_H + img_h))
}

fn paint_keyboard_leaders(
    bounds: Bounds<gpui::Pixels>,
    leaders: &[Vec<(f32, f32)>],
    selected: Option<usize>,
    hovered: Option<usize>,
    window: &mut Window,
) {
    for (idx, corners) in leaders.iter().enumerate() {
        let highlighted = key_is_highlighted(idx, selected, hovered);
        let mut path = PathBuilder::stroke(if highlighted { px(2.) } else { px(1.) });
        for (i, &(x, y)) in corners.iter().enumerate() {
            let corner = bounds.origin + point(px(x), px(y));
            if i == 0 {
                path.move_to(corner);
            } else {
                path.line_to(corner);
            }
        }
        if let Ok(path) = path.build() {
            if highlighted {
                window.paint_path(path, rgb(ACCENT_BLUE));
            } else {
                window.paint_path(path, hsla(0., 0., 0.55, 0.35));
            }
        }
    }
}

fn next_selection_after_click(current: Option<usize>, clicked: usize) -> Option<usize> {
    (current != Some(clicked)).then_some(clicked)
}

fn key_is_highlighted(idx: usize, selected: Option<usize>, hovered: Option<usize>) -> bool {
    selected == Some(idx) || hovered == Some(idx)
}

fn key_target_left_px(x_frac: f32, img_w: f32, target_w: f32) -> f32 {
    (x_frac * img_w - target_w / 2.0).clamp(0.0, img_w - target_w)
}

fn key_target_top_px(y_frac: f32, img_h: f32, target_h: f32) -> f32 {
    (y_frac * img_h - target_h / 2.0).clamp(0.0, img_h - target_h)
}

fn binding_after_pick(current: Option<&Action>, picked: Action) -> Option<Action> {
    (current != Some(&picked)).then_some(picked)
}

/// The scrollable config panel for the selected key. Lists the same action
/// catalog the mouse picker uses, plus a Power User section. Renders the rows
/// directly (no popover) in a tall card.
impl FunctionRowView {
    fn config_panel(
        &self,
        selected_idx: usize,
        slots: &[KeySlot],
        view: &Entity<Self>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let pal = theme::palette(cx);
        let slot = &slots[selected_idx];
        let target = slot.target.clone();
        let key_name = slot.label.clone();

        // If an editor is active, render it instead of the list.
        if let Some(kind) = self.active_editor {
            return super::editors::editor_card(
                target,
                kind,
                self.text_state.clone(),
                self.workflow_draft.clone(),
                self.editor_invalid,
                view,
                pal,
            );
        }

        let state = AppState::try_read(cx);
        if let Some(panel) =
            state.and_then(|state| profile_key_panel(state, &target, &key_name, &pal))
        {
            return panel;
        }
        let current = state.and_then(|state| selected_key_action(state, &target));
        let uses_default = current.is_none();
        let app = state.and_then(|state| binding_app(state, &target));

        let view_for_pick = view.clone();
        let target_for_pick = target.clone();
        let current_for_pick = current.clone();
        let on_pick: PickFn = Rc::new(move |action, _window, cx| {
            AppState::apply(cx, |state| {
                commit_key_action(
                    state,
                    &target_for_pick,
                    binding_after_pick(current_for_pick.as_ref(), action),
                )
            });
            view_for_pick.update(cx, |_, vcx| vcx.notify());
        });

        let mut rows = Vec::new();
        // Unbound is not a catalog action. A control rests on its firmware
        // function (#1172); a function key rests on the key itself. The row
        // clears the assignment, inheriting the device default in an app profile.
        let view_for_native = view.clone();
        let target_for_native = target.clone();
        let (resting, resting_icon) = resting_row(&target);
        rows.push(
            v_flex().child(
                MenuRow::new("panel-native")
                    .debug_selector(|| "key-panel-native".into())
                    .selected(uses_default)
                    .role(Role::MenuItem)
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(
                                svg()
                                    .path(resting_icon)
                                    .size_4()
                                    .flex_none()
                                    .text_color(pal.text_muted),
                            )
                            .child(div().child(if app.is_some() {
                                tr!("profiles.use_the_default_profile")
                            } else {
                                resting
                            })),
                    )
                    .when(uses_default, |row| {
                        row.child(
                            div()
                                .debug_selector(|| "key-panel-native-check".into())
                                .flex_none()
                                .size_3()
                                .child(
                                    gpui_kit::component::Icon::new(
                                        gpui_kit::component::IconName::Check,
                                    )
                                    .size_3()
                                    .text_color(rgb(ACCENT_BLUE)),
                                ),
                        )
                    })
                    .on_click(move |_ev, _window, cx| {
                        AppState::apply(cx, |state| {
                            commit_key_action(state, &target_for_native, None)
                        });
                        view_for_native.update(cx, |_, vcx| vcx.notify());
                    }),
            ),
        );
        rows.extend(panel_action_rows(current.as_ref(), &on_pick, view, &pal));

        compact_panel(pal)
            .max_h_full()
            .child(title_header(&key_name, app.as_deref(), &pal))
            .child(divider(pal))
            .child(editor_scroll_list("key-panel-scroll", rows))
    }
}

/// The app profile `target` binds in: the open one for a per-device key,
/// which binds inside it like a mouse button; none for an F-row trigger,
/// which binds across apps.
fn binding_app(state: &AppState, target: &KeyTarget) -> Option<String> {
    match target.stored_as() {
        StoredAs::Device(_) => state.editing_app().map(|app| profile_name(state, app)),
        StoredAs::Global(_) => None,
    }
}

/// The panel of M1–M3 in profile mode, or `None` for any other key: they
/// bind nothing, they switch which profile G1–G5 run, selecting one edits
/// that profile's G-keys, and the panel names the profile.
fn profile_key_panel(
    state: &AppState,
    target: &KeyTarget,
    key_name: &SharedString,
    pal: &Palette,
) -> Option<gpui::Div> {
    let profile = profile_key(target, state.current_gaming_keys()?.mode)?;
    let name = state.current_g_key_profile_name(profile).map(str::to_owned);
    // A named profile reads as its key and its name: "M2 · Work".
    let title = name.as_ref().map_or_else(
        || key_name.clone(),
        |name| format!("{key_name} · {name}").into(),
    );
    let rename = control_button("g-key-profile-rename")
        .outline()
        .label(tr!("keyboard.set_profile_name", profile => profile.label()))
        .on_click(move |_, window, cx| {
            gaming::open_profile_name_dialog(window, cx, profile, name.clone().unwrap_or_default());
        });
    let panel =
        compact_panel(*pal)
            .max_h_full()
            .child(inspector_title(title, pal))
            .child(divider(*pal))
            .child(
                v_flex()
                    .px_2()
                    .py_2()
                    .gap_3()
                    .child(div().text_color(pal.text_muted).child(
                        tr!("keyboard.g_key_profile_description", profile => profile.label()),
                    ))
                    .child(rename),
            );
    Some(panel)
}

/// The panel's title — which key is selected, e.g. "F1" or "Screen Capture
/// Key", and the app profile it binds in when one is open.
fn title_header(key_name: &SharedString, app: Option<&str>, pal: &Palette) -> impl IntoElement {
    let title = match app {
        Some(app) => tr!("actions.bind_control_in_app", name => key_name.clone(), app => app),
        None => tr!("actions.bind_control", name => key_name.clone()),
    };
    inspector_title(title, pal)
}

/// A heading in the inspector column, over the overview or a key's panel.
fn inspector_title(title: SharedString, pal: &Palette) -> impl IntoElement {
    h_flex()
        .items_center()
        .justify_between()
        .px_2()
        .pb_1()
        .child(
            div()
                .text_caption()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(pal.text_muted)
                .child(title),
        )
}

/// The action rows + a Power User section, mirroring the picker's list but
/// adapted for the panel context (no popover dismissal).
fn panel_action_rows(
    current: Option<&Action>,
    on_pick: &PickFn,
    view: &Entity<FunctionRowView>,
    pal: &Palette,
) -> Vec<gpui::Div> {
    let mut children = action_rows("panel-action", current, on_pick, *pal);

    let power_user_actions: &[(PowerUserKind, SharedString, &'static str)] = &[
        (
            PowerUserKind::CustomShortcut,
            tr!("action_ring.custom_shortcut"),
            "action-icons/keyboard.svg",
        ),
        (
            PowerUserKind::OpenApplication,
            tr!("action_ring.open_application_or_folder"),
            "action-icons/grid-3x3.svg",
        ),
        (
            PowerUserKind::TypeText,
            tr!("actions.type_text"),
            "action-icons/keyboard.svg",
        ),
        (
            PowerUserKind::RunAppleScript,
            tr!("actions.run_applescript"),
            "action-icons/terminal.svg",
        ),
        (
            PowerUserKind::RunShellCommand,
            tr!("actions.run_shell_command"),
            "action-icons/terminal.svg",
        ),
        (
            PowerUserKind::Workflow,
            tr!("actions.workflow"),
            "action-icons/list-checks.svg",
        ),
    ];

    children.push(
        v_flex()
            .child(editor_section(tr!("actions.power_user").to_string(), *pal))
            .children(power_user_actions.iter().enumerate().map(
                |(idx, (kind, label, icon_path))| {
                    let kind = *kind;
                    let view = view.clone();
                    let selected = power_user_selected(current, kind);
                    MenuRow::new(format!("panel-power-{idx}"))
                        .selected(selected)
                        .role(Role::MenuItem)
                        .aria_label(label.clone())
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    svg()
                                        .path(*icon_path)
                                        .size_4()
                                        .flex_none()
                                        .text_color(pal.text_muted),
                                )
                                .child(div().child(label.clone())),
                        )
                        .when(selected, |s| {
                            s.child(
                                gpui_kit::component::Icon::new(
                                    gpui_kit::component::IconName::Check,
                                )
                                .size_3()
                                .text_color(rgb(ACCENT_BLUE)),
                            )
                        })
                        .on_click(move |_ev, _window, cx| {
                            view.update(cx, |v, vcx| v.open_editor(kind, vcx));
                        })
                },
            )),
    );
    children
}

/// Whether `current` is the action the `kind` editor makes, so its row reads
/// as the key's binding.
fn power_user_selected(current: Option<&Action>, kind: PowerUserKind) -> bool {
    matches!(
        (current, kind),
        (
            Some(Action::CustomShortcut(_)),
            PowerUserKind::CustomShortcut
        ) | (
            Some(Action::OpenApplication(_)),
            PowerUserKind::OpenApplication
        ) | (Some(Action::TypeText(_)), PowerUserKind::TypeText)
            | (
                Some(Action::RunAppleScript(_)),
                PowerUserKind::RunAppleScript
            )
            | (
                Some(Action::RunShellCommand(_)),
                PowerUserKind::RunShellCommand
            )
            | (Some(Action::Workflow(_)), PowerUserKind::Workflow)
    )
}

/// The keyboard image, or a labeled placeholder when no asset resolved. The
/// element is sized to the PNG's own aspect (see [`keyboard_render_size`]), so
/// the contain-fit paints edge to edge and the marker overlays stay registered.
fn image_or_fallback(
    img_path: Option<std::path::PathBuf>,
    img_w: f32,
    img_h: f32,
    pal: &Palette,
) -> AnyElement {
    match img_path {
        Some(path) if path.exists() => img(path).w(px(img_w)).h(px(img_h)).into_any_element(),
        Some(_) | None => div()
            .w(px(img_w))
            .h(px(160.))
            .rounded_md()
            .border_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .flex()
            .items_center()
            .justify_center()
            .text_color(pal.text_muted)
            .child(tr!("keyboard.no_keyboard_image_available"))
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests;
