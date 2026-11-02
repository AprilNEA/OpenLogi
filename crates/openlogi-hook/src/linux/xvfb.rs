//! A private Xvfb display for the X11 tests. Nothing here reads `DISPLAY`, so a
//! test can never touch the developer's own session.

use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::Window;
use x11rb::rust_connection::RustConnection;

/// One isolated Xvfb server, killed and reaped on drop.
pub(super) struct XServer {
    child: Child,
    display: String,
}

impl XServer {
    pub(super) fn start() -> Self {
        // Own the child before reading its display number, so a failed read
        // still stops the server.
        let mut server = Self {
            child: Command::new("Xvfb")
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
            display: String::new(),
        };
        let mut number = String::new();
        BufReader::new(server.child.stdout.take().unwrap())
            .read_line(&mut number)
            .unwrap();
        server.display = format!(":{}", number.trim());
        server
    }

    /// Open a new client connection, returned with its screen's root window.
    pub(super) fn connect(&self) -> (RustConnection, Window) {
        let (conn, screen) = RustConnection::connect(Some(&self.display)).unwrap();
        let root = conn.setup().roots[screen].root;
        (conn, root)
    }
}

impl Drop for XServer {
    fn drop(&mut self) {
        self.child.kill().expect("stop private Xvfb server");
        self.child.wait().expect("reap private Xvfb server");
    }
}
