//! Linux systemd user-unit ownership and autostart reconciliation.
//!
//! When a packaged unit already launches this exact binary — installed by a
//! distribution package, `install.sh`, or an administrator — it is simply
//! enabled. Generating a second copy would duplicate its directives one tier
//! higher, shadowing later changes to it and outliving its removal, since no
//! package script can clean a home directory.
//!
//! Otherwise a systemd **user** unit at
//! `$XDG_DATA_HOME/systemd/user/openlogi-agent.service` (default
//! `~/.local/share/systemd/user/openlogi-agent.service`) is written/removed,
//! then `systemctl --user daemon-reload` and `enable`/`disable` are called.
//! That is the tier systemd reserves for units installed *on* a user's behalf;
//! `$XDG_CONFIG_HOME/systemd/user` outranks it and belongs to the user, so a
//! unit they author by hand always wins over this generated one.
//! `Restart=on-failure` mirrors the macOS `KeepAlive=SuccessfulExit:false`
//! semantics. A clean `exit(0)` leaves the unit enabled but stopped until the
//! next session login.
//!
//! Neither user-writable tier is exclusively this app's, so nothing is written
//! over or deleted unless it round-trips through the renderer and is therefore
//! provably one of ours — including the file earlier versions wrote into
//! `$XDG_CONFIG_HOME/systemd/user`, which is cleaned up on reconcile. A unit
//! anything else installed under the same name is left alone, and the setting
//! is honoured through enablement alone.
//!
//! Enablement is tracked separately, because `systemctl --user enable` records
//! only that a unit is enabled, never who asked. Without that record,
//! reconciling a disabled setting would withdraw the enablement made by the
//! documented `systemctl --user enable --now` install step. A marker beside the
//! config notes an enablement this app made; `disable` runs only against one.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

/// Reconcile the agent's autostart state with `enabled`.
///
/// Idempotent; failures are logged, not propagated — startup must not abort
/// because an autostart directory is read-only or systemd is unavailable.
pub(super) fn reconcile(enabled: bool) {
    if let Err(e) = apply(enabled) {
        warn!(error = %e, enabled, "agent systemd unit reconcile failed");
    }
}

/// Name of the systemd user unit file.
const UNIT_NAME: &str = "openlogi-agent.service";

/// Unit directories to fall back on when systemd cannot be asked for the real
/// load path, highest precedence first.
///
/// Mirrors the user-mode load path in `systemd.unit(5)` rather than a fixed set
/// of absolute paths: `$XDG_CONFIG_DIRS`, `$XDG_RUNTIME_DIR`, and
/// `$XDG_DATA_DIRS` all contribute directories that differ per session, and a
/// unit in the runtime tier outranks the one this app generates. The two tiers
/// this app writes to are omitted here and filtered again by [`user_unit_dirs`].
fn fallback_unit_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    push_xdg_dirs(&mut dirs, "XDG_CONFIG_DIRS", "/etc/xdg");
    dirs.push(PathBuf::from("/etc/systemd/user"));
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        dirs.push(Path::new(&runtime).join("systemd").join("user"));
    }
    dirs.push(PathBuf::from("/run/systemd/user"));
    push_xdg_dirs(&mut dirs, "XDG_DATA_DIRS", "/usr/local/share:/usr/share");
    dirs.push(PathBuf::from("/usr/local/lib/systemd/user"));
    dirs.push(PathBuf::from("/usr/lib/systemd/user"));
    dirs
}

/// Append `systemd/user` under each entry of a colon-separated XDG base list,
/// falling back to `default` when the variable is unset or empty.
fn push_xdg_dirs(dirs: &mut Vec<PathBuf>, var: &str, default: &str) {
    let value = std::env::var(var).unwrap_or_default();
    let list = if value.trim().is_empty() {
        default
    } else {
        &value
    };
    dirs.extend(
        list.split(':')
            .map(str::trim)
            .filter(|base| base.starts_with('/'))
            .map(|base| Path::new(base).join("systemd").join("user")),
    );
}

/// The user unit load path, highest precedence first, minus the two tiers this
/// app writes to.
///
/// Asked of systemd rather than assumed. The real path is environment
/// dependent — `$XDG_CONFIG_DIRS`, `/run`, custom `$XDG_DATA_DIRS`, and the
/// Flatpak export directories all appear in it — so enumerating it by hand
/// would answer for a machine nobody is running. What remains after the filter
/// is every directory a unit could arrive in from somewhere other than here.
fn user_unit_dirs() -> Vec<PathBuf> {
    let ours: Vec<PathBuf> = [generated_unit_path(), legacy_unit_path()]
        .into_iter()
        .filter_map(|path| {
            path.ok()
                .and_then(|path| path.parent().map(Path::to_path_buf))
        })
        .collect();
    let dirs = systemd_unit_paths().unwrap_or_else(fallback_unit_dirs);
    exclude_dirs(dirs, &ours)
}

/// The load path as `systemd-analyze` reports it, or `None` when it cannot be
/// run — not installed, or no session bus to query.
fn systemd_unit_paths() -> Option<Vec<PathBuf>> {
    let output = std::process::Command::new("systemd-analyze")
        .args(["--user", "unit-paths"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let listing = String::from_utf8(output.stdout).ok()?;
    let dirs = parse_unit_paths(&listing);
    (!dirs.is_empty()).then_some(dirs)
}

/// One directory per non-empty line, order preserved.
fn parse_unit_paths(listing: &str) -> Vec<PathBuf> {
    listing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Drop `exclude` from `dirs`, preserving precedence order.
fn exclude_dirs(dirs: Vec<PathBuf>, exclude: &[PathBuf]) -> Vec<PathBuf> {
    dirs.into_iter()
        .filter(|dir| !exclude.contains(dir))
        .collect()
}

fn apply(enabled: bool) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let launch = LaunchCommand::for_agent(&exe);
    migrate_legacy_unit();

    let path = generated_unit_path()?;
    let current = std::fs::read_to_string(&path).ok();

    // A unit at a user-writable tier that this app did not render belongs to
    // whoever wrote it — another installer, or the user. Never overwrite it and
    // never delete it; the toggle is honoured through enablement alone.
    if current
        .as_deref()
        .is_some_and(|have| !is_generated_unit(have))
    {
        warn!(
            path = %path.display(),
            "leaving a systemd user unit this app did not write in place",
        );
        if enabled {
            enable_unit();
        } else {
            disable_unit_if_ours();
        }
        return Ok(());
    }

    // A packaged unit that already launches this exact binary makes a generated
    // copy pure redundancy — identical directives, one tier higher. Writing one
    // would shadow the package (later changes to the packaged unit would never
    // reach this user) and outlive it (uninstall cannot reach a home
    // directory). Enable the packaged unit instead and write nothing.
    if enabled && let Some(packaged) = packaged_unit_for(&launch.program) {
        remove_generated_unit()?;
        info!(path = %packaged.display(), "enabling the packaged systemd user unit");
        enable_unit();
        return Ok(());
    }

    let desired = enabled.then(|| launch.render_unit());
    match (desired.as_deref(), current.as_deref()) {
        (Some(want), Some(have)) if want == have => {
            debug!(path = %path.display(), "systemd user unit already current");
            // Re-enable unconditionally: the unit file is current but the user
            // may have manually disabled the service since the last reconcile.
            enable_unit();
        }
        (Some(want), _) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, want)?;
            info!(path = %path.display(), "systemd user unit written");
            run_systemctl(&["daemon-reload"]);
            enable_unit();
        }
        (None, Some(_)) => {
            disable_unit_if_ours();
            remove_generated_unit()?;
        }
        (None, None) => {
            debug!("systemd user unit already absent");
            disable_unit_if_ours();
        }
    }
    Ok(())
}

/// Path to the marker recording that *this app* enabled the unit.
///
/// `systemctl --user enable` writes a symlink that carries no record of who
/// asked for it, and the unit name is shared with whatever a package or the
/// user installs. Without this, reconciling a disabled setting would withdraw
/// an enablement made by the documented `systemctl --user enable --now`
/// install step, silently turning off autostart the user asked for.
fn enablement_marker_path() -> io::Result<PathBuf> {
    let data_dir = openlogi_core::paths::data_dir().map_err(io::Error::other)?;
    Ok(data_dir.join("autostart.enabled"))
}

/// Enable the unit and record that this app is the one that did.
///
/// The claim is recorded *before* enabling. An enablement this app cannot
/// record is one a later reconcile could never withdraw, leaving autostart
/// running while the setting reads off — so if the marker cannot be written,
/// autostart is left alone rather than turned on untrackably. If `systemctl`
/// then fails, the claim is dropped again: recording an enablement that never
/// happened would let a later reconcile disable something this app never
/// turned on.
///
/// Claiming an enablement the user made by hand is deliberate: reaching the
/// toggle at all is an explicit request for this app to manage autostart, and
/// the setting has to mean what it says when it is switched back off.
fn enable_unit() {
    let Ok(marker) = enablement_marker_path() else {
        return;
    };
    enable_unit_with(&marker, || run_systemctl(&["enable", UNIT_NAME]));
}

/// The ordering and rollback rules of [`enable_unit`], over an injectable
/// marker path and enable step so the state transitions can be tested.
///
/// A claim that already existed is left alone when `enable` fails: the failure
/// may be transient (no session bus yet at login), and dropping a claim made by
/// an earlier reconcile would leave an enablement this app owns with nothing
/// able to withdraw it. Only a claim *this* call created is rolled back.
fn enable_unit_with(marker: &Path, enable: impl FnOnce() -> bool) {
    let claimed_now = match record_enablement_at(marker) {
        Ok(claimed_now) => claimed_now,
        Err(e) => {
            warn!(
                error = %e,
                "could not record the autostart enablement; leaving autostart unchanged",
            );
            return;
        }
    };
    if !enable() && claimed_now {
        clear_marker_at(marker);
    }
}

/// Drop this app's claim on the enablement. Absent is success.
fn clear_marker_at(marker: &Path) {
    match std::fs::remove_file(marker) {
        Ok(()) => debug!(path = %marker.display(), "cleared the autostart marker"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            warn!(error = %e, path = %marker.display(), "could not clear the autostart marker");
        }
    }
}

/// Record this app's claim on the enablement, reporting whether the claim was
/// created by this call rather than already held from an earlier reconcile.
///
/// Created exclusively, so the answer is decided by the filesystem and not by
/// a check made a moment earlier: of any number of callers racing for a
/// missing marker, exactly one is told it made the claim. The rollback in
/// [`enable_unit_with`] trusts that answer, and a second "yes" there would let
/// one caller's failure withdraw a claim another caller still holds.
fn record_enablement_at(path: &Path) -> io::Result<bool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

/// Withdraw the enablement only when this app recorded making it.
///
/// A missing marker means the enablement came from somewhere else — the
/// install instructions, another tool, the user — and is not ours to remove.
/// Losing the marker therefore fails safe: autostart keeps working, and the
/// user can still turn it off the way they turned it on.
fn disable_unit_if_ours() {
    let Ok(marker) = enablement_marker_path() else {
        return;
    };
    if !marker.exists() {
        debug!("autostart enablement was not made by OpenLogi; leaving it alone");
        return;
    }
    if !run_systemctl(&["disable", UNIT_NAME]) {
        // Keep the marker so the next reconcile retries rather than stranding
        // an enablement this app is still responsible for.
        return;
    }
    clear_marker_at(&marker);
}

/// Path to the generated unit:
/// `$XDG_DATA_HOME/systemd/user/openlogi-agent.service`
/// (default `~/.local/share/systemd/user/openlogi-agent.service`).
///
/// The *data* tier, not `$XDG_CONFIG_HOME`: systemd ranks it below the user's
/// own config directory, so a unit the user writes by hand always wins over
/// this generated one. `$XDG_CONFIG_HOME/systemd/user` belongs to them.
fn generated_unit_path() -> io::Result<PathBuf> {
    let data_home = openlogi_core::paths::xdg_data_home().map_err(io::Error::other)?;
    Ok(data_home.join("systemd").join("user").join(UNIT_NAME))
}

/// The location earlier versions wrote to, inside the user's own config tier.
/// Read only, to clean up after them — never written.
fn legacy_unit_path() -> io::Result<PathBuf> {
    let config_home = openlogi_core::paths::xdg_config_home().map_err(io::Error::other)?;
    Ok(config_home.join("systemd").join("user").join(UNIT_NAME))
}

/// Remove the unit earlier versions wrote into `$XDG_CONFIG_HOME/systemd/user`.
///
/// That path outranks every other tier, so leaving it behind would shadow both
/// the generated unit and any packaged one indefinitely — including a stale
/// `ExecStart` pointing at a binary that no longer exists.
///
/// Only a file this app generated is removed, and provenance is exact rather
/// than heuristic. One template has ever shipped, parameterised solely by
/// `ExecStart`, so splicing a file's own `ExecStart` line back into that
/// template reproduces the file byte for byte if and only if we rendered it.
/// Anything carrying another directive is the user's: it is left in place and
/// keeps winning, which is the right outcome for a unit they chose to author.
fn migrate_legacy_unit() {
    let Ok(path) = legacy_unit_path() else {
        return;
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return;
    };
    if !is_generated_unit(&contents) {
        warn!(
            path = %path.display(),
            "leaving a hand-edited systemd user unit in place; it takes precedence over OpenLogi's own",
        );
        return;
    }
    match std::fs::remove_file(&path) {
        Ok(()) => {
            info!(path = %path.display(), "removed the generated systemd user unit from the user's config tier");
            // The enable symlink still points at the file just deleted; the
            // reload plus the enable that follows re-point it at the new unit.
            run_systemctl(&["daemon-reload"]);
        }
        Err(e) => {
            warn!(error = %e, path = %path.display(), "could not remove the legacy systemd user unit");
        }
    }
}

/// Whether `contents` is a unit this app rendered, for *any* executable path.
fn is_generated_unit(contents: &str) -> bool {
    exec_start_value(contents).is_some_and(|value| render_unit_with_exec(value) == contents)
}

/// The verbatim, still-escaped value of the file's single `ExecStart=` line.
///
/// `None` when there is no such line, or more than one — neither shape is
/// something [`render_unit`] can produce.
fn exec_start_value(contents: &str) -> Option<&str> {
    let mut values = contents
        .lines()
        .filter_map(|line| line.strip_prefix("ExecStart="));
    let value = values.next()?;
    values.next().is_none().then_some(value)
}

/// The first system-tier unit whose `ExecStart` launches `exe`, if any.
///
/// A unit that launches some *other* binary is not a match: the generated unit
/// is what makes autostart work for a build the packaged unit cannot describe,
/// so in that case the normal write path must still run.
fn packaged_unit_for(exe: &Path) -> Option<PathBuf> {
    packaged_unit_in(&user_unit_dirs(), exe)
}

/// [`packaged_unit_for`] over an injectable set of directories, listed highest
/// precedence first.
///
/// Only the *effective* unit is considered — the first one that exists. A lower
/// entry is never matched past a higher one that names a different executable,
/// because systemd would resolve the name to that higher unit: enabling on the
/// strength of a shadowed entry would start a binary this app never verified.
fn packaged_unit_in(dirs: &[PathBuf], exe: &Path) -> Option<PathBuf> {
    let effective = dirs
        .iter()
        .map(|dir| dir.join(UNIT_NAME))
        // A /dev/null mask exists but is not a regular file. It must block
        // lower entries just like an empty unit file does.
        .find(|path| path.exists())?;
    let contents = std::fs::read_to_string(&effective).ok()?;
    let packaged = exec_start_value(&contents).map(unescape_systemd_exec)?;
    same_executable(Path::new(&packaged), exe).then_some(effective)
}

/// Recover the executable path from a rendered `ExecStart` value.
///
/// Inverts [`escape_systemd_exec`] and drops any arguments. Units using
/// systemd's `ExecStart` prefix characters (`-`, `@`, `+`, `!`) are not
/// unwrapped: they are nothing this app writes, and failing to match one
/// simply falls back to generating a unit, which is the safe direction.
fn unescape_systemd_exec(value: &str) -> String {
    let program = match value.strip_prefix('"') {
        Some(rest) => rest.split_once('"').map_or_else(
            || rest.to_string(),
            |(inner, _)| inner.replace("\\\"", "\""),
        ),
        None => value
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string(),
    };
    program.replace("%%", "%").replace("$$", "$")
}

/// Whether two paths name the same executable.
///
/// Resolved through symlinks when both exist, so a merged-`/usr` layout or an
/// `install.sh --prefix` that points one at the other still compares equal.
/// Falls back to a literal comparison when either side cannot be resolved.
fn same_executable(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Delete the generated unit if present, and only if this app rendered it.
///
/// The data tier is where OpenLogi writes, but it is not exclusively its own —
/// another tool can install a unit under the same name. Absent is success.
fn remove_generated_unit() -> io::Result<()> {
    let path = generated_unit_path()?;
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    if !is_generated_unit(&contents) {
        warn!(
            path = %path.display(),
            "leaving a systemd user unit this app did not write in place",
        );
        return Ok(());
    }
    std::fs::remove_file(&path)?;
    info!(path = %path.display(), "removed the generated systemd user unit");
    run_systemctl(&["daemon-reload"]);
    Ok(())
}

/// What `ExecStart` has to run to bring *this* agent back at login.
///
/// Normally the executable itself. Inside an AppImage the executable lives on
/// a FUSE mount the runtime creates per launch and tears down when the last
/// process from that launch exits, so a unit naming it would point at nothing
/// by the next login. There the durable path is the AppImage file, and its
/// `AppRun` dispatches on the first argument to start the agent instead of
/// the GUI (`packaging/linux/appimage/AppRun`).
#[derive(Debug, PartialEq, Eq)]
struct LaunchCommand {
    program: PathBuf,
    args: Vec<String>,
}

impl LaunchCommand {
    fn for_agent(exe: &Path) -> Self {
        Self::for_agent_in(
            exe,
            std::env::var_os("APPIMAGE").map(PathBuf::from),
            std::env::var_os("APPDIR").map(PathBuf::from),
        )
    }

    /// [`Self::for_agent`] over the two variables the AppImage runtime exports.
    ///
    /// Both must agree with the running executable: `APPIMAGE` alone is what
    /// any process launched *from* an AppImage inherits, so an agent installed
    /// from a package but started by an AppImage GUI must still register its
    /// own path, not the GUI's image.
    fn for_agent_in(exe: &Path, appimage: Option<PathBuf>, appdir: Option<PathBuf>) -> Self {
        if let (Some(appimage), Some(appdir)) = (appimage, appdir)
            && exe.starts_with(&appdir)
        {
            return Self {
                program: appimage,
                args: vec![openlogi_core::brand::Helper::Agent.executable().to_owned()],
            };
        }
        Self {
            program: exe.to_path_buf(),
            args: Vec::new(),
        }
    }

    /// Render the systemd user unit that launches this command.
    ///
    /// `Restart=on-failure` mirrors the macOS `KeepAlive=SuccessfulExit:false`
    /// semantics: the agent is respawned after a crash but a clean `exit(0)`
    /// (e.g. the tray's Quit) stays stopped until the next login.
    fn render_unit(&self) -> String {
        let mut exec_start = escape_systemd_exec(&self.program.to_string_lossy());
        for arg in &self.args {
            exec_start.push(' ');
            exec_start.push_str(&escape_systemd_exec(arg));
        }
        render_unit_with_exec(&exec_start)
    }
}

/// The unit for a bare executable path: [`LaunchCommand::render_unit`] with
/// no arguments, kept as the shorthand the tests are written against.
#[cfg(test)]
fn render_unit(exe: &str) -> String {
    render_unit_with_exec(&escape_systemd_exec(exe))
}

/// Render the unit around an `ExecStart` value that is **already escaped**.
///
/// Split out so [`is_generated_unit`] can splice a file's own `ExecStart` line
/// back in verbatim: running [`escape_systemd_exec`] over an already-escaped
/// value would double `%%` into `%%%%`, and a unit this app wrote would fail to
/// match itself.
fn render_unit_with_exec(exec_start: &str) -> String {
    format!(
        "[Unit]\n\
        Description=OpenLogi background agent (Logitech HID++ device control)\n\
        After=graphical-session.target\n\
        \n\
        [Service]\n\
        Type=simple\n\
        ExecStart={exec_start}\n\
        Restart=on-failure\n\
        RestartSec=5\n\
        \n\
        [Install]\n\
        WantedBy=graphical-session.target\n"
    )
}

/// Escape a string for use as `ExecStart` in a systemd unit file.
///
/// `%` starts a specifier and must be doubled. A value containing spaces is
/// wrapped in double quotes (inner `"` are backslash-escaped).
fn escape_systemd_exec(s: &str) -> String {
    let doubled = s.replace('%', "%%").replace('$', "$$");
    if doubled.contains(' ') {
        format!("\"{}\"", doubled.replace('"', "\\\""))
    } else {
        doubled
    }
}

/// Invoke `systemctl --user <args>`. Failures are logged but not propagated —
/// the unit file write is the authoritative record; enable/disable is
/// best-effort (e.g. the session D-Bus may be unavailable in some environments).
fn run_systemctl(args: &[&str]) -> bool {
    let label = SystemctlArgsDisplay(args);
    let mut cmd = std::process::Command::new("systemctl");
    cmd.arg("--user").args(args);
    match cmd.output() {
        Ok(out) if out.status.success() => {
            debug!("systemctl --user {label} succeeded");
            true
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            warn!(
                "systemctl --user {label} exited {}: {}",
                out.status,
                stderr.trim()
            );
            false
        }
        Err(e) => {
            warn!("systemctl --user {label} failed to spawn: {e}");
            false
        }
    }
}

struct SystemctlArgsDisplay<'a, 'b>(&'a [&'b str]);

impl fmt::Display for SystemctlArgsDisplay<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut separator = "";
        for arg in self.0 {
            write!(f, "{separator}{arg}")?;
            separator = " ";
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
