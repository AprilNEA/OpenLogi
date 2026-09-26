//! Native X11 pointer queries. XWayland is deliberately not a Wayland backend.

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, Window};
use x11rb::rust_connection::RustConnection;

use super::foreground::{SessionKind, detect_session_kind, x11::window_app_id};
use crate::{ForegroundApp, PointerContext, PointerTarget};

pub(crate) fn pointer_context_supported() -> bool {
    detect_session_kind() == SessionKind::X11
}

fn atom(conn: &RustConnection, name: &[u8]) -> Option<Atom> {
    Some(conn.intern_atom(false, name).ok()?.reply().ok()?.atom)
}

fn property(
    conn: &RustConnection,
    window: Window,
    name: &[u8],
    kind: AtomEnum,
) -> Option<Vec<u32>> {
    let reply = conn
        .get_property(false, window, atom(conn, name)?, kind, 0, u32::MAX)
        .ok()?
        .reply()
        .ok()?;
    Some(reply.value32()?.collect())
}

fn target(conn: &RustConnection, window: Window) -> Option<PointerTarget> {
    let pid = *property(conn, window, b"_NET_WM_PID", AtomEnum::CARDINAL)?.first()?;
    let process_id = i32::try_from(pid).ok().filter(|pid| *pid > 0)?;
    Some(PointerTarget::Window {
        process_id,
        window_id: u64::from(window),
    })
}

pub(crate) fn pointer_context() -> Option<PointerContext> {
    let (conn, screen) = RustConnection::connect(None).ok()?;
    let root = conn.setup().roots[screen].root;
    context_on_connection(&conn, root)
}

fn context_on_connection(conn: &RustConnection, root: Window) -> Option<PointerContext> {
    let pointer = conn.query_pointer(root).ok()?.reply().ok()?;
    if !pointer.same_screen {
        return None;
    }
    if pointer.child == x11rb::NONE {
        return Some(PointerContext {
            app: None,
            target: PointerTarget::Desktop,
        });
    }

    // QueryPointer identifies the actual top root child, including shaped
    // windows and unmanaged popups. Never geometrically search through an
    // unknown child to an application or desktop behind it.
    let clients = property(conn, root, b"_NET_CLIENT_LIST_STACKING", AtomEnum::WINDOW)?;
    for client in clients.into_iter().rev() {
        let mut ancestor = client;
        // Reparenting WMs put the client below a decoration/frame. Resolve the
        // client even when the pointer is on its titlebar, not its content.
        for _ in 0..32 {
            if ancestor == pointer.child {
                let desktop_type = atom(conn, b"_NET_WM_WINDOW_TYPE_DESKTOP")?;
                let types = property(conn, client, b"_NET_WM_WINDOW_TYPE", AtomEnum::ATOM)
                    .unwrap_or_default();
                if types.contains(&desktop_type) {
                    return Some(PointerContext {
                        app: None,
                        target: PointerTarget::Desktop,
                    });
                }
                return Some(PointerContext {
                    app: Some(ForegroundApp::unnamed(window_app_id(conn, client)?)),
                    target: target(conn, client)?,
                });
            }
            let tree = conn.query_tree(ancestor).ok()?.reply().ok()?;
            if tree.parent == root || tree.parent == x11rb::NONE {
                break;
            }
            ancestor = tree.parent;
        }
    }
    None
}

pub(crate) fn pointer_target_is_focused(expected: PointerTarget) -> bool {
    let Some((conn, screen)) = RustConnection::connect(None).ok() else {
        return false;
    };
    focused_target(&conn, conn.setup().roots[screen].root) == Some(expected)
}

fn focused_target(conn: &RustConnection, root: Window) -> Option<PointerTarget> {
    let window = *property(conn, root, b"_NET_ACTIVE_WINDOW", AtomEnum::WINDOW)?.first()?;
    if window == x11rb::NONE {
        return None;
    }
    target(conn, window)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader};
    use std::process::{Child, Command, Stdio};

    use x11rb::protocol::xproto::{CreateWindowAux, PropMode, WindowClass};
    use x11rb::wrapper::ConnectionExt as _;

    use super::*;

    struct XServer(Child);

    impl XServer {
        fn start() -> (Self, RustConnection, Window) {
            let mut server = Self(
                Command::new("Xvfb")
                    .args([
                        "-displayfd",
                        "1",
                        "-screen",
                        "0",
                        "640x480x24",
                        "-nolisten",
                        "tcp",
                    ])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("start Xvfb"),
            );
            let mut display = String::new();
            BufReader::new(server.0.stdout.take().unwrap())
                .read_line(&mut display)
                .unwrap();
            let (conn, screen) =
                RustConnection::connect(Some(&format!(":{}", display.trim()))).unwrap();
            let root = conn.setup().roots[screen].root;
            (server, conn, root)
        }
    }

    impl Drop for XServer {
        fn drop(&mut self) {
            self.0.kill().expect("stop private Xvfb server");
            self.0.wait().expect("reap private Xvfb server");
        }
    }

    #[test]
    #[ignore = "requires Xvfb; starts its own isolated X server, never uses DISPLAY"]
    fn x11_pointer_distinguishes_client_desktop_obstacle_and_focus() {
        let (_server, conn, root) = XServer::start();
        conn.warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, 15, 17)
            .unwrap()
            .check()
            .unwrap();
        assert_eq!(
            context_on_connection(&conn, root).unwrap(),
            PointerContext {
                app: None,
                target: PointerTarget::Desktop
            }
        );
        assert_eq!(context_on_connection(&conn, x11rb::NONE), None);

        let frame = conn.generate_id().unwrap();
        let client = conn.generate_id().unwrap();
        for (id, parent, y) in [(frame, root, 5), (client, frame, 25)] {
            conn.create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                id,
                parent,
                5,
                y,
                200,
                140,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new(),
            )
            .unwrap()
            .check()
            .unwrap();
            conn.map_window(id).unwrap().check().unwrap();
        }
        // An unregistered frame is an obstacle, not desktop behind it.
        assert_eq!(context_on_connection(&conn, root), None);
        let client_list = atom(&conn, b"_NET_CLIENT_LIST_STACKING").unwrap();
        conn.change_property32(
            PropMode::REPLACE,
            root,
            client_list,
            AtomEnum::WINDOW,
            &[client],
        )
        .unwrap()
        .check()
        .unwrap();
        conn.change_property8(
            PropMode::REPLACE,
            client,
            AtomEnum::WM_CLASS,
            AtomEnum::STRING,
            b"instance\0ProfileClass\0",
        )
        .unwrap()
        .check()
        .unwrap();
        let pid = atom(&conn, b"_NET_WM_PID").unwrap();
        // Missing owner metadata still does not mean desktop.
        assert_eq!(context_on_connection(&conn, root), None);
        conn.change_property32(PropMode::REPLACE, client, pid, AtomEnum::CARDINAL, &[731])
            .unwrap()
            .check()
            .unwrap();
        let expected = PointerTarget::Window {
            process_id: 731,
            window_id: u64::from(client),
        };
        // Pointer is on the frame titlebar, outside the client content.
        assert_eq!(
            context_on_connection(&conn, root).unwrap(),
            PointerContext {
                app: Some(ForegroundApp::unnamed("ProfileClass".into())),
                target: expected,
            }
        );

        let window_type = atom(&conn, b"_NET_WM_WINDOW_TYPE").unwrap();
        let desktop_type = atom(&conn, b"_NET_WM_WINDOW_TYPE_DESKTOP").unwrap();
        conn.change_property32(
            PropMode::REPLACE,
            client,
            window_type,
            AtomEnum::ATOM,
            &[desktop_type],
        )
        .unwrap()
        .check()
        .unwrap();
        assert_eq!(
            context_on_connection(&conn, root).unwrap(),
            PointerContext {
                app: None,
                target: PointerTarget::Desktop
            }
        );

        verify_focus_and_popup(&conn, root, client, expected);
    }

    fn verify_focus_and_popup(
        conn: &RustConnection,
        root: Window,
        client: Window,
        expected: PointerTarget,
    ) {
        let other = conn.generate_id().unwrap();
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            other,
            root,
            300,
            200,
            80,
            60,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new(),
        )
        .unwrap()
        .check()
        .unwrap();
        let pid = atom(conn, b"_NET_WM_PID").unwrap();
        conn.change_property32(PropMode::REPLACE, other, pid, AtomEnum::CARDINAL, &[731])
            .unwrap()
            .check()
            .unwrap();
        let active = atom(conn, b"_NET_ACTIVE_WINDOW").unwrap();
        conn.change_property32(PropMode::REPLACE, root, active, AtomEnum::WINDOW, &[other])
            .unwrap()
            .check()
            .unwrap();
        assert_eq!(
            focused_target(conn, root),
            Some(PointerTarget::Window {
                process_id: 731,
                window_id: u64::from(other)
            })
        );
        assert_ne!(focused_target(conn, root), Some(expected));
        conn.change_property32(PropMode::REPLACE, root, active, AtomEnum::WINDOW, &[client])
            .unwrap()
            .check()
            .unwrap();
        assert_eq!(focused_target(conn, root), Some(expected));

        // An unmanaged popup covers the desktop client: never look through it.
        conn.configure_window(
            other,
            &x11rb::protocol::xproto::ConfigureWindowAux::new().x(0).y(0),
        )
        .unwrap()
        .check()
        .unwrap();
        conn.map_window(other).unwrap().check().unwrap();
        assert_eq!(context_on_connection(conn, root), None);
    }
}
