//! Onboard profile editor. Drafts stay here; device I/O goes through AppState's IPC sender.
use crate::{
    state::AppState,
    ui::{
        components::{control_button, control_input},
        theme::{self, Typography as _},
    },
};
use gpui::AppContext as _;
use gpui::{App, Context, Entity, SharedString, Window};
use gpui_component::input::InputState;
use openlogi_core::hid::DeviceRoute;
use openlogi_ipc::gaming::{
    GamingAction, GamingAssignment, GamingCommand, GamingDraft, GamingProfile, GamingSnapshot,
};

mod keyboard;
#[cfg(test)]
mod tests;
mod view;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Assignments,
    Sensitivity,
    Profiles,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layer {
    Normal,
    Shifted,
}

pub struct GamingPanel {
    route: Option<DeviceRoute>,
    snapshot: Option<GamingSnapshot>,
    draft: GamingDraft,
    name: Entity<InputState>,
    shortcut: Entity<InputState>,
    selected_button: u8,
    page: Page,
    dpi: [Entity<InputState>; 5],
    busy: bool,
    needs_refresh: bool,
    message: String,
    error: bool,
    layer: Layer,
    _input_observers: Vec<gpui::Subscription>,
    _state_observer: Option<gpui::Subscription>,
}

impl GamingPanel {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state_observer = AppState::try_read(cx)
            .is_some()
            .then(|| AppState::repaint_on(cx, |_| false));
        let shortcut = cx.new(|cx| InputState::new(window, cx));
        let name = cx.new(|cx| InputState::new(window, cx));
        let dpi: [Entity<InputState>; 5] =
            std::array::from_fn(|_| cx.new(|cx| InputState::new(window, cx)));
        let observers = std::iter::once(&name)
            .chain(dpi.iter())
            .map(|input| cx.observe(input, |_, _, cx| cx.notify()))
            .collect();
        Self {
            route: None,
            snapshot: None,
            draft: GamingDraft::default(),
            name,
            shortcut,
            selected_button: 0,
            page: Page::Assignments,
            dpi,
            _input_observers: observers,
            _state_observer: state_observer,
            busy: false,
            needs_refresh: false,
            message: String::new(),
            error: false,
            layer: Layer::Normal,
        }
    }

    fn current_route(cx: &App) -> Option<DeviceRoute> {
        AppState::try_read(cx)
            .and_then(AppState::current_record)
            .filter(|r| r.online)
            .and_then(|r| r.route.clone())
    }

    fn profile(&self) -> Option<&GamingProfile> {
        self.snapshot
            .as_ref()?
            .profiles
            .iter()
            .find(|p| p.sector == self.draft.sector)
    }

    fn dirty(&self, cx: &App) -> bool {
        self.profile().is_some_and(|p| {
            self.name.read(cx).value().as_ref() != p.name
                || self
                    .dpi
                    .iter()
                    .zip(p.dpi)
                    .any(|(input, value)| input.read(cx).value().as_ref() != value.to_string())
                || self.draft.report_rate_hz != rate(p.report_interval_ms)
                || self.draft.default_dpi_slot != p.default_dpi_slot
                || self.draft.shift_dpi_slot != p.shift_dpi_slot
                || !self.draft.buttons.is_empty()
        })
    }

    fn select(&mut self, sector: u16, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self
            .snapshot
            .as_ref()
            .and_then(|s| s.profiles.iter().find(|p| p.sector == sector))
            .cloned()
        else {
            return;
        };
        self.name
            .update(cx, |input, cx| input.set_value(p.name.clone(), window, cx));
        for (input, dpi) in self.dpi.iter().zip(p.dpi) {
            input.update(cx, |input, cx| input.set_value(dpi.to_string(), window, cx));
        }
        self.draft = GamingDraft {
            sector,
            name: p.name,
            report_rate_hz: rate(p.report_interval_ms),
            dpi: p.dpi,
            default_dpi_slot: p.default_dpi_slot,
            shift_dpi_slot: p.shift_dpi_slot,
            buttons: Vec::new(),
        };
        cx.notify();
    }

    fn collect_draft(&self, cx: &App) -> Result<GamingDraft, String> {
        let mut draft = self.draft.clone();
        draft.name = self.name.read(cx).value().to_string();
        for (ix, input) in self.dpi.iter().enumerate() {
            draft.dpi[ix] = input
                .read(cx)
                .value()
                .trim()
                .parse()
                .map_err(|_| tr!("gaming.invalid_dpi").to_string())?;
        }
        if draft
            .dpi
            .iter()
            .any(|v| *v != 0 && (!(100..=25600).contains(v) || v % 50 != 0))
            || [draft.default_dpi_slot, draft.shift_dpi_slot]
                .iter()
                .any(|i| draft.dpi.get(usize::from(*i)).is_none_or(|v| *v == 0))
        {
            return Err(tr!("gaming.invalid_dpi").to_string());
        }
        Ok(draft)
    }

    fn submit(&mut self, apply: bool, window: &mut Window, cx: &mut Context<Self>) {
        let draft = match self.collect_draft(cx) {
            Ok(draft) => draft,
            Err(error) => {
                self.message = error;
                self.error = true;
                cx.notify();
                return;
            }
        };
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let backup_json = snapshot.backup_json.clone();
        let command = if apply {
            GamingCommand::Apply { backup_json, draft }
        } else {
            GamingCommand::Prepare { backup_json, draft }
        };
        self.request(command, window, cx);
    }

    fn request(&mut self, command: GamingCommand, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(route) = Self::current_route(cx) else {
            self.message = tr!("gaming.offline").into();
            self.error = true;
            cx.notify();
            return;
        };
        if !matches!(command, GamingCommand::Read) && self.route.as_ref() != Some(&route) {
            return;
        }
        let applied = matches!(command, GamingCommand::Apply { .. });
        let preserve = matches!(
            command,
            GamingCommand::Prepare { .. } | GamingCommand::Export
        );
        let mutation = matches!(
            command,
            GamingCommand::Apply { .. } | GamingCommand::SetMode(_) | GamingCommand::Select(_)
        );
        let (reply, received) = tokio::sync::oneshot::channel();
        let sender = AppState::global(cx).read(cx).ipc_sender();
        let _ = sender.send(
            crate::services::ipc::Gaming {
                route: route.clone(),
                command,
                reply,
            }
            .into(),
        );
        self.busy = true;
        self.error = false;
        self.message = tr!("gaming.working").into();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = received
                .await
                .unwrap_or_else(|_| Err("Agent unavailable".into()));
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if Self::current_route(cx).as_ref() != Some(&route) {
                    cx.notify();
                    return;
                }
                match result {
                    Ok(snapshot) => {
                        this.message = snapshot.saved_path.as_ref().map_or_else(
                            || tr!("gaming.ready").to_string(),
                            |path| {
                                if applied {
                                    tr!("gaming.written", path = path).to_string()
                                } else {
                                    tr!("gaming.saved", path = path).to_string()
                                }
                            },
                        );
                        if !preserve {
                            let sector = if snapshot
                                .profiles
                                .iter()
                                .any(|p| p.sector == this.draft.sector)
                            {
                                this.draft.sector
                            } else {
                                snapshot
                                    .profiles
                                    .iter()
                                    .find(|p| p.sector == snapshot.active_profile)
                                    .or_else(|| snapshot.profiles.first())
                                    .map_or(0, |p| p.sector)
                            };
                            this.snapshot = Some(snapshot);
                            this.route = Some(route);
                            this.needs_refresh = false;
                            this.select(sector, window, cx);
                            if applied {
                                this.page = Page::Profiles;
                            }
                        }
                    }
                    Err(error) => {
                        this.message = error;
                        this.error = true;
                        this.needs_refresh |= mutation;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn assign(&mut self, action: Option<GamingAction>, cx: &mut Context<Self>) {
        if self.busy || self.needs_refresh {
            return;
        }
        let index = self.selected_button;
        let shifted = self.layer == Layer::Shifted;
        self.draft
            .buttons
            .retain(|b| b.index != index || b.shifted != shifted);
        if let Some(action) = action {
            self.draft.buttons.push(GamingAssignment {
                index,
                shifted,
                action,
            });
        }
        cx.notify();
    }

    fn binding_label(&self, index: usize) -> SharedString {
        let pending =
            self.draft.buttons.iter().find(|b| {
                usize::from(b.index) == index && b.shifted == (self.layer == Layer::Shifted)
            });
        if let Some(pending) = pending {
            return action_name(&pending.action);
        }
        let profile = self.profile();
        let original = profile.and_then(|p| {
            if self.layer == Layer::Shifted {
                p.shifted_buttons.get(index)
            } else {
                p.buttons.get(index)
            }
        });
        original.map_or_else(|| tr!("gaming.unassigned"), |s| original_name(s))
    }
}

fn rate(interval: u8) -> u16 {
    if interval == 0 {
        0
    } else {
        1000 / u16::from(interval)
    }
}

fn action_name(action: &GamingAction) -> SharedString {
    match action {
        GamingAction::Mouse(button) => match button {
            1 => tr!("gaming.left_click"),
            2 => tr!("gaming.right_click"),
            3 => tr!("gaming.middle_click"),
            4 => tr!("gaming.back"),
            5 => tr!("gaming.forward"),
            _ => tr!("gaming.mouse_action", number = button),
        },
        GamingAction::Special(code) => match code {
            1 => tr!("gaming.tilt_left"),
            2 => tr!("gaming.tilt_right"),
            3 => tr!("gaming.dpi_next"),
            4 => tr!("gaming.dpi_previous"),
            5 => tr!("gaming.dpi_cycle"),
            6 => tr!("gaming.dpi_default"),
            7 => tr!("gaming.dpi_shift"),
            8 => tr!("gaming.profile_next"),
            9 => tr!("gaming.profile_previous"),
            10 => tr!("gaming.profile_cycle"),
            11 => "G-Shift".into(),
            12 => tr!("gaming.battery"),
            _ => format!("Special {code}").into(),
        },
        GamingAction::Key { usage, modifiers } => keyboard::label(*usage, *modifiers).into(),
        GamingAction::Consumer(0xe9) => tr!("gaming.volume_up"),
        GamingAction::Consumer(0xea) => tr!("gaming.volume_down"),
        GamingAction::Consumer(0xe2) => tr!("gaming.mute"),
        GamingAction::Consumer(0xcd) => tr!("gaming.play_pause"),
        GamingAction::Consumer(usage) => format!("Consumer HID {usage:#x}").into(),
        GamingAction::Disabled => tr!("gaming.unassigned"),
    }
}

fn original_name(description: &str) -> SharedString {
    if let Some((usage, modifiers)) = description
        .strip_prefix("keyboard HID 0x")
        .and_then(|s| s.split_once(", modifiers 0x"))
        .and_then(|(u, m)| {
            Some((
                u8::from_str_radix(u, 16).ok()?,
                u8::from_str_radix(m, 16).ok()?,
            ))
        })
    {
        return keyboard::label(usage, modifiers).into();
    }
    let action = match description {
        "mouse mask 0x0001" => GamingAction::Mouse(1),
        "mouse mask 0x0002" => GamingAction::Mouse(2),
        "mouse mask 0x0004" => GamingAction::Mouse(3),
        "mouse mask 0x0008" => GamingAction::Mouse(4),
        "mouse mask 0x0010" => GamingAction::Mouse(5),
        "tilt left" => GamingAction::Special(1),
        "tilt right" => GamingAction::Special(2),
        "DPI next" => GamingAction::Special(3),
        "DPI previous" => GamingAction::Special(4),
        "DPI cycle" => GamingAction::Special(5),
        "DPI default" => GamingAction::Special(6),
        "DPI shift" => GamingAction::Special(7),
        "profile next" => GamingAction::Special(8),
        "profile previous" => GamingAction::Special(9),
        "profile cycle" => GamingAction::Special(10),
        "G-Shift" => GamingAction::Special(11),
        "battery" => GamingAction::Special(12),
        "disabled" => GamingAction::Disabled,
        _ => return description.to_string().into(),
    };
    action_name(&action)
}
