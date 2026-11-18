//! Exercises the production GJS and zbus endpoints on a disposable session bus.

use std::{
    process::{Child, Command},
    time::Duration,
};

use super::{GEOMETRY_REFRESH, PointerLoop, PointerThread, entry, ms, proto, right_edge};
use openlogi_hook::{DesktopPoint, PointerProbe};

struct Shell(Child);

impl Drop for Shell {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn call(method: &str, args: &[&str]) -> String {
    let result = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.openlogi.Frontmost",
            "--object-path",
            "/org/openlogi/Test",
            "--method",
            &format!("org.openlogi.Test.{method}"),
            "--",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}

fn read_count() -> u32 {
    let stats = call("Stats", &[]);
    stats
        .trim_start_matches("(uint32 ")
        .split(',')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
#[ignore = "requires GJS, gdbus and dbus-run-session"]
fn gnome_wayland_round_trip() {
    if std::env::var_os("OPENLOGI_TEST_GNOME_PRIVATE_BUS").is_none() {
        let status = Command::new("dbus-run-session")
            .arg("--")
            .arg(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "flow::pointer::tests::gnome::gnome_wayland_round_trip",
                "--nocapture",
            ])
            .env("OPENLOGI_TEST_GNOME_PRIVATE_BUS", "1")
            .env("GIO_USE_VFS", "local")
            .env("XDG_SESSION_TYPE", "wayland")
            .env("XDG_CURRENT_DESKTOP", "GNOME")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    check_private_bus();
}

fn check_private_bus() {
    let script = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../openlogi-hook/gnome-shell-extension/tests/flow-fixture.js"
    );
    let _shell = Shell(Command::new("gjs").args(["-m", script]).spawn().unwrap());
    assert!(
        Command::new("gdbus")
            .args([
                "wait",
                "--session",
                "--timeout",
                "5",
                "org.openlogi.Frontmost"
            ])
            .status()
            .unwrap()
            .success()
    );
    let mut state = PointerLoop::new(
        PointerProbe::connect().expect("GNOME Wayland needs a compositor pointer probe"),
        Duration::ZERO,
    );
    assert!(
        state.edges.segments().is_empty(),
        "agent starts before extension"
    );
    call("Mode", &["enable"]);
    state.refresh(GEOMETRY_REFRESH);
    assert!(!state.edges.segments().is_empty());
    assert_other_caller_denied();
    state.configure(right_edge(true));

    call("Move", &["-1", "300", "true"]);
    assert!(state.sample(ms(0)).is_none());
    assert!(state.sample(ms(300)).is_none(), "internal monitor seam");
    call("Move", &["1919", "300", "false"]);
    assert!(state.sample(ms(310)).is_none(), "Control released");
    call("Move", &["1900", "300", "true"]);
    assert!(state.sample(ms(320)).is_none());
    call("Move", &["1919", "300", "true"]);
    assert!(state.sample(ms(330)).is_some(), "fast deliberate crossing");

    call("Mode", &["lock"]);
    assert!(state.sample(ms(400)).is_none());
    state.refresh(GEOMETRY_REFRESH * 2);
    assert!(state.edges.segments().is_empty());
    call("Mode", &["unlock"]);
    state.refresh(GEOMETRY_REFRESH * 3);
    assert!(state.sample(ms(800)).is_none());
    call("Mode", &["disable"]);
    call("Mode", &["enable"]);
    state.refresh(GEOMETRY_REFRESH * 4);
    assert!(
        state.sample(ms(1100)).is_none(),
        "old dwell died with extension"
    );
    assert!(state.sample(ms(1400)).is_some(), "new generation can dwell");

    call("Move", &["-1280", "360", "false"]);
    state.place(&entry(proto::Side::Left, 1.0 / 3.0));
    assert_eq!(
        state.pointer.state().unwrap().position,
        DesktopPoint {
            x: -1278.0,
            y: 360.0
        }
    );
    // Arrival motion and a later dwell sample cannot send the mouse back.
    state.configure(super::PointerSettings {
        sides: super::ArmedSides::from_sides([super::EdgeSide::Left]),
        require_modifier: false,
    });
    assert!(state.sample(ms(1500)).is_none());
    assert!(state.sample(ms(1800)).is_none());
    call("Move", &["-1280", "360", "false"]);
    for mode in ["confined", "interrupt"] {
        call("Mode", &[mode]);
        state.refresh(state.next_refresh());
        assert!(
            state
                .pointer
                .warp(DesktopPoint {
                    x: -1278.0,
                    y: 360.0
                })
                .is_err(),
            "a pointer two pixels away must not confirm"
        );
        assert!(
            call("Stats", &[]).trim().ends_with(", 0, false)"),
            "tracker and listener released"
        );
    }
    drop(state);

    measure_motion();
}

fn assert_other_caller_denied() {
    let denied = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.openlogi.Frontmost",
            "--object-path",
            "/org/openlogi/FlowPointer",
            "--method",
            "org.openlogi.FlowPointer.GetLayout",
        ])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&denied.stderr).contains("AccessDenied"));
}

fn measure_motion() {
    // Run a real worker and count the pointer reads caused by a burst of hook notifications.
    let (crossings, _) = tokio::sync::mpsc::channel(1);
    let thread = PointerThread::spawn(crossings).unwrap();
    thread.configure(right_edge(false));
    let waker = thread.waker();
    let start = std::time::Instant::now();
    let baseline = read_count();
    while read_count() == baseline {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "worker did not acquire its layout"
        );
        waker.moved();
    }
    let before = read_count();
    for _ in 0..2000 {
        waker.moved();
    }
    let reads = read_count() - before;
    eprintln!("2000 hook notifications produced {reads} pointer reads");
    assert!(reads < 2000, "motion must coalesce");

    let before = read_count();
    let start = std::time::Instant::now();
    for _ in 0..250 {
        waker.moved();
        std::thread::sleep(Duration::from_millis(4));
    }
    let elapsed = start.elapsed();
    let reads = read_count() - before;
    eprintln!("250 paced motion wakes in {elapsed:?} produced {reads} pointer reads");
    assert!(reads > 0);
    thread.stop();
}
