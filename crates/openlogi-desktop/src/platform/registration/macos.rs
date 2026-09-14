//! The macOS implementation: `SMAppService` over `objc2-service-management`,
//! plus the version marker that drives re-registration after an app update.

use super::ServiceStatus;

/// The launchd service label this process manages: the dev variant inside a
/// dev-profile bundle, so a dev registration can never collide with the
/// shipped one.
#[must_use]
pub fn agent_service_label() -> String {
    if openlogi_core::paths::is_dev_profile() {
        openlogi_core::brand::dev_id(openlogi_core::brand::AGENT_SERVICE_LABEL)
    } else {
        openlogi_core::brand::AGENT_SERVICE_LABEL.to_owned()
    }
}

pub(super) fn status() -> ServiceStatus {
    backend::status()
}

/// What [`ensure_registered`] should do, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnsureAction {
    /// The service is absent — register it.
    Register,
    /// The service is registered but a different executable registered it —
    /// unregister-then-register, the dance Apple requires after an update.
    Reregister,
}

/// The pure convergence rule behind [`ensure_registered`] (which is what the
/// tests below pin down).
///
/// - Absent (`NotRegistered`) → register. `NotFound` also attempts it, so a
///   broken bundle surfaces an informative framework error instead of
///   silence.
/// - `Enabled` with a stale version marker → re-register.
/// - `RequiresApproval` → nothing, ever: the user's System Settings choice
///   outranks the update path too.
fn ensure_action(status: ServiceStatus, stale: bool) -> Option<EnsureAction> {
    match status {
        ServiceStatus::NotRegistered | ServiceStatus::NotFound => Some(EnsureAction::Register),
        ServiceStatus::Enabled if stale => Some(EnsureAction::Reregister),
        ServiceStatus::Enabled | ServiceStatus::RequiresApproval => None,
    }
}

pub(super) fn ensure_registered() -> Result<(), String> {
    match ensure_action(backend::status(), registration_is_stale()) {
        Some(EnsureAction::Register) => {
            backend::register()?;
            tracing::info!("registered the agent service with launchd");
        }
        Some(EnsureAction::Reregister) => {
            backend::unregister()?;
            backend::register()?;
            tracing::info!("re-registered the agent service (executable changed)");
        }
        None => return Ok(()),
    }
    record_registered_version();
    Ok(())
}

/// Whether the recorded registering version differs from this build. A
/// missing marker reads as stale, so installs that registered before the
/// marker existed get their one catch-up re-registration.
fn registration_is_stale() -> bool {
    registered_version_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_none_or(|recorded| recorded.trim() != env!("CARGO_PKG_VERSION"))
}

/// Marker file under the data dir recording which app version last
/// registered the service.
fn registered_version_path() -> Option<std::path::PathBuf> {
    openlogi_core::paths::data_dir()
        .ok()
        .map(|dir| dir.join("registration-version"))
}

fn record_registered_version() {
    let Some(path) = registered_version_path() else {
        return;
    };
    let write = || -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, env!("CARGO_PKG_VERSION"))
    };
    if let Err(error) = write() {
        // Worst case the next launch re-registers once more.
        tracing::warn!(%error, "could not record the service registration version");
    }
}

#[expect(
    unsafe_code,
    reason = "plain no-argument ObjC class method via objc2 bindings"
)]
pub(super) fn open_login_items_settings() {
    // SAFETY: plain ObjC class method with no arguments.
    unsafe {
        objc2_service_management::SMAppService::openSystemSettingsLoginItems();
    }
}

/// The raw `SMAppService` calls, one place per operation, with the benign
/// already-converged error code forgiven where it means success.
mod backend {
    use objc2::rc::Retained;
    use objc2_foundation::{NSError, NSString};
    use objc2_service_management::SMAppService;

    use super::{ServiceStatus, agent_service_label};

    /// The domain `SMAppService` reports its errors in.
    ///
    /// Spelled out on purpose. The framework has used this string since
    /// macOS 13 but exports it as the `SMAppServiceErrorDomain` symbol only
    /// from macOS 15, and a binary that imports that symbol is refused by
    /// dyld on 13 and 14 before `main` runs (#1279). Rust has no availability
    /// checking to catch that, so the constant must never be linked here.
    const ERROR_DOMAIN: &str = "SMAppServiceErrorDomain";

    /// Recognize `SMAppService` errors without importing its macOS 15-only
    /// error-domain symbol.
    pub(super) trait SmAppServiceErrorExt {
        /// Match both the framework's domain and `expected`: the same small
        /// integers mean something else as POSIX or OSStatus codes.
        fn is_sm_app_service_error(&self, expected: core::ffi::c_uint) -> bool;
    }

    impl SmAppServiceErrorExt for NSError {
        fn is_sm_app_service_error(&self, expected: core::ffi::c_uint) -> bool {
            self.domain().to_string() == ERROR_DOMAIN
                && isize::try_from(expected).is_ok_and(|expected| self.code() == expected)
        }
    }

    /// The framework handle for the agent service's embedded plist.
    #[expect(unsafe_code, reason = "plain ObjC class method via objc2 bindings")]
    fn service() -> Retained<SMAppService> {
        let plist_name = NSString::from_str(&format!("{}.plist", agent_service_label()));
        // SAFETY: plain ObjC class method; the name is a valid NSString.
        unsafe { SMAppService::agentServiceWithPlistName(&plist_name) }
    }

    #[expect(unsafe_code, reason = "plain ObjC property read via objc2 bindings")]
    pub(super) fn status() -> ServiceStatus {
        use objc2_service_management::SMAppServiceStatus;
        // SAFETY: plain ObjC property read on a handle this process owns.
        let status = unsafe { service().status() };
        match status {
            SMAppServiceStatus::Enabled => ServiceStatus::Enabled,
            SMAppServiceStatus::RequiresApproval => ServiceStatus::RequiresApproval,
            SMAppServiceStatus::NotFound => ServiceStatus::NotFound,
            // NotRegistered, and any future framework value: nothing is
            // registered that we could rely on.
            _ => ServiceStatus::NotRegistered,
        }
    }

    /// Register the service; an existing registration is success.
    #[expect(unsafe_code, reason = "plain ObjC call via objc2 bindings")]
    pub(super) fn register() -> Result<(), String> {
        use objc2_service_management::kSMErrorAlreadyRegistered;
        // SAFETY: plain ObjC call; the returned NSError is a managed
        // `Retained`.
        let result = unsafe { service().registerAndReturnError() };
        forgive(result, kSMErrorAlreadyRegistered)
    }

    /// Unregister the service; an absent registration is success.
    #[expect(unsafe_code, reason = "plain ObjC call via objc2 bindings")]
    pub(super) fn unregister() -> Result<(), String> {
        use objc2_service_management::kSMErrorJobNotFound;
        // SAFETY: plain ObjC call; the returned NSError is a managed
        // `Retained`.
        let result = unsafe { service().unregisterAndReturnError() };
        forgive(result, kSMErrorJobNotFound)
    }

    /// Treat exactly one framework error code — the "already in the desired
    /// state" one for the operation — as success. Matched by the framework's
    /// own constants, never bare ints.
    fn forgive(
        result: Result<(), Retained<NSError>>,
        benign: core::ffi::c_uint,
    ) -> Result<(), String> {
        result.or_else(|error| {
            if error.is_sm_app_service_error(benign) {
                Ok(())
            } else {
                Err(error.localizedDescription().to_string())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_service_is_registered() {
        assert_eq!(
            ensure_action(ServiceStatus::NotRegistered, false),
            Some(EnsureAction::Register)
        );
        // A fresh install has no marker, which reads as stale — that must
        // still be a plain register, not an unregister dance.
        assert_eq!(
            ensure_action(ServiceStatus::NotRegistered, true),
            Some(EnsureAction::Register)
        );
    }

    #[test]
    fn a_missing_plist_still_attempts_registration() {
        // NotFound means a broken or bare bundle; attempting the register
        // surfaces an informative framework error instead of silence.
        assert_eq!(
            ensure_action(ServiceStatus::NotFound, false),
            Some(EnsureAction::Register)
        );
    }

    #[test]
    fn a_current_registration_is_left_alone() {
        assert_eq!(ensure_action(ServiceStatus::Enabled, false), None);
    }

    #[test]
    fn an_update_reregisters() {
        assert_eq!(
            ensure_action(ServiceStatus::Enabled, true),
            Some(EnsureAction::Reregister)
        );
    }

    #[test]
    fn a_system_settings_disable_is_never_overridden() {
        // Not on a normal launch, and not by the update path either: the
        // user's Login Items choice outranks both.
        assert_eq!(ensure_action(ServiceStatus::RequiresApproval, false), None);
        assert_eq!(ensure_action(ServiceStatus::RequiresApproval, true), None);
    }

    #[test]
    fn only_the_frameworks_own_error_domain_and_code_match() {
        use objc2_foundation::{NSError, NSString};
        use objc2_service_management::{kSMErrorAlreadyRegistered, kSMErrorJobNotFound};

        use super::backend::SmAppServiceErrorExt;

        for (domain, code, expected, matches) in [
            (
                "SMAppServiceErrorDomain",
                12,
                kSMErrorAlreadyRegistered,
                true,
            ),
            ("SMAppServiceErrorDomain", 6, kSMErrorJobNotFound, true),
            // The other operation's benign code is a real failure for this one.
            ("SMAppServiceErrorDomain", 12, kSMErrorJobNotFound, false),
            (
                "SMAppServiceErrorDomain",
                6,
                kSMErrorAlreadyRegistered,
                false,
            ),
            // ENOMEM is 12 too; POSIX/OSStatus errors must never match.
            ("NSPOSIXErrorDomain", 12, kSMErrorAlreadyRegistered, false),
            ("NSOSStatusErrorDomain", 6, kSMErrorJobNotFound, false),
            (
                "SMAppServiceErrorDomain",
                -1,
                kSMErrorAlreadyRegistered,
                false,
            ),
        ] {
            let error = NSError::new(code, &NSString::from_str(domain));
            assert_eq!(
                error.is_sm_app_service_error(expected),
                matches,
                "domain={domain}, code={code}, expected={expected}"
            );
        }
    }
}
