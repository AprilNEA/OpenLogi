use std::sync::{Arc, mpsc};
use std::thread;

use super::*;
use crate::linux::foreground::stop::stop_pair;

const INITIAL: &[u8] = b"{\"WindowsChanged\":{\"windows\":[{\"id\":1,\"app_id\":\"org.example.One\",\"is_focused\":true},{\"id\":2,\"app_id\":\"org.example.Two\",\"is_focused\":false}]}}\n";

#[test]
fn snapshot_focus_identity_changes_and_close_follow_niri_events() {
    let mut windows = Windows::default();
    for (event, expected) in [
        (INITIAL, Some("org.example.One")),
        (b"{\"WindowFocusChanged\":{\"id\":2}}", Some("org.example.Two")),
        (b"{\"WindowOpenedOrChanged\":{\"window\":{\"id\":2,\"app_id\":\"org.example.Renamed\",\"is_focused\":true}}}", Some("org.example.Renamed")),
        (b"{\"FutureUnrelatedEvent\":{\"data\":123}}", Some("org.example.Renamed")),
        (b"{\"WindowFocusChanged\":{\"id\":null}}", None),
        (b"{\"WindowOpenedOrChanged\":{\"window\":{\"id\":3,\"app_id\":null,\"is_focused\":true}}}", None),
        (b"{\"WindowFocusChanged\":{\"id\":1}}", Some("org.example.One")),
        (b"{\"WindowClosed\":{\"id\":1}}", None),
        (b"{\"WindowsChanged\":{\"windows\":[]}}", None),
    ] {
        windows.apply(parse_event(event).unwrap());
        assert_eq!(windows.app_id().as_deref(), expected);
    }
}

#[test]
fn stream_drains_buffered_events_and_stop_interrupts_a_partial_frame() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let (control, stop) = stop_pair().unwrap();
    let (tx, rx) = mpsc::channel();
    let publish: PublishAppId = Arc::new(move |id| tx.send(id).unwrap());
    let observer = thread::spawn(move || Session::new(client).unwrap().observe(&stop, &publish));
    let mut request = String::new();
    BufReader::new(&server).read_line(&mut request).unwrap();
    assert_eq!(request, "\"EventStream\"\n");
    // One write covers the reply and multiple events, so a worker that polls
    // before consuming BufReader's queued bytes will miss the second update.
    let mut burst = b"{\"Ok\":\"Handled\"}\n".to_vec();
    burst.extend_from_slice(INITIAL);
    burst.extend_from_slice(b"{\"WindowFocusChanged\":{\"id\":2}}\n{\"WindowFocus");
    server.write_all(&burst).unwrap();
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap().as_deref(),
        Some("org.example.One")
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap().as_deref(),
        Some("org.example.Two")
    );
    control.request();
    observer.join().unwrap().unwrap();
}

#[test]
fn fragmented_event_is_preserved_until_its_newline() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let (control, stop) = stop_pair().unwrap();
    let (tx, rx) = mpsc::channel();
    let publish: PublishAppId = Arc::new(move |id| tx.send(id).unwrap());
    let observer = thread::spawn(move || Session::new(client).unwrap().observe(&stop, &publish));
    let mut request = String::new();
    BufReader::new(&server).read_line(&mut request).unwrap();
    server.write_all(b"{\"Ok\":\"Handled\"}\n").unwrap();
    server.write_all(INITIAL).unwrap();
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
    server.write_all(b"{\"WindowFocusChanged\":{").unwrap();
    server.write_all(b"\"id\":2}}\n").unwrap();
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap().as_deref(),
        Some("org.example.Two")
    );
    control.request();
    observer.join().unwrap().unwrap();
}

#[test]
fn rejected_subscription_and_eof_end_the_stream() {
    for reply in [
        b"{\"Err\":\"unavailable\"}\n".as_slice(),
        b"{\"Ok\":\"Handled\"}\n",
    ] {
        let (client, mut server) = UnixStream::pair().unwrap();
        let (_control, stop) = stop_pair().unwrap();
        let peer = thread::spawn(move || {
            let mut request = String::new();
            BufReader::new(&server).read_line(&mut request).unwrap();
            server.write_all(reply).unwrap();
        });
        let publish: PublishAppId = Arc::new(|_| {});
        Session::new(client)
            .unwrap()
            .observe(&stop, &publish)
            .unwrap_err();
        peer.join().unwrap();
    }
}

#[test]
fn malformed_known_events_fail_instead_of_retaining_stale_focus() {
    parse_event(b"{\"WindowFocusChanged\":{\"id\":\"invalid\"}}").unwrap_err();
    parse_event(b"[]").unwrap_err();
    parse_event(b"{\"WindowsChanged\":{}}").unwrap_err();
}

#[test]
fn disconnect_clears_focus_and_reconnect_replaces_the_window_snapshot() {
    use std::os::unix::net::UnixListener;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("niri.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (control, stop) = stop_pair().unwrap();
    let (tx, rx) = mpsc::channel();
    let publish: PublishAppId = Arc::new(move |id| tx.send(id).unwrap());
    let source = Box::new(NiriSource { path });
    let observer = thread::spawn(move || source.observe(stop, publish));

    for snapshot in [INITIAL, b"{\"WindowsChanged\":{\"windows\":[]}}\n"] {
        let (mut server, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&server).read_line(&mut request).unwrap();
        assert_eq!(request, "\"EventStream\"\n");
        server.write_all(b"{\"Ok\":\"Handled\"}\n").unwrap();
        server.write_all(snapshot).unwrap();
        let app = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        if snapshot == INITIAL {
            assert_eq!(app.as_deref(), Some("org.example.One"));
            drop(server);
            assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), None);
        } else {
            assert_eq!(app, None);
            control.request();
        }
    }
    let mut source = observer.join().unwrap();
    // After dropping observation, the same source can serve an idle snapshot.
    let peer = thread::spawn(move || {
        let (mut server, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&server).read_line(&mut request).unwrap();
        assert_eq!(request, "\"FocusedWindow\"\n");
        server.write_all(b"{\"Ok\":{\"FocusedWindow\":{\"id\":5,\"app_id\":\"org.example.After\",\"is_focused\":true}}}\n").unwrap();
    });
    assert_eq!(
        source.frontmost_app_id().as_deref(),
        Some("org.example.After")
    );
    peer.join().unwrap();
}
