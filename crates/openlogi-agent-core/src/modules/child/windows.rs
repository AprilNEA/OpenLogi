//! The Windows side of a run: a module runs inside a job object that kills
//! every process in it when the run ends or the job's handle closes.

use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

use tokio::process::{Child, Command};
use tracing::warn;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};

/// Start the module without a console window: a console module started by
/// the windowless agent would otherwise open one of its own.
pub(super) fn prepare(command: &mut Command) {
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
}

/// The job object the module runs in, which its children join when they
/// start. `None` when the job could not be made; the module itself is still
/// ended.
pub(super) struct Group(Option<OwnedHandle>);

impl Group {
    pub(super) fn of(child: &Child) -> Self {
        let job = child.raw_handle().and_then(|process| {
            let job = kill_on_close_job()?;
            #[expect(unsafe_code, reason = "AssignProcessToJobObject is a Win32 call")]
            // SAFETY: both handles are open: `job` is owned here and
            // `process` belongs to the unreaped `child`.
            let assigned = unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } != 0;
            assigned.then_some(job)
        });
        if job.is_none() {
            warn!(error = %std::io::Error::last_os_error(), "module job object unavailable: its children may outlive it");
        }
        Self(job)
    }

    /// Kill the module and every process in its job.
    pub(super) fn kill(&self, child: &mut Child) {
        self.terminate();
        let _ = child.start_kill();
    }

    /// The module was just reaped: kill what it left in its job now.
    pub(super) fn reaped(&mut self) {
        self.terminate();
    }

    fn terminate(&self) {
        if let Some(job) = &self.0 {
            #[expect(unsafe_code, reason = "TerminateJobObject is a Win32 call")]
            // SAFETY: `job` is an open job handle owned by this group.
            unsafe {
                TerminateJobObject(job.as_raw_handle(), 1);
            }
        }
    }
}

/// A new job object that kills every process in it once its last handle
/// closes, so a run whose group is dropped leaves nothing behind. A process
/// the module creates with `CREATE_BREAKAWAY_FROM_JOB` is left out of it:
/// an app the module opens can outlive the module.
fn kill_on_close_job() -> Option<OwnedHandle> {
    #[expect(unsafe_code, reason = "CreateJobObjectW is a Win32 call")]
    // SAFETY: no security attributes and no name; a null result is checked
    // before the handle is owned.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() {
        return None;
    }
    #[expect(unsafe_code, reason = "taking ownership of a fresh Win32 handle")]
    // SAFETY: `raw` is a valid handle CreateJobObjectW just returned, owned
    // by nothing else.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
    let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).ok()?;
    #[expect(unsafe_code, reason = "SetInformationJobObject is a Win32 call")]
    // SAFETY: `limits` is a live JOBOBJECT_EXTENDED_LIMIT_INFORMATION of
    // `size` bytes, the structure JobObjectExtendedLimitInformation names.
    let set = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size,
        )
    };
    (set != 0).then_some(job)
}
