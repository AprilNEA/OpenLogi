//! Autostart reconciliation for the background agent.
//!
//! Implements `launch_at_login` by writing/removing a platform-specific
//! autostart descriptor whenever the setting changes. The reconcile is
//! idempotent: it writes only when the content differs, and removes only when
//! the file exists. Failures are logged but not propagated — startup must not
//! abort because an autostart directory is read-only.
//!
//! ## macOS
//!
//! A `LaunchAgent` plist at `~/Library/LaunchAgents/org.openlogi.agent.plist`
//! is kept in sync with the running agent executable. `KeepAlive` is
//! `{SuccessfulExit: false}` — the always-on daemon is respawned after a crash
//! (mirroring Logi Options+'s own agent), but the tray's "Quit" (a clean
//! `exit(0)`) is *not* relaunched, so Quit actually stops it until the next
//! login. No `--minimized`: the agent is always headless.
//!
//! The legacy `org.openlogi.openlogi` plist (the pre-split GUI autostart) is
//! removed on every reconcile so the GUI no longer self-launches.
//!
//! Production should register via `SMAppService` once the app is signed +
//! bundled with the plist in `Contents/Library/LaunchAgents`.
//! TODO(signing): add the `SMAppService` registration path.
//!
//! ## Linux
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

use tracing::debug;

#[cfg(target_os = "linux")]
use std::fmt;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::io;
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::path::PathBuf;
#[cfg(target_os = "windows")]
use tracing::warn;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use tracing::{info, warn};

/// Stable launch-agent identifier for the background agent.
///
/// Deliberately *not* `brand::AGENT_ID`, though it currently matches: this is a
/// filesystem key (`~/Library/LaunchAgents/<label>.plist`), so renaming it
/// orphans the plist already on disk. Following a bundle-id change silently
/// would leave users with two autostart entries; changing it is a migration,
/// which is what [`LEGACY_LABEL`] is for.
#[cfg(target_os = "macos")]
const LABEL: &str = "org.openlogi.agent";

/// The pre-split GUI autostart label, removed on migration. Frozen history —
/// never link it to `brand::APP_ID`, which it happens to match: if that value
/// ever changes, this one must not, or the stale plist is never cleaned up.
#[cfg(target_os = "macos")]
const LEGACY_LABEL: &str = "org.openlogi.openlogi";

/// Reconcile the agent's autostart state with `enabled`.
///
/// Idempotent; failures are logged, not propagated — startup must not abort
/// because an autostart directory is read-only or systemd is unavailable.
pub fn reconcile(enabled: bool) {
    #[cfg(target_os = "macos")]
    {
        remove_legacy();
        if let Err(e) = reconcile_macos(enabled) {
            warn!(error = %e, enabled, "agent LaunchAgent reconcile failed");
        }
    }
    #[cfg(target_os = "windows")]
    if let Err(e) = reconcile_windows(enabled) {
        warn!(error = %e, enabled, "agent autostart reconcile failed");
    }
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = reconcile_linux(enabled) {
            warn!(error = %e, enabled, "agent systemd unit reconcile failed");
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        if enabled {
            debug!("launch_at_login set but no autostart backend on this platform");
        }
        let _ = enabled;
    }
}

#[cfg(target_os = "macos")]
fn reconcile_macos(enabled: bool) -> io::Result<()> {
    let path = plist_path(LABEL)?;
    let exe = std::env::current_exe()?;
    let desired = enabled
        .then(|| render_plist(&exe.to_string_lossy()))
        .transpose()?;

    let current = std::fs::read_to_string(&path).ok();
    match (desired.as_deref(), current.as_deref()) {
        (Some(want), Some(have)) if want == have => {
            debug!(path = %path.display(), "agent LaunchAgent already current");
        }
        (Some(want), _) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, want)?;
            info!(path = %path.display(), "agent LaunchAgent installed");
        }
        (None, Some(_)) => {
            std::fs::remove_file(&path)?;
            info!(path = %path.display(), "agent LaunchAgent removed");
        }
        (None, None) => debug!("agent LaunchAgent already absent"),
    }
    Ok(())
}

/// Remove the legacy GUI LaunchAgent so the old `--minimized` GUI no longer
/// self-launches at login. Best-effort: a present-but-unreadable file is left
/// alone (logged), and a currently-running old instance survives until logout.
#[cfg(target_os = "macos")]
fn remove_legacy() {
    let Ok(path) = plist_path(LEGACY_LABEL) else {
        return;
    };
    if !path.exists() {
        return;
    }
    match std::fs::remove_file(&path) {
        Ok(()) => info!("removed legacy GUI LaunchAgent ({LEGACY_LABEL})"),
        Err(e) => warn!(error = %e, "could not remove legacy LaunchAgent"),
    }
}

#[cfg(target_os = "macos")]
fn plist_path(label: &str) -> io::Result<PathBuf> {
    let home =
        openlogi_core::paths::home_dir().map_err(|e| io::Error::new(io::ErrorKind::NotFound, e))?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist")))
}

#[cfg(target_os = "macos")]
fn render_plist(exe: &str) -> io::Result<String> {
    let mut keep_alive = plist::Dictionary::new();
    keep_alive.insert("SuccessfulExit".into(), plist::Value::Boolean(false));

    let mut root = plist::Dictionary::new();
    root.insert("Label".into(), plist::Value::String(LABEL.into()));
    root.insert(
        "ProgramArguments".into(),
        plist::Value::Array(vec![plist::Value::String(exe.into())]),
    );
    root.insert("RunAtLoad".into(), plist::Value::Boolean(true));
    root.insert("KeepAlive".into(), plist::Value::Dictionary(keep_alive));

    let mut bytes = Vec::new();
    plist::to_writer_xml(&mut bytes, &plist::Value::Dictionary(root)).map_err(io::Error::other)?;
    String::from_utf8(bytes).map_err(io::Error::other)
}

/// HKCU autostart subkey + value name for the agent.
#[cfg(target_os = "windows")]
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(target_os = "windows")]
const RUN_VALUE: &str = "OpenLogiAgent";

/// Windows autostart: keep `HKCU\…\Run\OpenLogiAgent` pointed at the running
/// agent executable so the next login relaunches it, or remove it when disabled.
///
/// Unlike the macOS LaunchAgent there is no crash-respawn — a Run-key entry only
/// fires once at login. A future SCM/Task Scheduler backend could add restart
/// semantics; the login-launch path is enough for the headless agent today.
#[cfg(target_os = "windows")]
fn reconcile_windows(enabled: bool) -> std::io::Result<()> {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    let (run, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_SUBKEY)?;
    if enabled {
        let exe = std::env::current_exe()?;
        // Windows parses Run-key values as command lines, so a bare path with
        // spaces (e.g. under "C:\Program Files\") is split at the first space and
        // the launch silently fails. Quote it. Built via OsString so a non-UTF-8
        // path survives exactly (no lossy `display()`).
        let mut quoted = std::ffi::OsString::from("\"");
        quoted.push(exe.as_os_str());
        quoted.push("\"");
        run.set_value(RUN_VALUE, &quoted)?;
        debug!(value = RUN_VALUE, "agent autostart registry value set");
    } else {
        match run.delete_value(RUN_VALUE) {
            Ok(()) => debug!(value = RUN_VALUE, "agent autostart registry value removed"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!("agent autostart registry value already absent");
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// ── Linux systemd user-unit reconcile ────────────────────────────────────────

/// Name of the systemd user unit file.
#[cfg(target_os = "linux")]
const UNIT_NAME: &str = "openlogi-agent.service";

/// Unit directories outside the two user-writable tiers, in systemd's own
/// precedence order (`systemd.unit(5)`, "Load path when running in user mode").
///
/// Probed to answer one question: has something outside this app's control —
/// a distribution package, `install.sh`, or an administrator — already
/// installed this unit? The user tiers are deliberately absent: a file there
/// is either ours or the user's, never a package's.
#[cfg(target_os = "linux")]
const SYSTEM_UNIT_DIRS: &[&str] = &[
    "/etc/systemd/user",
    "/usr/local/share/systemd/user",
    "/usr/share/systemd/user",
    "/usr/local/lib/systemd/user",
    "/usr/lib/systemd/user",
];

#[cfg(target_os = "linux")]
fn reconcile_linux(enabled: bool) -> io::Result<()> {
    let exe = std::env::current_exe()?;
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
    if enabled && let Some(packaged) = packaged_unit_for(&exe) {
        remove_generated_unit()?;
        info!(path = %packaged.display(), "enabling the packaged systemd user unit");
        enable_unit();
        return Ok(());
    }

    let desired = enabled.then(|| render_unit(&exe.to_string_lossy()));
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn enable_unit() {
    if let Err(e) = record_enablement() {
        warn!(
            error = %e,
            "could not record the autostart enablement; leaving autostart unchanged",
        );
        return;
    }
    if !run_systemctl(&["enable", UNIT_NAME]) {
        clear_enablement_marker();
    }
}

/// Drop this app's claim on the enablement. Absent is success.
#[cfg(target_os = "linux")]
fn clear_enablement_marker() {
    let Ok(marker) = enablement_marker_path() else {
        return;
    };
    match std::fs::remove_file(&marker) {
        Ok(()) => debug!(path = %marker.display(), "cleared the autostart marker"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            warn!(error = %e, path = %marker.display(), "could not clear the autostart marker");
        }
    }
}

#[cfg(target_os = "linux")]
fn record_enablement() -> io::Result<()> {
    let path = enablement_marker_path()?;
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, b"")
}

/// Withdraw the enablement only when this app recorded making it.
///
/// A missing marker means the enablement came from somewhere else — the
/// install instructions, another tool, the user — and is not ours to remove.
/// Losing the marker therefore fails safe: autostart keeps working, and the
/// user can still turn it off the way they turned it on.
#[cfg(target_os = "linux")]
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
    clear_enablement_marker();
}

/// Path to the generated unit:
/// `$XDG_DATA_HOME/systemd/user/openlogi-agent.service`
/// (default `~/.local/share/systemd/user/openlogi-agent.service`).
///
/// The *data* tier, not `$XDG_CONFIG_HOME`: systemd ranks it below the user's
/// own config directory, so a unit the user writes by hand always wins over
/// this generated one. `$XDG_CONFIG_HOME/systemd/user` belongs to them.
#[cfg(target_os = "linux")]
fn generated_unit_path() -> io::Result<PathBuf> {
    let data_home = openlogi_core::paths::xdg_data_home().map_err(io::Error::other)?;
    Ok(data_home.join("systemd").join("user").join(UNIT_NAME))
}

/// The location earlier versions wrote to, inside the user's own config tier.
/// Read only, to clean up after them — never written.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn is_generated_unit(contents: &str) -> bool {
    exec_start_value(contents).is_some_and(|value| render_unit_with_exec(value) == contents)
}

/// The verbatim, still-escaped value of the file's single `ExecStart=` line.
///
/// `None` when there is no such line, or more than one — neither shape is
/// something [`render_unit`] can produce.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
fn packaged_unit_for(exe: &Path) -> Option<PathBuf> {
    SYSTEM_UNIT_DIRS
        .iter()
        .map(|dir| Path::new(dir).join(UNIT_NAME))
        .find(|path| {
            std::fs::read_to_string(path)
                .ok()
                .and_then(|contents| exec_start_value(&contents).map(unescape_systemd_exec))
                .is_some_and(|packaged| same_executable(Path::new(&packaged), exe))
        })
}

/// Recover the executable path from a rendered `ExecStart` value.
///
/// Inverts [`escape_systemd_exec`] and drops any arguments. Units using
/// systemd's `ExecStart` prefix characters (`-`, `@`, `+`, `!`) are not
/// unwrapped: they are nothing this app writes, and failing to match one
/// simply falls back to generating a unit, which is the safe direction.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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

/// Render the systemd user unit for the given executable path.
///
/// `Restart=on-failure` mirrors the macOS `KeepAlive=SuccessfulExit:false`
/// semantics: the agent is respawned after a crash but a clean `exit(0)` (e.g.
/// the tray's Quit) stays stopped until the next login.
#[cfg(target_os = "linux")]
fn render_unit(exe: &str) -> String {
    render_unit_with_exec(&escape_systemd_exec(exe))
}

/// Render the unit around an `ExecStart` value that is **already escaped**.
///
/// Split out so [`is_generated_unit`] can splice a file's own `ExecStart` line
/// back in verbatim: running [`escape_systemd_exec`] over an already-escaped
/// value would double `%%` into `%%%%`, and a unit this app wrote would fail to
/// match itself.
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
struct SystemctlArgsDisplay<'a, 'b>(&'a [&'b str]);

#[cfg(target_os = "linux")]
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg(target_os = "macos")]
mod macos_tests;

#[cfg(test)]
#[cfg(target_os = "linux")]
mod linux_tests;
