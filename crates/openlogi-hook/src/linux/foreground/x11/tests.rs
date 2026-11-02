use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use x11rb::protocol::xproto::{CreateWindowAux, PropMode, WindowClass};
use x11rb::wrapper::ConnectionExt as _;

use super::*;
use crate::linux::foreground::stop_pair;
use crate::linux::xvfb::XServer;

#[test]
fn property_changes_follow_focus_and_the_focused_class_only() {
    const ROOT: Window = 0x100;
    const NET_ACTIVE_WINDOW: Atom = 0x300;
    const FOCUSED: Window = 0x0040_0001;
    const UNFOCUSED: Window = 0x0050_0001;
    let wm_class = Atom::from(AtomEnum::WM_CLASS);
    let wm_name = Atom::from(AtomEnum::WM_NAME);
    let change = |window, atom, focused| {
        let event = PropertyNotifyEvent {
            window,
            atom,
            ..PropertyNotifyEvent::default()
        };
        PropertyChange::of(&event, ROOT, NET_ACTIVE_WINDOW, focused)
    };

    assert_eq!(
        change(ROOT, NET_ACTIVE_WINDOW, Some(FOCUSED)),
        PropertyChange::Focus
    );
    assert_eq!(change(ROOT, NET_ACTIVE_WINDOW, None), PropertyChange::Focus);
    // #2078: the focused window rewrites its class in place.
    assert_eq!(
        change(FOCUSED, wm_class, Some(FOCUSED)),
        PropertyChange::Class
    );
    // Title churn on the focused window is not an identity change.
    assert_eq!(
        change(FOCUSED, wm_name, Some(FOCUSED)),
        PropertyChange::Unrelated
    );
    // A window that lost focus can still deliver an already queued rewrite.
    assert_eq!(
        change(UNFOCUSED, wm_class, Some(FOCUSED)),
        PropertyChange::Unrelated
    );
    assert_eq!(change(FOCUSED, wm_class, None), PropertyChange::Unrelated);
    // The atom only means a focus change on the root.
    assert_eq!(
        change(FOCUSED, NET_ACTIVE_WINDOW, Some(FOCUSED)),
        PropertyChange::Unrelated
    );
    assert_eq!(
        change(ROOT, wm_class, Some(FOCUSED)),
        PropertyChange::Unrelated
    );
}

/// Time for a wrongly selected change to publish on its own, before the next
/// step could coalesce it into the same drain.
const SETTLE: Duration = Duration::from_millis(150);

fn create_window(conn: &RustConnection, root: Window) -> Window {
    let window = conn.generate_id().unwrap();
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        100,
        100,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new(),
    )
    .unwrap()
    .check()
    .unwrap();
    window
}

fn set_class(conn: &RustConnection, window: Window, class: &[u8]) {
    conn.change_property8(
        PropMode::REPLACE,
        window,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        class,
    )
    .unwrap()
    .check()
    .unwrap();
}

/// Play the window manager: name `window` in the root's `_NET_ACTIVE_WINDOW`.
fn focus(conn: &RustConnection, root: Window, net_active_window: Atom, window: Window) {
    conn.change_property32(
        PropMode::REPLACE,
        root,
        net_active_window,
        AtomEnum::WINDOW,
        &[window],
    )
    .unwrap()
    .check()
    .unwrap();
}

#[test]
#[ignore = "requires Xvfb; starts its own isolated X server, never uses DISPLAY"]
fn x11_observer_republishes_a_class_rewritten_on_the_focused_window() {
    let server = XServer::start();
    let (client, root) = server.connect();
    let (observer, observer_root) = server.connect();
    let source = X11Source::on_connection(observer, observer_root).unwrap();
    let active = source.net_active_window;

    let calc = create_window(&client, root);
    let dolphin = create_window(&client, root);
    set_class(&client, calc, b"soffice\0Soffice\0");
    set_class(&client, dolphin, b"dolphin\0dolphin\0");
    focus(&client, root, active, calc);

    let (stop, stop_token) = stop_pair().unwrap();
    let (tx, rx) = mpsc::channel();
    let publish: PublishAppId = Arc::new(move |app| tx.send(app).unwrap());
    let worker = thread::spawn(move || Box::new(source).observe(stop_token, publish));
    let next = || {
        rx.recv_timeout(Duration::from_secs(2))
            .expect("the observer publishes the change")
    };

    assert_eq!(next().as_deref(), Some("Soffice"));
    // LibreOffice loads the Calc module into the frame that already has focus.
    set_class(&client, calc, b"libreoffice-calc\0libreoffice-calc\0");
    assert_eq!(next().as_deref(), Some("libreoffice-calc"));

    // A title change keeps the identity: the next publication is the focus move.
    client
        .change_property8(
            PropMode::REPLACE,
            calc,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            b"Untitled 1 - LibreOffice Calc",
        )
        .unwrap()
        .check()
        .unwrap();
    thread::sleep(SETTLE);
    focus(&client, root, active, dolphin);
    assert_eq!(next().as_deref(), Some("dolphin"));

    // The watch moved with focus: the newly focused window's rewrite publishes
    // without another focus change, and the old window's rewrite publishes
    // nothing.
    set_class(&client, dolphin, b"dolphin\0org.kde.dolphin\0");
    assert_eq!(next().as_deref(), Some("org.kde.dolphin"));
    set_class(&client, calc, b"soffice\0Soffice\0");
    thread::sleep(SETTLE);
    focus(&client, root, active, x11rb::NONE);
    assert_eq!(next(), None);
    focus(&client, root, active, calc);
    assert_eq!(next().as_deref(), Some("Soffice"));

    stop.request();
    drop(worker.join().unwrap());
    assert_eq!(rx.try_recv(), Err(mpsc::TryRecvError::Disconnected));
}
