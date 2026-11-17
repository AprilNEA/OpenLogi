//! The host against real child processes: a module written as a shell
//! script records every line it reads and answers as its mode file says.
//! Unix only, where `/bin/sh` runs the script; the protocol, restart, and
//! discovery halves have platform-free tests of their own.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use openlogi_core::binding::{GridKey, GridSlot, KeypadPage};
use openlogi_core::config::ModuleConfig;
use openlogi_core::module::ModuleId;
use openlogi_ipc::ModuleState;
use tokio::sync::{mpsc, watch};

use super::protocol::tests::solid_png;
use super::*;
use crate::watchers::key_display::KeyImage;

/// The test module. `mode` in its folder picks how it answers:
/// `normal` answers `hello` and draws every key it is given, `exit` draws
/// and then exits with status 3, `silent` never answers, `deaf` answers and
/// then stops reading, `closer` closes its input, answers, and keeps
/// running, `one` draws `keypad-1`'s middle key, `spam` draws without end
/// once it has keys, `wrapper` starts a `sleep` of its own and never
/// answers, `helper` starts one and answers as `normal` does, and `orphan`
/// answers, then starts one that holds its output open and exits with
/// status 4. Each run notes its process id in `pids.log`, and each `sleep`
/// its own in `sleep.pids`.
const SCRIPT: &str = r#"#!/bin/sh
dir="$OPENLOGI_MODULE_DIR"
mode="$(cat "$dir/mode")"
png="$(cat "$dir/png.b64")"
printf '%s|%s\n' "$OPENLOGI_MODULE_ID" "$(pwd -P)" >> "$dir/env.log"
printf '%s\n' "$$" >> "$dir/pids.log"
if [ "$mode" = wrapper ] || [ "$mode" = helper ]; then
  sleep 300 &
  printf '%s\n' "$!" >> "$dir/sleep.pids"
fi
spammer=
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$dir/received.log"
  case "$line" in
    *'"type":"hello"'*)
      [ "$mode" = silent ] || [ "$mode" = wrapper ] && continue
      [ "$mode" = closer ] && exec 0<&-
      printf '{"type":"log","level":"info","message":"hello from the test module"}\n'
      printf '{"type":"ready","protocol":1}\n'
      [ "$mode" = deaf ] && exec sleep 30
      [ "$mode" = closer ] && exec sleep 30
      ;;
    *'"keys":[]'*) ;;
    *'"type":"keys"'*)
      case "$mode" in
        one) printf '{"type":"image","device":"keypad-1","slot":"gridMiddle","png":"%s"}\n' "$png" ;;
        orphan)
          sleep 300 &
          printf '%s\n' "$!" >> "$dir/sleep.pids"
          exit 4
          ;;
        spam)
          if [ -z "$spammer" ]; then
            while :; do printf '{"type":"image_all","png":"%s"}\n' "$png" || exit; done &
            spammer=$!
          fi
          ;;
        *) printf '{"type":"image_all","png":"%s"}\n' "$png" ;;
      esac
      [ "$mode" = exit ] && exit 3
      ;;
    *'"type":"shutdown"'*)
      [ -n "$spammer" ] && kill "$spammer"
      exit 0
      ;;
  esac
done
"#;

const ID: &str = "com.example.test";

/// The longest a test waits for the host to do something before failing.
const WAIT: Duration = Duration::from_secs(10);

fn id() -> ModuleId {
    ModuleId::try_new(ID.to_owned()).unwrap()
}

fn timings() -> ModuleTimings {
    ModuleTimings {
        ready_timeout: Duration::from_secs(5),
        shutdown_grace: Duration::from_millis(500),
        first_restart: Duration::from_millis(50),
        max_restart: Duration::from_millis(400),
        healthy_after: Duration::from_secs(60),
        rescan_every: Duration::from_millis(200),
    }
}

/// Install the test module under `modules` in `mode`, and return its folder.
fn install(modules: &Path, mode: &str) -> PathBuf {
    let folder = modules.join("test-module");
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join("module.toml"),
        format!(
            "id = \"{ID}\"\nname = \"Test\"\nprotocol = 1\nplatforms = [\"macos\", \"linux\"]\nexecutable = \"run.sh\"\n"
        ),
    )
    .unwrap();
    let script = folder.join("run.sh");
    fs::write(&script, SCRIPT).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(folder.join("mode"), mode).unwrap();
    fs::write(
        folder.join("png.b64"),
        base64::engine::general_purpose::STANDARD.encode(solid_png(4, [0, 255, 0])),
    )
    .unwrap();
    folder
}

fn key(page: u8, key: GridKey) -> ModuleKey {
    ModuleKey {
        device: "keypad".into(),
        slot: GridSlot {
            page: KeypadPage::new(page).unwrap(),
            key,
        },
        size: 118,
    }
}

fn plan(enabled: bool, settings: &[(&str, &str)], keys: Vec<ModuleKey>) -> Arc<ModulePlan> {
    let mut config = ModuleConfig {
        enabled,
        ..ModuleConfig::default()
    };
    for (name, value) in settings {
        config.settings.insert((*name).into(), (*value).into());
    }
    Arc::new(ModulePlan {
        config: [(id(), config)].into(),
        keys: if keys.is_empty() {
            BTreeMap::new()
        } else {
            [(id(), keys)].into()
        },
        turned_off: BTreeMap::new(),
    })
}

/// A plan with no keys from an agent that has seen the module turned off
/// `turned_off` times.
fn toggled_plan(enabled: bool, turned_off: u64) -> Arc<ModulePlan> {
    let mut plan = plan(enabled, &[], vec![]).as_ref().clone();
    plan.turned_off.insert(id(), turned_off);
    Arc::new(plan)
}

/// What a harness does with the host's module events.
#[derive(Clone, Copy)]
enum Events {
    /// Collect every one.
    Collected,
    /// Take none, through a channel with room for one: an agent that has
    /// stopped draining them.
    Undrained,
}

/// A running host and everything a test watches it through.
struct Harness {
    handle: ModuleHostHandle,
    plan: watch::Sender<Arc<ModulePlan>>,
    events: Arc<Mutex<Vec<ModuleEvent>>>,
    folder: PathBuf,
    _undrained: Option<mpsc::Receiver<ModuleEvent>>,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn start(mode: &str, first: Arc<ModulePlan>, timings: ModuleTimings) -> Self {
        Self::start_with(mode, first, timings, Events::Collected)
    }

    fn start_with(
        mode: &str,
        first: Arc<ModulePlan>,
        timings: ModuleTimings,
        drain: Events,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let modules = dir.path().join("modules");
        let folder = install(&modules, mode);
        let (handle, host) = ModuleHostHandle::new(Some(modules), timings);
        let (plan, plan_rx) = watch::channel(first);
        let events = Arc::new(Mutex::new(Vec::new()));
        let (events_tx, undrained) = match drain {
            Events::Collected => {
                let (events_tx, mut events_rx) = mpsc::channel(8);
                let sink = Arc::clone(&events);
                tokio::spawn(async move {
                    while let Some(event) = events_rx.recv().await {
                        sink.lock().unwrap().push(event);
                    }
                });
                (events_tx, None)
            }
            Events::Undrained => {
                let (events_tx, events_rx) = mpsc::channel(1);
                (events_tx, Some(events_rx))
            }
        };
        tokio::spawn(host.run(plan_rx, events_tx));
        Self {
            handle,
            plan,
            events,
            folder,
            _undrained: undrained,
            _dir: dir,
        }
    }

    /// The lines of `file` in the module's folder.
    fn lines(&self, file: &str) -> Vec<String> {
        fs::read_to_string(self.folder.join(file))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn received(&self) -> Vec<String> {
        self.lines("received.log")
    }

    fn hellos(&self) -> Vec<serde_json::Value> {
        self.received()
            .iter()
            .filter(|line| line.contains("\"type\":\"hello\""))
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect()
    }

    fn state(&self) -> Option<(ModuleState, Option<String>)> {
        self.handle
            .statuses()
            .into_iter()
            .next()
            .map(|status| (status.state, status.error))
    }

    fn events(&self) -> Vec<ModuleEvent> {
        self.events.lock().unwrap().clone()
    }

    async fn until(&self, what: &str, done: impl Fn(&Self) -> bool) {
        self.within(WAIT, what, done).await;
    }

    /// Wait until `done`, failing the test after `limit`.
    async fn within(&self, limit: Duration, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = tokio::time::Instant::now() + limit;
        while !done(self) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}: state {:?}, received {:#?}, events {:#?}",
                self.state(),
                self.received(),
                self.events()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Shut the host down, failing when it does not answer within [`WAIT`]:
    /// [`ModuleHostHandle::shutdown`] gives up silently at its bound.
    async fn shutdown(&self) {
        let asked = tokio::time::Instant::now();
        self.handle.shutdown(WAIT).await;
        assert!(
            asked.elapsed() < WAIT,
            "the host did not shut down within {WAIT:?}: state {:?}",
            self.state()
        );
    }

    /// Stop every module, failing when the host does not answer within
    /// [`WAIT`].
    async fn stop_all(&self) {
        let asked = tokio::time::Instant::now();
        self.handle.stop_all(WAIT).await;
        assert!(
            asked.elapsed() < WAIT,
            "the host did not stop its modules within {WAIT:?}: state {:?}",
            self.state()
        );
    }

    async fn resume(&self) {
        tokio::time::timeout(WAIT, self.handle.resume())
            .await
            .expect("the host takes the resume in time");
    }
}

fn green() -> KeyImage {
    KeyImage::decode(&solid_png(4, [0, 255, 0])).unwrap()
}

/// Whether the process `pid` is still there, as a zombie too.
fn exists(pid: &str) -> bool {
    let output = std::process::Command::new("ps")
        .args(["-p", pid, "-o", "stat="])
        .output()
        .unwrap();
    !String::from_utf8_lossy(&output.stdout).trim().is_empty()
}

#[tokio::test]
async fn a_module_hears_hello_its_keys_and_presses_and_leaves_on_shutdown() {
    let middle = key(1, GridKey::Middle);
    let host = Harness::start(
        "normal",
        plan(true, &[("city", "Lynchburg")], vec![middle.clone()]),
        timings(),
    );
    host.until("the module to run", |host| {
        host.state() == Some((ModuleState::Running, None))
    })
    .await;
    host.until("its keys", |host| host.received().len() >= 2)
        .await;

    let received = host.received();
    let hello: serde_json::Value = serde_json::from_str(&received[0]).unwrap();
    assert_eq!(hello["type"], "hello", "hello comes first: {received:?}");
    assert_eq!(hello["module_id"], ID);
    assert_eq!(hello["settings"], serde_json::json!({"city": "Lynchburg"}));
    assert_eq!(
        received[1],
        r#"{"type":"keys","keys":[{"device":"keypad-1","slot":"gridMiddle","size":118}]}"#,
        "keys only after ready"
    );
    let env = fs::read_to_string(host.folder.join("env.log")).unwrap();
    assert_eq!(
        env.trim(),
        format!("{ID}|{}", host.folder.canonicalize().unwrap().display()),
        "the module's id and folder in its environment, and its folder as its working directory"
    );
    host.until("its image", |host| !host.events().is_empty())
        .await;
    assert_eq!(
        host.events(),
        [ModuleEvent::ImageAll {
            module: id(),
            image: green(),
        }]
    );

    host.handle.press(ModulePress {
        module: id(),
        device: "keypad".into(),
        slot: middle.slot,
    });
    host.until("the press", |host| host.received().len() >= 3)
        .await;
    assert_eq!(
        host.received()[2],
        r#"{"type":"pressed","device":"keypad-1","slot":"gridMiddle"}"#
    );

    // A page turn: the module owns a key on page 2 now.
    let top_left = key(2, GridKey::TopLeft);
    host.plan
        .send_replace(plan(true, &[("city", "Lynchburg")], vec![top_left]));
    host.until("the new keys", |host| host.received().len() >= 4)
        .await;
    assert_eq!(
        host.received()[3],
        r#"{"type":"keys","keys":[{"device":"keypad-1","slot":"page2:gridTopLeft","size":118}]}"#
    );
    assert_eq!(host.hellos().len(), 1, "new keys do not restart the module");

    host.shutdown().await;
    assert_eq!(host.received().last().unwrap(), r#"{"type":"shutdown"}"#);
    assert!(host.handle.statuses().is_empty());
}

/// A changed `settings` table restarts the module with a fresh `hello`;
/// the same table again does not.
#[tokio::test]
async fn changed_settings_restart_the_module_with_a_fresh_hello() {
    let keys = vec![key(1, GridKey::Middle)];
    let host = Harness::start(
        "normal",
        plan(true, &[("city", "Lynchburg")], keys.clone()),
        timings(),
    );
    host.until("the first run", |host| {
        host.state() == Some((ModuleState::Running, None))
    })
    .await;

    host.plan
        .send_replace(plan(true, &[("city", "Lynchburg")], keys.clone()));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        host.hellos().len(),
        1,
        "an unchanged table restarts nothing"
    );

    host.plan
        .send_replace(plan(true, &[("city", "Roanoke")], keys.clone()));
    host.until("the second hello", |host| host.hellos().len() == 2)
        .await;
    let settings: Vec<_> = host
        .hellos()
        .into_iter()
        .map(|hello| hello["settings"].clone())
        .collect();
    assert_eq!(
        settings,
        [
            serde_json::json!({"city": "Lynchburg"}),
            serde_json::json!({"city": "Roanoke"}),
        ]
    );
    assert!(
        host.received()
            .iter()
            .any(|line| line == r#"{"type":"shutdown"}"#),
        "the first run was told to leave"
    );
    host.until("the restarted module", |host| {
        host.state() == Some((ModuleState::Running, None))
    })
    .await;
    host.shutdown().await;
}

/// A module that exits is started again, and the faces it drew go with it.
#[tokio::test]
async fn an_exited_module_drops_its_faces_and_restarts() {
    let host = Harness::start(
        "exit",
        plan(true, &[], vec![key(1, GridKey::Middle)]),
        timings(),
    );
    host.until("two runs", |host| {
        host.events()
            .iter()
            .filter(|event| matches!(event, ModuleEvent::ImageAll { .. }))
            .count()
            >= 2
    })
    .await;
    let events = host.events();
    assert!(
        matches!(events[0], ModuleEvent::ImageAll { .. })
            && events[1] == ModuleEvent::Stopped { module: id() }
            && matches!(events[2], ModuleEvent::ImageAll { .. }),
        "its image, then its end, then the next run's image: {events:?}"
    );
    assert_eq!(
        host.handle.statuses()[0].error.as_deref(),
        Some("exited with status 3")
    );
    host.shutdown().await;
}

/// A module that never answers `hello` is killed at the deadline and, after
/// five such runs, left failed until it is turned off and on again.
#[tokio::test]
async fn a_module_that_never_gets_ready_fails_until_toggled() {
    let timings = ModuleTimings {
        ready_timeout: Duration::from_secs(1),
        first_restart: Duration::from_millis(10),
        ..timings()
    };
    let host = Harness::start(
        "silent",
        plan(true, &[], vec![key(1, GridKey::Middle)]),
        timings,
    );
    host.until("five failed starts", |host| {
        matches!(host.state(), Some((ModuleState::Failed, _)))
    })
    .await;
    let (_, error) = host.state().unwrap();
    assert!(
        error.as_deref().unwrap().contains("did not answer hello"),
        "{error:?}"
    );
    assert_eq!(
        host.received().len(),
        5,
        "five hellos, and no keys for a module that is not ready"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        host.received().len(),
        5,
        "a failed module is not started again on its own"
    );

    host.plan.send_replace(toggled_plan(false, 1));
    host.until("the module off", |host| {
        host.state() == Some((ModuleState::Disabled, None))
    })
    .await;
    host.plan.send_replace(toggled_plan(true, 1));
    host.until("a sixth start", |host| host.hellos().len() == 6)
        .await;
    host.shutdown().await;
}

/// A module turned off is never started, and one that stops reading its
/// input is killed rather than allowed to back the host up.
#[tokio::test]
async fn a_disabled_module_never_runs_and_a_deaf_one_is_killed() {
    let host = Harness::start("deaf", plan(false, &[], vec![]), timings());
    host.until("the folder scanned", |host| host.state().is_some())
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(host.state(), Some((ModuleState::Disabled, None)));
    assert!(!host.folder.join("env.log").exists(), "never started");

    host.plan
        .send_replace(plan(true, &[], vec![key(1, GridKey::Middle)]));
    host.until("the module to run", |host| {
        host.state() == Some((ModuleState::Running, None))
    })
    .await;
    let press = ModulePress {
        module: id(),
        device: "keypad".into(),
        slot: key(1, GridKey::Middle).slot,
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let error = host.handle.statuses()[0].error.clone();
        if error.as_deref() == Some("stopped reading its input") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the deaf module was never killed: {:?}",
            host.state()
        );
        for _ in 0..32 {
            host.handle.press(press.clone());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    host.shutdown().await;
}

/// A module knows each keypad only as `keypad-1`, `keypad-2`, and so on,
/// never by its config key; what it sends for one comes back under the
/// config key.
#[tokio::test]
async fn a_module_sees_keypads_by_name_never_by_config_key() {
    let mut first = key(1, GridKey::Middle);
    first.device = "unit:0f1e2d3c".into();
    let mut second = key(1, GridKey::TopLeft);
    second.device = "unit:4b5a6978".into();
    let host = Harness::start(
        "one",
        plan(true, &[], vec![first.clone(), second]),
        timings(),
    );
    host.until("its image", |host| !host.events().is_empty())
        .await;
    assert_eq!(
        host.received()[1],
        r#"{"type":"keys","keys":[{"device":"keypad-1","slot":"gridMiddle","size":118},{"device":"keypad-2","slot":"gridTopLeft","size":118}]}"#
    );
    assert_eq!(
        host.events(),
        [ModuleEvent::Image {
            module: id(),
            device: "unit:0f1e2d3c".into(),
            slot: first.slot,
            image: green(),
        }]
    );

    host.handle.press(ModulePress {
        module: id(),
        device: "unit:0f1e2d3c".into(),
        slot: first.slot,
    });
    host.until("the press", |host| host.received().len() >= 3)
        .await;
    assert_eq!(
        host.received()[2],
        r#"{"type":"pressed","device":"keypad-1","slot":"gridMiddle"}"#
    );
    assert!(
        host.received().iter().all(|line| !line.contains("unit:")),
        "no config key reaches the module: {:?}",
        host.received()
    );
    host.shutdown().await;
}

/// `stop_all` tells every module to leave and reaps it, starts none while
/// held, and `resume` starts them again; the agent hears of the stopped
/// run only once it listens again.
#[tokio::test]
async fn stop_all_reaps_every_module_and_resume_starts_them_again() {
    let host = Harness::start(
        "normal",
        plan(true, &[], vec![key(1, GridKey::Middle)]),
        timings(),
    );
    host.until("the module to draw", |host| !host.events().is_empty())
        .await;
    let [pid] = host.lines("pids.log").try_into().unwrap();

    host.stop_all().await;
    assert_eq!(host.received().last().unwrap(), r#"{"type":"shutdown"}"#);
    assert!(!exists(&pid), "the module is gone and reaped, no zombie");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(host.hellos().len(), 1, "a held host starts nothing");
    assert!(
        !host
            .events()
            .contains(&ModuleEvent::Stopped { module: id() }),
        "nothing reaches an agent that is not listening"
    );

    host.resume().await;
    host.until("the second run", |host| host.hellos().len() == 2)
        .await;
    host.until("the restarted module", |host| {
        host.state() == Some((ModuleState::Running, None))
    })
    .await;
    assert!(
        host.events()
            .contains(&ModuleEvent::Stopped { module: id() }),
        "the first run's end is told on resume: {:?}",
        host.events()
    );
    host.shutdown().await;
}

/// A module that was ready and then stopped reading was not a failed
/// start: five such kills leave it restarting, never failed.
#[tokio::test]
async fn a_ready_module_that_stalls_five_times_keeps_restarting() {
    let host = Harness::start(
        "deaf",
        plan(true, &[], vec![key(1, GridKey::Middle)]),
        timings(),
    );
    let press = ModulePress {
        module: id(),
        device: "keypad".into(),
        slot: key(1, GridKey::Middle).slot,
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while host.hellos().len() < 6 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no sixth start after the stalls: {:?}",
            host.state()
        );
        assert_ne!(
            host.state().map(|(state, _)| state),
            Some(ModuleState::Failed),
            "a stall is not a failed start"
        );
        for _ in 0..32 {
            host.handle.press(press.clone());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    host.shutdown().await;
}

/// An off and an on that reach the host as one plan still clear a failed
/// module: the plan's count of turn-offs moved.
#[tokio::test]
async fn an_off_and_on_coalesced_into_one_plan_clear_a_failure() {
    let timings = ModuleTimings {
        ready_timeout: Duration::from_secs(1),
        first_restart: Duration::from_millis(10),
        ..timings()
    };
    let host = Harness::start("silent", toggled_plan(true, 0), timings);
    host.until("five failed starts", |host| {
        matches!(host.state(), Some((ModuleState::Failed, _)))
    })
    .await;
    assert_eq!(host.hellos().len(), 5);

    host.plan.send_replace(toggled_plan(false, 1));
    host.plan.send_replace(toggled_plan(true, 1));
    host.until("a sixth start", |host| host.hellos().len() == 6)
        .await;
    host.shutdown().await;
}

/// A module drawing faster than the agent takes its images still hears
/// `shutdown` when the agent stops listening and leaves.
#[tokio::test]
async fn a_module_flooding_an_undrained_agent_still_hears_shutdown() {
    let host = Harness::start_with(
        "spam",
        plan(true, &[], vec![key(1, GridKey::Middle)]),
        timings(),
        Events::Undrained,
    );
    host.until("its keys", |host| host.received().len() >= 2)
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    host.shutdown().await;
    assert_eq!(host.received().last().unwrap(), r#"{"type":"shutdown"}"#);
    assert!(host.handle.statuses().is_empty(), "the host stopped");
}

/// A kill takes the module's whole process group: a wrapper's own child
/// does not outlive it.
#[tokio::test]
async fn a_killed_wrapper_takes_its_children_with_it() {
    let timings = ModuleTimings {
        ready_timeout: Duration::from_secs(1),
        first_restart: Duration::from_millis(10),
        ..timings()
    };
    let host = Harness::start(
        "wrapper",
        plan(true, &[], vec![key(1, GridKey::Middle)]),
        timings,
    );
    host.until("five killed starts", |host| {
        matches!(host.state(), Some((ModuleState::Failed, _)))
    })
    .await;
    let sleeps = host.lines("sleep.pids");
    assert_eq!(sleeps.len(), 5, "one sleep per run: {sleeps:?}");
    host.until("every sleep gone", |_| {
        sleeps.iter().all(|pid| !exists(pid))
    })
    .await;
    host.shutdown().await;
}

/// A ready module that exits while a child of its own holds its output open
/// has still ended: the child goes with it as soon as the module is reaped,
/// not once the output's grace runs out, and the module starts again.
#[tokio::test]
async fn a_module_that_exits_while_its_child_holds_its_output_restarts() {
    let timings = ModuleTimings {
        shutdown_grace: Duration::from_secs(5),
        ..timings()
    };
    let host = Harness::start(
        "orphan",
        plan(true, &[], vec![key(1, GridKey::Middle)]),
        timings,
    );
    host.until("the first run's child", |host| {
        !host.lines("sleep.pids").is_empty()
    })
    .await;
    let first = host.lines("sleep.pids").remove(0);
    host.within(Duration::from_secs(1), "the first run's child gone", |_| {
        !exists(&first)
    })
    .await;
    host.until("a second start", |host| host.hellos().len() >= 2)
        .await;
    assert!(
        host.events()
            .contains(&ModuleEvent::Stopped { module: id() }),
        "the agent heard the first run end: {:?}",
        host.events()
    );
    host.shutdown().await;
}

/// A module that leaves promptly on `shutdown` still takes the children it
/// started with it.
#[tokio::test]
async fn a_module_that_leaves_on_shutdown_takes_its_children() {
    let host = Harness::start(
        "helper",
        plan(true, &[], vec![key(1, GridKey::Middle)]),
        timings(),
    );
    host.until("the module to run", |host| {
        host.state() == Some((ModuleState::Running, None))
    })
    .await;
    let [sleep] = host.lines("sleep.pids").try_into().unwrap();

    host.shutdown().await;
    assert_eq!(host.received().last().unwrap(), r#"{"type":"shutdown"}"#);
    host.until("its child gone", |_| !exists(&sleep)).await;
}

/// A ready module that closes its input but keeps running is restarted as
/// soon as a line for it cannot be written, not left holding keys whose
/// presses it can never read: here its `keys`, the first line after ready.
#[tokio::test]
async fn a_module_that_closes_its_input_is_restarted() {
    let host = Harness::start(
        "closer",
        plan(true, &[], vec![key(1, GridKey::Middle)]),
        timings(),
    );
    host.until("a second start", |host| host.hellos().len() >= 2)
        .await;
    host.shutdown().await;
}
