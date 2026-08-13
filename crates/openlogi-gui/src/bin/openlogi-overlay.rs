//! Lightweight GPUI host for the cursor-centred Actions Ring.
//!
//! This process is a pure IPC client. The agent owns HID++, session validation,
//! haptic output, and action execution; the overlay only renders the
//! agent-snapshotted actions and reports hover/activate/cancel interactions.

#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

rust_i18n::i18n!("locales", fallback = "en");

#[path = "../action_ring_geometry.rs"]
mod action_ring_geometry;
#[path = "../action_ring_icons.rs"]
mod action_ring_icons;
#[path = "../app_assets.rs"]
mod app_assets;
#[path = "../locale.rs"]
mod locale;
#[path = "../platform/overlay.rs"]
mod overlay_platform;

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use gpui::{
    AppContext as _, Bounds, Context, InteractiveElement, IntoElement, ParentElement, Pixels,
    Point, Render, SharedString, Size, StatefulInteractiveElement as _, Styled, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, hsla, point,
    prelude::FluentBuilder as _, px, svg,
};
use openlogi_agent_core::ipc::{ActionRingInvocation, AgentClient, PROTOCOL_VERSION};
use openlogi_core::binding::ActionRingSlot;
use tarpc::{client, context};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use tracing_subscriber::EnvFilter;

const WINDOW_SIZE: f32 = 360.0;
const SLOT_SIZE: f32 = 54.0;
const RADIUS: f32 = 122.0;
const DISPLAY_LIFETIME: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverlayCommand {
    Hover {
        session_id: u64,
        slot: ActionRingSlot,
    },
    Activate {
        session_id: u64,
        slot: ActionRingSlot,
    },
    Cancel {
        session_id: u64,
    },
}

impl OverlayCommand {
    const fn is_terminal(self) -> bool {
        matches!(self, Self::Activate { .. } | Self::Cancel { .. })
    }
}

struct Ipc {
    invocations: mpsc::UnboundedReceiver<ActionRingInvocation>,
    commands: mpsc::UnboundedSender<OverlayCommand>,
}

struct RingView {
    invocation: ActionRingInvocation,
    commands: mpsc::UnboundedSender<OverlayCommand>,
    hovered: Option<ActionRingSlot>,
}

impl RingView {
    fn slot_position(slot: ActionRingSlot) -> (f32, f32) {
        let (x, y) = action_ring_geometry::slot_offset(slot);
        (
            WINDOW_SIZE / 2.0 + x * RADIUS - SLOT_SIZE / 2.0,
            WINDOW_SIZE / 2.0 + y * RADIUS - SLOT_SIZE / 2.0,
        )
    }

    fn slot_element(
        &self,
        slot: ActionRingSlot,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let presentation = self.invocation.slots.get(&slot)?;
        let icon_path = action_ring_icons::ring_icon_path(presentation.icon);
        let selected = self.hovered == Some(slot);
        let (left, top) = Self::slot_position(slot);
        let session_id = self.invocation.session_id;
        let activate = self.commands.clone();
        Some(
            div()
                .id(("ring-slot", slot.index()))
                .absolute()
                .left(px(left))
                .top(px(top))
                .size(px(SLOT_SIZE))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(if selected {
                    hsla(0.59, 0.72, 0.48, 1.0)
                } else {
                    hsla(0.0, 0.0, 0.16, 0.98)
                })
                .when(selected, |slot| {
                    slot.border_2().border_color(hsla(0.59, 0.90, 0.72, 1.0))
                })
                .shadow_md()
                .text_color(hsla(0.0, 0.0, 0.98, 1.0))
                .cursor_pointer()
                .child(
                    svg()
                        .path(icon_path)
                        .size(px(22.0))
                        .text_color(hsla(0.0, 0.0, 0.98, 1.0)),
                )
                .on_hover(cx.listener(move |this, hovered, _, cx| {
                    if *hovered && this.hovered != Some(slot) {
                        this.hovered = Some(slot);
                        let _ = this
                            .commands
                            .send(OverlayCommand::Hover { session_id, slot });
                        cx.notify();
                    } else if !*hovered && this.hovered == Some(slot) {
                        this.hovered = None;
                        cx.notify();
                    }
                }))
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    let _ = activate.send(OverlayCommand::Activate { session_id, slot });
                    window.remove_window();
                })
                .into_any_element(),
        )
    }
}

impl Render for RingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session_id = self.invocation.session_id;
        let root_commands = self.commands.clone();
        let center_commands = self.commands.clone();
        let hovered_label = self.hovered.and_then(|slot| {
            let presentation = self.invocation.slots.get(&slot)?;
            // User-authored labels render verbatim: passing them through the
            // localization table would translate any label that happens to
            // collide with a known key ("Copy" → "Copier" under fr).
            let label = if presentation.literal {
                presentation.label.clone()
            } else {
                rust_i18n::t!(presentation.label.as_str()).into_owned()
            };
            Some(SharedString::from(label))
        });
        let slots = ActionRingSlot::ALL
            .into_iter()
            .filter_map(|slot| self.slot_element(slot, cx))
            .collect::<Vec<_>>();

        div()
            .id("ring-root")
            .relative()
            .size_full()
            .child(
                div()
                    .absolute()
                    .left(px(18.0))
                    .top(px(18.0))
                    .size(px(WINDOW_SIZE - 36.0))
                    .rounded_full()
                    .bg(hsla(0.0, 0.0, 0.06, 0.82))
                    .shadow_lg(),
            )
            .children(slots)
            .child(
                div()
                    .id("ring-cancel")
                    .absolute()
                    .left(px(WINDOW_SIZE / 2.0 - 24.0))
                    .top(px(WINDOW_SIZE / 2.0 - 24.0))
                    .size(px(48.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(hsla(0.0, 0.0, 0.20, 0.98))
                    .text_color(hsla(0.0, 0.0, 0.82, 1.0))
                    .text_lg()
                    .cursor_pointer()
                    .child("×")
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        let _ = center_commands.send(OverlayCommand::Cancel { session_id });
                        window.remove_window();
                    }),
            )
            .when_some(hovered_label, |ring, label| {
                ring.child(
                    div()
                        .absolute()
                        .left(px(WINDOW_SIZE / 2.0 - 80.0))
                        .top(px(WINDOW_SIZE / 2.0 + 34.0))
                        .w(px(160.0))
                        .text_center()
                        .text_sm()
                        .text_color(hsla(0.0, 0.0, 0.94, 1.0))
                        .child(label),
                )
            })
            .on_click(move |_, window, _| {
                let _ = root_commands.send(OverlayCommand::Cancel { session_id });
                window.remove_window();
            })
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            EnvFilter::try_from_env("OPENLOGI_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    rust_i18n::set_locale(locale::resolve(None));
    let _guard = openlogi_core::single_instance::acquire("overlay.lock")
        .context("Actions Ring overlay single-instance check")?;
    let Ipc {
        mut invocations,
        commands,
    } = spawn_ipc();

    let app = gpui_platform::application().with_assets(app_assets::AppAssets);
    app.run(move |cx| {
        overlay_platform::configure_application();
        let live_session = Arc::new(ClickAwaySession::new());
        spawn_click_away_dismissal(cx, Arc::clone(&live_session));
        cx.spawn(async move |cx| {
            while let Some(invocation) = invocations.recv().await {
                rust_i18n::set_locale(locale::resolve(invocation.language.as_deref()));
                cx.update(|cx| {
                    live_session.clear();
                    for handle in cx.windows() {
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                    }
                    let options = ring_window_options(cx);
                    let commands = commands.clone();
                    let timeout_commands = commands.clone();
                    let session_id = invocation.session_id;
                    match cx.open_window(options, |_, cx| {
                        cx.new(|_| RingView {
                            invocation,
                            commands,
                            hovered: None,
                        })
                    }) {
                        Ok(handle) => {
                            live_session.set(session_id);
                            overlay_platform::configure_windows();
                            cx.spawn(async move |cx| {
                                cx.background_executor().timer(DISPLAY_LIFETIME).await;
                                if handle
                                    .update(cx, |_, window, _| window.remove_window())
                                    .is_ok()
                                {
                                    let _ = timeout_commands
                                        .send(OverlayCommand::Cancel { session_id });
                                }
                            })
                            .detach();
                        }
                        Err(error) => warn!(%error, "could not open Actions Ring window"),
                    }
                });
            }
        })
        .detach();
    });
    Ok(())
}

/// Session the click-away monitor may dismiss; `0` means no ring is showing.
struct ClickAwaySession(AtomicU64);

impl ClickAwaySession {
    const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Publish which ring is showing, or `0` while none is.
    fn set(&self, session_id: u64) {
        self.0.store(session_id, Ordering::Release);
    }

    /// Forget the showing session so later clicks cannot name it.
    fn clear(&self) {
        self.set(0);
    }

    /// Session id at click time, or `None` when no ring is showing.
    #[must_use]
    fn observe(&self) -> Option<u64> {
        match self.0.load(Ordering::Acquire) {
            0 => None,
            session_id => Some(session_id),
        }
    }
}

/// True when the click still names the ring that is open.
#[must_use]
const fn click_away_targets(observed: u64, open: u64) -> bool {
    observed != 0 && observed == open
}

/// Dismiss a showing ring when the user clicks anywhere off it, the way a
/// transient popup closes on click-away — without swallowing that click.
///
/// The ring window only covers its own 360×360 bounds, so an outside click
/// never reaches the window's handlers. A global monitor closes the gap:
/// macOS only delivers it events routed to *other* applications, so clicks on
/// the ring itself can't race the slot/cancel handlers, and monitors can't
/// consume events, so the click lands where the user aimed it. The handler
/// snapshots the showing session onto a channel; teardown runs on the GPUI
/// side, and only that session is cancelled so a queued click cannot close a
/// ring that opened afterward.
fn spawn_click_away_dismissal(cx: &mut gpui::App, live: Arc<ClickAwaySession>) {
    let (clicks_tx, mut clicks) = mpsc::unbounded_channel();
    let monitor = overlay_platform::watch_clicks_outside(move || {
        if let Some(session_id) = live.observe() {
            let _ = clicks_tx.send(session_id);
        }
    });
    if monitor.is_none() && cfg!(target_os = "macos") {
        warn!(
            "could not install the click-away monitor; the ring will not dismiss on outside clicks"
        );
    }
    cx.spawn(async move |cx| {
        #[cfg(target_os = "macos")]
        let _monitor = monitor;
        #[cfg(not(target_os = "macos"))]
        drop_unused_click_away_monitor(monitor);
        while let Some(session_id) = clicks.recv().await {
            cx.update(|cx| dismiss_click_away(cx, session_id));
        }
    })
    .detach();
}

/// Drop the stub monitor; non-macOS has no native owner to keep alive.
#[cfg(not(target_os = "macos"))]
const fn drop_unused_click_away_monitor(_monitor: Option<overlay_platform::ClickAwayMonitor>) {}

/// Cancel the open ring only if it is still the session the click named.
fn dismiss_click_away(cx: &mut gpui::App, session_id: u64) {
    for handle in cx.windows() {
        let Some(ring) = handle.downcast::<RingView>() else {
            continue;
        };
        let _ = ring.update(cx, |view, window, _| {
            if !click_away_targets(session_id, view.invocation.session_id) {
                return;
            }
            let _ = view.commands.send(OverlayCommand::Cancel {
                session_id: view.invocation.session_id,
            });
            window.remove_window();
        });
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "native cursor coordinates are screen-sized and exactly usable as GPUI f32 pixels"
)]
fn ring_window_options(cx: &mut gpui::App) -> WindowOptions {
    let cursor = openlogi_hook::cursor_position();
    let cursor_point = cursor.map(|cursor| point(px(cursor.x as f32), px(cursor.y as f32)));
    let display = cursor_point
        .and_then(|cursor| {
            cx.displays()
                .into_iter()
                .find(|display| display.bounds().contains(&cursor))
        })
        .or_else(|| cx.primary_display());
    let center = cursor_point
        .or_else(|| display.as_ref().map(|display| display.bounds().center()))
        .unwrap_or_default();
    let size = Size::new(px(WINDOW_SIZE), px(WINDOW_SIZE));
    let desired_origin = point(center.x - size.width / 2.0, center.y - size.height / 2.0);
    let origin = display.as_ref().map_or(desired_origin, |display| {
        clamp_window_origin(desired_origin, size, display.bounds())
    });
    let bounds = Bounds::new(origin, size);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: None,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        display_id: display.map(|display| display.id()),
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some("openlogi-action-ring".to_string()),
        ..WindowOptions::default()
    }
}

fn clamp_window_origin(
    desired: Point<Pixels>,
    window_size: Size<Pixels>,
    display: Bounds<Pixels>,
) -> Point<Pixels> {
    let max = point(
        display.right() - window_size.width,
        display.bottom() - window_size.height,
    );
    desired.clamp(&display.origin, &max)
}

fn spawn_ipc() -> Ipc {
    let (invocation_tx, invocations) = mpsc::unbounded_channel();
    let (commands, mut command_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                warn!(%error, "overlay IPC runtime initialization failed");
                return;
            }
        };
        runtime.block_on(async move {
            tokio::join!(
                poll_invocations(invocation_tx),
                send_commands(&mut command_rx)
            );
        });
    });
    Ipc {
        invocations,
        commands,
    }
}

async fn connect() -> Option<AgentClient> {
    let stream = openlogi_agent_core::transport::connect().await.ok()?;
    let transport = openlogi_agent_core::transport::wrap(stream);
    let client = AgentClient::new(client::Config::default(), transport).spawn();
    let version = client.protocol_version(context::current()).await.ok()?;
    (version == PROTOCOL_VERSION).then_some(client)
}

async fn poll_invocations(tx: mpsc::UnboundedSender<ActionRingInvocation>) {
    let mut client = None;
    loop {
        if client.is_none() {
            client = connect().await;
        }
        let Some(active) = client.as_ref() else {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        let mut ctx = context::current();
        ctx.deadline = std::time::Instant::now() + Duration::from_secs(25);
        match active.next_action_ring(ctx).await {
            Ok(Some(invocation)) => {
                if tx.send(invocation).is_err() {
                    return;
                }
            }
            Ok(None) => {}
            Err(error) => {
                debug!(?error, "Actions Ring long-poll disconnected");
                client = None;
            }
        }
    }
}

fn coalesce_command(current: OverlayCommand, next: OverlayCommand) -> OverlayCommand {
    match next {
        OverlayCommand::Hover { .. }
            if matches!(
                current,
                OverlayCommand::Activate { .. } | OverlayCommand::Cancel { .. }
            ) =>
        {
            current
        }
        _ => next,
    }
}

type CommandFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

async fn send_command(client: &AgentClient, command: OverlayCommand) -> bool {
    let ctx = context::current();
    match command {
        OverlayCommand::Hover { session_id, slot } => client
            .action_ring_hover(ctx, session_id, slot)
            .await
            .is_ok(),
        OverlayCommand::Activate { session_id, slot } => client
            .action_ring_activate(ctx, session_id, slot)
            .await
            .is_ok(),
        OverlayCommand::Cancel { session_id } => {
            client.action_ring_cancel(ctx, session_id).await.is_ok()
        }
    }
}

async fn send_commands(rx: &mut mpsc::UnboundedReceiver<OverlayCommand>) {
    send_commands_with(
        rx,
        || Box::pin(connect()),
        |client, command| Box::pin(send_command(client, command)),
    )
    .await;
}

async fn send_commands_with<C>(
    rx: &mut mpsc::UnboundedReceiver<OverlayCommand>,
    mut connect_client: impl FnMut() -> CommandFuture<'static, Option<C>>,
    mut send: impl for<'a> FnMut(&'a C, OverlayCommand) -> CommandFuture<'a, bool>,
) {
    let mut client = None;
    while let Some(mut command) = rx.recv().await {
        while let Ok(next) = rx.try_recv() {
            command = coalesce_command(command, next);
        }
        let mut deadline = command_deadline(command);
        loop {
            while let Ok(next) = rx.try_recv() {
                (command, deadline) = merge_pending(command, deadline, next);
            }
            if client.is_none() {
                match await_command_attempt(rx, command, deadline, connect_client()).await {
                    CommandAttempt::Completed(connected) => client = connected,
                    CommandAttempt::Superseded(next, next_deadline) => {
                        command = next;
                        deadline = next_deadline;
                        continue;
                    }
                    CommandAttempt::Expired => break,
                    CommandAttempt::Closed => return,
                }
            }
            let Some(active) = client.as_ref() else {
                let Some((next, next_deadline)) = wait_for_retry(rx, command, deadline).await
                else {
                    break;
                };
                command = next;
                deadline = next_deadline;
                continue;
            };
            match await_command_attempt(rx, command, deadline, send(active, command)).await {
                CommandAttempt::Completed(false) => client = None,
                CommandAttempt::Superseded(next, next_deadline) => {
                    command = next;
                    deadline = next_deadline;
                    continue;
                }
                CommandAttempt::Completed(true) | CommandAttempt::Expired => break,
                CommandAttempt::Closed => return,
            }
            let Some((next, next_deadline)) = wait_for_retry(rx, command, deadline).await else {
                break;
            };
            command = next;
            deadline = next_deadline;
        }
    }
}

#[derive(Debug)]
enum CommandAttempt<T> {
    Completed(T),
    Superseded(OverlayCommand, Option<Instant>),
    Expired,
    Closed,
}

async fn await_command_attempt<T>(
    rx: &mut mpsc::UnboundedReceiver<OverlayCommand>,
    command: OverlayCommand,
    deadline: Option<Instant>,
    attempt: impl Future<Output = T>,
) -> CommandAttempt<T> {
    tokio::pin!(attempt);
    loop {
        tokio::select! {
            result = &mut attempt => return CommandAttempt::Completed(result),
            next = rx.recv() => {
                let Some(next) = next else {
                    return CommandAttempt::Closed;
                };
                let mut pending = merge_pending(command, deadline, next);
                while let Ok(next) = rx.try_recv() {
                    pending = merge_pending(pending.0, pending.1, next);
                }
                if pending.0 != command {
                    return CommandAttempt::Superseded(pending.0, pending.1);
                }
            }
            () = deadline_elapsed(deadline) => return CommandAttempt::Expired,
        }
    }
}

async fn deadline_elapsed(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline.into()).await;
    } else {
        std::future::pending::<()>().await;
    }
}

fn command_deadline(command: OverlayCommand) -> Option<Instant> {
    command
        .is_terminal()
        .then(|| Instant::now() + DISPLAY_LIFETIME)
}

fn merge_pending(
    command: OverlayCommand,
    deadline: Option<Instant>,
    next: OverlayCommand,
) -> (OverlayCommand, Option<Instant>) {
    let pending = coalesce_command(command, next);
    let deadline = if pending == command {
        deadline
    } else {
        command_deadline(pending)
    };
    (pending, deadline)
}

async fn wait_for_retry(
    rx: &mut mpsc::UnboundedReceiver<OverlayCommand>,
    command: OverlayCommand,
    deadline: Option<Instant>,
) -> Option<(OverlayCommand, Option<Instant>)> {
    if !retry_before(deadline) {
        return None;
    }
    tokio::select! {
        () = tokio::time::sleep(Duration::from_millis(100)) => Some((command, deadline)),
        next = rx.recv() => {
            let mut pending = merge_pending(command, deadline, next?);
            while let Ok(next) = rx.try_recv() {
                pending = merge_pending(pending.0, pending.1, next);
            }
            Some(pending)
        }
    }
}

fn retry_before(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() < deadline)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "panic helpers are idiomatic in tests"
)]
mod tests {
    use super::*;

    #[test]
    fn overlay_origin_is_clamped_to_the_display() {
        let display = Bounds::new(point(px(100.0), px(50.0)), Size::new(px(800.0), px(600.0)));
        let size = Size::new(px(400.0), px(400.0));
        assert_eq!(
            clamp_window_origin(point(px(-50.0), px(-50.0)), size, display),
            point(px(100.0), px(50.0))
        );
        assert_eq!(
            clamp_window_origin(point(px(700.0), px(500.0)), size, display),
            point(px(500.0), px(250.0))
        );
    }

    #[test]
    fn activation_takes_priority_over_queued_hover_updates() {
        let hover = OverlayCommand::Hover {
            session_id: 1,
            slot: ActionRingSlot::Top,
        };
        let activation = OverlayCommand::Activate {
            session_id: 1,
            slot: ActionRingSlot::Right,
        };
        assert!(matches!(
            coalesce_command(hover, activation),
            OverlayCommand::Activate {
                slot: ActionRingSlot::Right,
                ..
            }
        ));
        assert!(matches!(
            coalesce_command(activation, hover),
            OverlayCommand::Activate { .. }
        ));
    }

    #[tokio::test]
    async fn newer_activation_supersedes_a_stale_retry_immediately() {
        let stale = OverlayCommand::Cancel { session_id: 1 };
        let replacement = OverlayCommand::Activate {
            session_id: 2,
            slot: ActionRingSlot::Right,
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send(replacement).unwrap();

        let (pending, _) = tokio::time::timeout(
            Duration::from_millis(20),
            wait_for_retry(&mut rx, stale, Some(Instant::now() + DISPLAY_LIFETIME)),
        )
        .await
        .expect("queued replacement should interrupt the retry delay")
        .expect("replacement command should remain pending");

        assert_eq!(pending, replacement);
    }

    #[tokio::test]
    async fn newer_activation_supersedes_a_stalled_terminal_request() {
        let stale = OverlayCommand::Cancel { session_id: 1 };
        let replacement = OverlayCommand::Activate {
            session_id: 2,
            slot: ActionRingSlot::Right,
        };
        let stale_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let replacement_sent = std::sync::Arc::new(tokio::sync::Notify::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn({
            let stale_started = std::sync::Arc::clone(&stale_started);
            let replacement_sent = std::sync::Arc::clone(&replacement_sent);
            async move {
                send_commands_with(
                    &mut rx,
                    || Box::pin(async { Some(()) }),
                    move |(), command| {
                        let stale_started = std::sync::Arc::clone(&stale_started);
                        let replacement_sent = std::sync::Arc::clone(&replacement_sent);
                        Box::pin(async move {
                            if command == stale {
                                stale_started.notify_one();
                                std::future::pending().await
                            } else {
                                replacement_sent.notify_one();
                                true
                            }
                        })
                    },
                )
                .await;
            }
        });

        tx.send(stale).unwrap();
        tokio::time::timeout(Duration::from_millis(100), stale_started.notified())
            .await
            .expect("stale request should start");
        tx.send(replacement).unwrap();
        tokio::time::timeout(Duration::from_millis(100), replacement_sent.notified())
            .await
            .expect("replacement should cancel the stalled request");
        drop(tx);
        tokio::time::timeout(Duration::from_millis(100), worker)
            .await
            .expect("command worker should stop")
            .expect("command worker should not panic");
    }

    #[tokio::test]
    async fn stalled_hover_stops_when_the_command_channel_closes() {
        let hover = OverlayCommand::Hover {
            session_id: 1,
            slot: ActionRingSlot::Top,
        };
        let request_started = std::sync::Arc::new(tokio::sync::Notify::new());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn({
            let request_started = std::sync::Arc::clone(&request_started);
            async move {
                send_commands_with(
                    &mut rx,
                    || Box::pin(async { Some(()) }),
                    move |(), _| {
                        let request_started = std::sync::Arc::clone(&request_started);
                        Box::pin(async move {
                            request_started.notify_one();
                            std::future::pending().await
                        })
                    },
                )
                .await;
            }
        });

        tx.send(hover).unwrap();
        tokio::time::timeout(Duration::from_millis(100), request_started.notified())
            .await
            .expect("hover request should start");
        drop(tx);
        tokio::time::timeout(Duration::from_millis(100), worker)
            .await
            .expect("closing the channel should stop the command worker")
            .expect("command worker should not panic");
    }

    #[test]
    fn only_terminal_commands_are_retryable() {
        let hover = OverlayCommand::Hover {
            session_id: 1,
            slot: ActionRingSlot::Top,
        };
        let activation = OverlayCommand::Activate {
            session_id: 1,
            slot: ActionRingSlot::Top,
        };
        let cancellation = OverlayCommand::Cancel { session_id: 1 };
        assert!(!hover.is_terminal());
        assert!(activation.is_terminal());
        assert!(cancellation.is_terminal());
    }

    #[test]
    fn terminal_retries_last_only_until_the_session_deadline() {
        assert!(retry_before(Some(Instant::now() + Duration::from_secs(1))));
        let past = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        assert!(!retry_before(Some(past)));
        assert!(!retry_before(None));
    }

    #[test]
    fn overlay_origin_stays_cursor_centered_away_from_edges() {
        let display = Bounds::new(Point::default(), Size::new(px(1600.0), px(1000.0)));
        let desired = point(px(600.0), px(300.0));
        assert_eq!(
            clamp_window_origin(desired, Size::new(px(400.0), px(400.0)), display),
            desired
        );
    }

    #[test]
    fn no_click_is_observed_when_no_ring_is_showing() {
        let live = ClickAwaySession::new();
        assert_eq!(live.observe(), None);
        live.set(11);
        live.clear();
        assert_eq!(live.observe(), None);
    }

    #[test]
    fn a_click_queued_before_a_new_ring_does_not_target_it() {
        let live = ClickAwaySession::new();
        live.set(11);
        let queued = live.observe().expect("a showing ring is observable");
        live.set(12);
        assert!(
            !click_away_targets(queued, live.observe().expect("replacement is showing")),
            "a click snapshotted against the previous session must not close the new ring"
        );
    }

    #[test]
    fn a_click_against_the_showing_ring_targets_it() {
        let live = ClickAwaySession::new();
        live.set(7);
        let queued = live.observe().expect("a showing ring is observable");
        assert!(click_away_targets(queued, 7));
    }
}
