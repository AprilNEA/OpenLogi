//! Frontmost backend for KWin (KDE Plasma), fed by a companion KWin script.
//!
//! KWin is not wlroots-based, so it implements no `wlr-foreign-toplevel`, and
//! its own `org.kde.KWin` D-Bus interface has no way to read the active window
//! without user interaction: `getWindowInfo` needs a window UUID that nothing
//! hands out, and `queryWindowInfo` asks the user to click a window. Verified
//! against KWin 6 on a live Plasma Wayland session — the whole reason this
//! backend exists rather than a direct D-Bus read.
//!
//! What KWin *does* offer is scripting: a script sees
//! `workspace.activeWindow.resourceClass` and can `callDBus` out. So the flow
//! runs the opposite way from the GNOME backend — there the shell serves and
//! OpenLogi polls; here **OpenLogi serves and the script pushes** on every
//! activation, because a KWin script can call D-Bus but cannot export a
//! service.
//!
//! # Why this backend loads the script itself
//!
//! Pushing has two consequences a polled backend does not have, and both are
//! solved by making the load part of startup rather than a user step:
//!
//! - **A pushed value cannot be asked for again.** The script reports the
//!   active window once at load and then only on change, so a script already
//!   running when OpenLogi starts has *already* sent its only unprompted
//!   update, into a bus name nobody owned yet. Focus would then stay unknown
//!   until the user next switched windows — possibly not for hours.
//! - **A push-fed backend cannot tell "no companion" from "nothing focused
//!   yet".** Both are an empty cache, so simply serving the name and hoping
//!   would make this candidate succeed on *every* session with a session bus
//!   — GNOME included — permanently suppressing the X11/XWayland fallback that
//!   is the only frontmost source those sessions have.
//!
//! So [`candidate`] claims the bus name first, then drives KWin's scripting
//! interface to (re)load the script, and reports failure unless every step
//! works. A session without the script installed, or without KWin at all,
//! fails at a definite step and falls through to the next candidate exactly as
//! before. Reloading an already-loaded script is deliberate: it is what makes
//! the load-time push land *after* the name exists.
//!
//! `resourceClass` is the same string X11 reports as `WM_CLASS`, so per-app
//! profile keys stay consistent across X11, XWayland and Plasma Wayland.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use tracing::debug;
use zbus::blocking::connection::Builder;
use zbus::{interface, proxy};

use super::FrontmostSource;

/// Bus name and object path the companion script pushes to. Distinct from the
/// GNOME extension's `org.openlogi.Frontmost`, which is a *served* interface
/// pointing the other way — sharing a name would let the two backends fight
/// over ownership on a session that somehow had both.
const DBUS_NAME: &str = "org.openlogi.KWinFrontmost";
const DBUS_PATH: &str = "/org/openlogi/KWinFrontmost";

/// The script's plugin id, which is also the name KWin's scripting interface
/// keys a loaded script by. Must match `KPlugin.Id` in the script's
/// `metadata.json`.
const SCRIPT_PLUGIN_ID: &str = "openlogi-frontmost";

/// Where the installed script sits inside an XDG data directory. KWin resolves
/// its script packages the same way, so searching the same directories finds
/// exactly what a System Settings install would have registered.
const SCRIPT_SUBPATH: &str = "kwin/scripts/openlogi-frontmost/contents/code/main.js";

/// Cap on every call into KWin. Without it a stalled compositor would block
/// the polling thread forever — the probe runs inside the `FRONTMOST_SOURCE`
/// initializer, so a stall there blocks every thread that touches it.
const METHOD_TIMEOUT: Duration = Duration::from_secs(5);

/// What the companion script has told us so far.
///
/// [`Focus::Silent`] is deliberately distinct from [`Focus::Empty`]: a push
/// carrying the empty string is KWin saying "nothing is focused", which is a
/// working pipeline, while silence may equally mean the script never ran.
/// Collapsing the two would make [`candidate`] unable to tell a live companion
/// from a dead one.
#[derive(Default, PartialEq, Eq)]
enum Focus {
    /// Nothing has been pushed yet.
    #[default]
    Silent,
    /// KWin activated no window — desktop focus, or the last window closing.
    Empty,
    /// The focused window's `resourceClass`.
    Window(String),
}

impl Focus {
    /// The identifier per-app profiles are keyed by, which exists only for a
    /// real window: both silence and an empty push mean "no frontmost app".
    fn app_id(&self) -> Option<String> {
        match self {
            Self::Window(class) => Some(class.clone()),
            Self::Silent | Self::Empty => None,
        }
    }
}

/// The latest [`Focus`], shared between the D-Bus receiver and the backend.
type Cached = Arc<Mutex<Focus>>;

/// How long [`candidate`] waits for the script's load-time push before giving
/// up on it. The push is sent as the script loads, so this is normally over in
/// milliseconds; the cap exists because `loadScript` reports success for a file
/// KWin never actually runs, making an arrived push the only real proof that
/// the companion works.
const FIRST_PUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// Gap between checks while waiting for that first push.
const FIRST_PUSH_POLL: Duration = Duration::from_millis(25);

/// D-Bus proxy for KWin's scripting interface. Note the lowercase `kwin` in the
/// interface name — `org.kde.KWin.Scripting` does not exist. Only the blocking
/// proxy is generated (`gen_async = false`), matching the synchronous startup
/// path this runs on.
#[proxy(
    interface = "org.kde.kwin.Scripting",
    default_service = "org.kde.KWin",
    default_path = "/Scripting",
    gen_async = false
)]
///
/// Every method carries an explicit `name`: KWin spells these members in
/// camelCase, while zbus would otherwise derive PascalCase ones that simply do
/// not exist on the interface.
trait KWinScripting {
    #[zbus(name = "isScriptLoaded")]
    fn is_script_loaded(&self, plugin_id: &str) -> zbus::Result<bool>;
    #[zbus(name = "unloadScript")]
    fn unload_script(&self, plugin_id: &str) -> zbus::Result<bool>;
    /// Returns the loaded script's id, which is of no use to us — the script
    /// is addressed by plugin id everywhere else.
    #[zbus(name = "loadScript")]
    fn load_script(&self, file_path: &str, plugin_id: &str) -> zbus::Result<i32>;
    /// Starts every loaded-but-not-yet-running script. A no-op for scripts that
    /// are already running, which is why the reload above is what actually
    /// re-triggers our script's load-time push.
    #[zbus(name = "start")]
    fn start(&self) -> zbus::Result<()>;
}

/// The D-Bus object the KWin script calls.
struct Receiver {
    cached: Cached,
}

#[interface(name = "org.openlogi.KWinFrontmost")]
impl Receiver {
    /// Record the newly activated window's `resourceClass`.
    ///
    /// An empty string means KWin activated no window (desktop focus, or the
    /// last window closing), which is reported as "no frontmost app" rather
    /// than as an app literally named "".
    fn set_focused_window_class(&self, class: &str) {
        let value = if class.is_empty() {
            Focus::Empty
        } else {
            Focus::Window(class.to_owned())
        };
        *self.cached.lock().unwrap_or_else(PoisonError::into_inner) = value;
    }
}

/// Frontmost backend fed by the companion KWin script.
pub(super) struct KWinScriptSource {
    cached: Cached,
    // Owning the connection is what keeps the bus name claimed and the object
    // served; nothing reads it again.
    _connection: zbus::blocking::Connection,
}

impl FrontmostSource for KWinScriptSource {
    fn frontmost_app_id(&self) -> Option<String> {
        self.cached
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .app_id()
    }

    fn name(&self) -> &'static str {
        "kwin-script"
    }
}

/// The XDG data directories, most specific first: `XDG_DATA_HOME` (or its
/// `~/.local/share` default) followed by `XDG_DATA_DIRS` (or its spec default).
/// A per-user install therefore shadows a system-wide one, matching KWin.
fn data_dirs() -> Vec<PathBuf> {
    let home = env::var_os("XDG_DATA_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));

    let shared = env::var_os("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from("/usr/local/share:/usr/share"));

    home.into_iter()
        .chain(env::split_paths(&shared))
        .filter(|dir| !dir.as_os_str().is_empty())
        .collect()
}

/// First installed copy of the script, or `None` when it is not installed.
///
/// `exists` is injected so the search order can be tested without touching the
/// filesystem.
fn find_script(dirs: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    dirs.iter()
        .map(|dir| dir.join(SCRIPT_SUBPATH))
        .find(|path| exists(path))
}

/// Why the companion could not be brought up.
#[derive(Debug, thiserror::Error)]
enum ReloadError {
    #[error("KWin scripting call failed: {0}")]
    Bus(#[from] zbus::Error),
    /// `loadScript` answers with a slot index, or `-1` when it refuses — which
    /// it does when that plugin id is already loaded. Reaching this means the
    /// unload above did not take effect, so the script KWin is running is not
    /// the one we just asked for and its load-time push is never coming.
    #[error("KWin refused to load the script (already loaded?)")]
    Refused,
    /// `loadScript` reports success for a path KWin never actually runs — it
    /// does not validate the file — so a missing push is the only evidence that
    /// the companion is not working.
    #[error("the script never reported a focused window")]
    Silent,
}

/// Reload the script so its load-time push lands after we own the bus name.
///
/// Unloading first is what forces that push: `start` alone does nothing for a
/// script KWin already started, so a script enabled through System Settings
/// would otherwise stay silent until the next window activation.
fn reload_script(
    connection: &zbus::blocking::Connection,
    path: &Path,
    cached: &Cached,
) -> Result<(), ReloadError> {
    let scripting = KWinScriptingProxy::new(connection)?;

    // The first call doubles as the "is this KWin at all?" probe: on a session
    // without it, this fails and the whole candidate declines.
    if scripting.is_script_loaded(SCRIPT_PLUGIN_ID)? {
        scripting.unload_script(SCRIPT_PLUGIN_ID)?;
    }
    if scripting.load_script(&path.to_string_lossy(), SCRIPT_PLUGIN_ID)? < 0 {
        return Err(ReloadError::Refused);
    }
    scripting.start()?;
    await_first_push(cached)
}

/// Block until the script pushes, or [`FIRST_PUSH_TIMEOUT`] elapses.
///
/// This is what keeps a broken or absent companion from capturing the backend
/// slot: without proof that pushes arrive, the candidate must decline so that
/// selection falls through to X11/XWayland.
fn await_first_push(cached: &Cached) -> Result<(), ReloadError> {
    let deadline = Instant::now() + FIRST_PUSH_TIMEOUT;
    loop {
        if *cached.lock().unwrap_or_else(PoisonError::into_inner) != Focus::Silent {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ReloadError::Silent);
        }
        thread::sleep(FIRST_PUSH_POLL);
    }
}

/// Serve the push endpoint and load the companion script, or `None` when this
/// is not a KWin session, the script is not installed, or the bus is
/// unreachable — in which case backend selection falls through as before.
pub(super) fn candidate() -> Option<Box<dyn FrontmostSource>> {
    let path = find_script(&data_dirs(), Path::exists).or_else(|| {
        debug!("kwin-script: no companion script installed under any XDG data directory");
        None
    })?;

    let cached: Cached = Arc::new(Mutex::new(Focus::default()));
    let receiver = Receiver {
        cached: Arc::clone(&cached),
    };
    // Claim the name *before* loading the script, so the script's load-time
    // push has somewhere to land.
    let connection = Builder::session()
        .and_then(|builder| builder.name(DBUS_NAME))
        .and_then(|builder| builder.serve_at(DBUS_PATH, receiver))
        .map(|builder| builder.method_timeout(METHOD_TIMEOUT))
        .and_then(Builder::build)
        .map_err(|error| debug!(%error, "kwin-script: could not serve the push endpoint"))
        .ok()?;

    // Dropping `connection` on the error path releases the bus name again, so a
    // failure here leaves nothing behind for the next candidate to trip over.
    reload_script(&connection, &path, &cached)
        .map_err(|error| debug!(%error, "kwin-script: companion script unusable"))
        .ok()?;

    debug!(
        "kwin-script: loaded {} from {}",
        SCRIPT_PLUGIN_ID,
        path.display()
    );
    Some(Box::new(KWinScriptSource {
        cached,
        _connection: connection,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a receiver plus the cache it writes into, without touching D-Bus.
    fn receiver() -> (Receiver, Cached) {
        let cached: Cached = Arc::new(Mutex::new(Focus::default()));
        (
            Receiver {
                cached: Arc::clone(&cached),
            },
            cached,
        )
    }

    /// Read through the same mapping the backend uses, so the tests exercise
    /// `Focus::app_id` rather than the cache representation directly.
    fn frontmost(cached: &Cached) -> Option<String> {
        cached
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .app_id()
    }

    #[test]
    fn reports_nothing_until_the_script_pushes() {
        let (_receiver, cached) = receiver();
        assert_eq!(frontmost(&cached), None);
    }

    #[test]
    fn records_the_pushed_window_class() {
        let (receiver, cached) = receiver();
        receiver.set_focused_window_class("org.kde.konsole");
        assert_eq!(frontmost(&cached), Some("org.kde.konsole".to_owned()));
    }

    /// The script sends `""` when KWin activated no window — focus on the
    /// desktop, or the last window closing. That is "no frontmost app", not an
    /// app whose name happens to be blank, which would otherwise become a
    /// per-app profile key of `""`.
    #[test]
    fn treats_an_empty_class_as_no_frontmost_window() {
        let (receiver, cached) = receiver();
        receiver.set_focused_window_class("org.kde.konsole");
        receiver.set_focused_window_class("");
        assert_eq!(frontmost(&cached), None);
    }

    /// Both report "no app", but only one of them proves the companion is
    /// alive — which is the whole basis on which the candidate decides whether
    /// to keep the backend slot.
    #[test]
    fn an_empty_push_still_counts_as_having_been_heard() {
        let (receiver, cached) = receiver();
        await_first_push(&cached).expect_err("silence must not count as heard");
        receiver.set_focused_window_class("");
        await_first_push(&cached).expect("an empty push is still a push");
    }

    /// Activations overwrite rather than accumulate — the cache is the *last*
    /// focused window, so switching back and forth stays correct.
    #[test]
    fn later_activations_replace_earlier_ones() {
        let (receiver, cached) = receiver();
        receiver.set_focused_window_class("org.kde.konsole");
        receiver.set_focused_window_class("org.kde.kcalc");
        assert_eq!(frontmost(&cached), Some("org.kde.kcalc".to_owned()));
        receiver.set_focused_window_class("org.kde.konsole");
        assert_eq!(frontmost(&cached), Some("org.kde.konsole".to_owned()));
    }

    /// `loadScript` reports success for a file KWin never runs, so silence is
    /// the only signal that the companion is not working — and it has to end
    /// the candidate rather than hold the slot with an empty cache.
    #[test]
    fn a_script_that_never_pushes_is_rejected() {
        let (_receiver, cached) = receiver();
        let error = await_first_push(&cached).expect_err("silence must not pass");
        assert!(matches!(error, ReloadError::Silent));
    }

    /// Not finding the script is the signal that this is not a KWin session
    /// with the companion installed, which is what preserves the X11/XWayland
    /// fallback for every other Wayland desktop.
    #[test]
    fn finds_no_script_when_it_is_installed_nowhere() {
        let dirs = vec![PathBuf::from("/one"), PathBuf::from("/two")];
        assert_eq!(find_script(&dirs, |_| false), None);
    }

    #[test]
    fn finds_the_script_inside_a_data_directory() {
        let dirs = vec![PathBuf::from("/usr/share")];
        assert_eq!(
            find_script(&dirs, |path| path.starts_with("/usr/share")),
            Some(PathBuf::from("/usr/share").join(SCRIPT_SUBPATH))
        );
    }

    /// `data_dirs` is ordered most-specific-first, and `find_script` must keep
    /// that order: a per-user install shadows a system-wide one, the same way
    /// KWin resolves its own script packages.
    #[test]
    fn prefers_the_earliest_data_directory() {
        let dirs = vec![
            PathBuf::from("/home/user/.local/share"),
            PathBuf::from("/usr/share"),
        ];
        assert_eq!(
            find_script(&dirs, |_| true),
            Some(PathBuf::from("/home/user/.local/share").join(SCRIPT_SUBPATH))
        );
    }
}
