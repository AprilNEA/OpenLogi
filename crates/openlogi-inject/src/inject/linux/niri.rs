//! Native compositor actions over niri's JSON IPC socket. No keymap or shell.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};

// The small protocol subset OpenLogi uses. Keep the wire spelling from
// https://github.com/niri-wm/niri/wiki/IPC; no niri implementation is linked.
#[derive(Debug, Serialize, Deserialize)]
enum Request {
    Action(Action),
}

#[derive(Debug, Serialize, Deserialize)]
enum Action {
    ToggleOverview {},
    FocusWorkspaceUp {},
    FocusWorkspaceDown {},
    FocusColumnLeft {},
    FocusColumnRight {},
    ScreenshotScreen {
        write_to_disk: bool,
        show_pointer: bool,
        path: Option<String>,
    },
    Screenshot {
        show_pointer: bool,
        path: Option<String>,
    },
}

#[derive(Deserialize)]
enum Response {
    Handled,
}

type Reply = Result<Response, String>;
use openlogi_core::binding::NativeAction;

const TIMEOUT: Duration = Duration::from_millis(500);
const MAX_REPLY_BYTES: u64 = 64 * 1024;

/// Return true once niri owns this action, including on IPC failure: replaying
/// a guessed desktop shortcut after an uncertain reply could act twice.
pub(super) fn dispatch(native: NativeAction) -> bool {
    let Some(path) = std::env::var_os("NIRI_SOCKET").filter(|path| !path.is_empty()) else {
        return false;
    };
    let Some(action) = action(native) else {
        return false;
    };
    let result = UnixStream::connect(path).and_then(|stream| send_action(stream, action));
    if let Err(error) = result {
        tracing::warn!(?native, %error, "niri action failed");
    }
    true
}

fn action(native: NativeAction) -> Option<Action> {
    Some(match native {
        NativeAction::MissionControl => Action::ToggleOverview {},
        NativeAction::PreviousDesktop => Action::FocusWorkspaceUp {},
        NativeAction::NextDesktop => Action::FocusWorkspaceDown {},
        NativeAction::NiriFocusColumnLeft => Action::FocusColumnLeft {},
        NativeAction::NiriFocusColumnRight => Action::FocusColumnRight {},
        NativeAction::Screenshot => Action::ScreenshotScreen {
            write_to_disk: true,
            show_pointer: true,
            path: None,
        },
        NativeAction::CaptureRegion => Action::Screenshot {
            show_pointer: true,
            path: None,
        },
        NativeAction::AppExpose
        | NativeAction::ShowDesktop
        | NativeAction::LaunchpadShow
        | NativeAction::LockScreen
        | NativeAction::Sleep => return None,
    })
}

fn send_action(mut stream: UnixStream, action: Action) -> io::Result<()> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let mut request = serde_json::to_vec(&Request::Action(action))?;
    request.push(b'\n');
    stream.write_all(&request)?;
    let mut reply = Vec::new();
    BufReader::new(stream)
        .take(MAX_REPLY_BYTES)
        .read_until(b'\n', &mut reply)?;
    if reply.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete niri reply",
        ));
    }
    match serde_json::from_slice::<Reply>(&reply)? {
        Ok(Response::Handled) => Ok(()),
        Err(message) => Err(io::Error::other(message)),
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    fn exchange(native: NativeAction, reply: &'static [u8]) -> (io::Result<()>, Request) {
        let (client, server) = UnixStream::pair().unwrap();
        let peer = thread::spawn(move || {
            let mut reader = BufReader::new(server);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            reader.get_mut().write_all(reply).unwrap();
            serde_json::from_str::<Request>(&request).unwrap()
        });
        let result = send_action(client, action(native).unwrap());
        (result, peer.join().unwrap())
    }

    #[test]
    fn overview_and_navigation_send_native_requests_not_shortcuts() {
        for (native, expected) in [
            (
                NativeAction::MissionControl,
                r#"{"Action":{"ToggleOverview":{}}}"#,
            ),
            (
                NativeAction::PreviousDesktop,
                r#"{"Action":{"FocusWorkspaceUp":{}}}"#,
            ),
            (
                NativeAction::NextDesktop,
                r#"{"Action":{"FocusWorkspaceDown":{}}}"#,
            ),
            (
                NativeAction::NiriFocusColumnLeft,
                r#"{"Action":{"FocusColumnLeft":{}}}"#,
            ),
            (
                NativeAction::NiriFocusColumnRight,
                r#"{"Action":{"FocusColumnRight":{}}}"#,
            ),
        ] {
            let (result, request) = exchange(native, b"{\"Ok\":\"Handled\"}\n");
            result.unwrap();
            assert_eq!(serde_json::to_string(&request).unwrap(), expected);
        }
    }

    #[test]
    fn screenshot_and_region_have_distinct_native_semantics() {
        assert!(matches!(
            action(NativeAction::Screenshot),
            Some(Action::ScreenshotScreen { .. })
        ));
        assert!(matches!(
            action(NativeAction::CaptureRegion),
            Some(Action::Screenshot { .. })
        ));
        for native in [
            NativeAction::AppExpose,
            NativeAction::ShowDesktop,
            NativeAction::LaunchpadShow,
            NativeAction::LockScreen,
            NativeAction::Sleep,
        ] {
            assert!(
                action(native).is_none(),
                "do not replace an unsupported scope with overview"
            );
        }
    }

    #[test]
    fn rejected_and_malformed_replies_are_not_success() {
        for reply in [
            b"{\"Err\":\"unsupported action\"}\n".as_slice(),
            b"{\"Ok\":{\"Version\":\"26.04\"}}\n",
            b"broken\n",
            b"{\"Ok\":\"Handled\"}",
        ] {
            exchange(NativeAction::MissionControl, reply).0.unwrap_err();
        }
    }

    #[test]
    fn silent_peer_cannot_block_the_action_worker_indefinitely() {
        let (client, _server) = UnixStream::pair().unwrap();
        let error = send_action(client, Action::ToggleOverview {}).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));
    }
}
