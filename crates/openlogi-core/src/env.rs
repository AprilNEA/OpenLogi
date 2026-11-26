//! The environment variables OpenLogi reads at runtime.
//!
//! Named here once because more than one place reads them — the agent, the
//! GUI, the overlay, the CLI, and the mock agent — and a knob spelled
//! differently in one of them is a knob that silently does nothing there.
//! Build-time knobs (`option_env!` in the updater) and the packaging overrides
//! `xtask` reads belong to the one place that reads them.

/// Forces the profile a process runs under, `dev` or `prod`; see
/// [`Profile`](crate::paths::Profile) for what that decides. Unset, the
/// profile is detected from the bundle the executable lives in.
pub const PROFILE: &str = "OPENLOGI_PROFILE";

/// The `tracing` filter every process installs, in `EnvFilter` directive
/// syntax (`hidpp=trace`, `openlogi_agent=debug,info`, …).
pub const LOG: &str = "OPENLOGI_LOG";

/// What the agent, the CLI, and the mock agent log when [`LOG`] is unset or
/// does not parse. tarpc opens an `info` span per RPC and warns whenever a
/// peer drops its connection — every GUI or overlay quit — so only its errors
/// get through.
pub const LOG_DEFAULT: &str = "info,tarpc=error";

/// What the GUI and the overlay log when [`LOG`] is unset or does not parse:
/// only what went wrong. Their log files are read when a bug report asks for
/// them, and a line per window or IPC event would bury the one that matters.
pub const LOG_DEFAULT_QUIET: &str = "warn,tarpc=error";

/// An asset mirror that overrides the configured source, for the GUI's asset
/// sync and the CLI's `assets sync`.
pub const ASSETS: &str = "OPENLOGI_ASSETS";

/// Pins the GUI's device renders to one depot regardless of what is attached
/// — a development convenience read in two places.
pub const FORCE_DEPOT: &str = "OPENLOGI_FORCE_DEPOT";
