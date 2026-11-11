//! A shell command's whole process tree, for [`super::shell_output`].
//!
//! Killing the shell alone leaves the processes it started running — a
//! pipeline's stages, a command it put in the background — and those keep
//! its output pipe open, so whoever reads the output waits until they end.
//! [`ProcessTree`] holds every process the shell starts and ends them all
//! when it is dropped: a timeout or a finished command then leaves nothing
//! behind that could hold the pipe or pile up.

use std::process::{Child, Command};

/// The processes one shell command started, ended together on drop.
pub(super) struct ProcessTree {
    #[cfg(unix)]
    group: i32,
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(unix)]
impl ProcessTree {
    /// Start `command` in a process group of its own and hold that group.
    pub(super) fn spawn(command: &mut Command) -> std::io::Result<(Child, Option<Self>)> {
        use std::os::unix::process::CommandExt as _;
        let child = command.process_group(0).spawn()?;
        // The shell leads its group, so the group id is its pid.
        let tree = i32::try_from(child.id()).ok().map(|group| Self { group });
        Ok((child, tree))
    }
}

#[cfg(unix)]
impl Drop for ProcessTree {
    #[expect(
        unsafe_code,
        reason = "kill(2) on the command's own process group is the only way to end its children"
    )]
    fn drop(&mut self) {
        // SAFETY: `kill` takes plain integers and touches no memory; a group
        // that has already ended answers ESRCH, which is ignored.
        unsafe {
            libc::kill(-self.group, libc::SIGKILL);
        }
    }
}

#[cfg(windows)]
impl ProcessTree {
    /// Start `command` and put it in a job object that ends every process
    /// in it when the job's last handle closes.
    #[expect(
        unsafe_code,
        reason = "a job object is the Win32 way to end a process together with its children"
    )]
    pub(super) fn spawn(command: &mut Command) -> std::io::Result<(Child, Option<Self>)> {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let child = command.spawn()?;
        // SAFETY: null attributes and name are documented as valid; the
        // handle is checked before use and owned by the returned tree.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Ok((child, None));
        }
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .unwrap_or(u32::MAX);
        // SAFETY: `info` is a live JOBOBJECT_EXTENDED_LIMIT_INFORMATION of
        // `size` bytes; `job` and the child's process handle are valid for
        // the duration of the calls.
        let held = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                size,
            ) != 0
                && AssignProcessToJobObject(job, child.as_raw_handle()) != 0
        };
        if !held {
            // SAFETY: `job` is a handle this function opened and nothing
            // else holds.
            unsafe { CloseHandle(job) };
            return Ok((child, None));
        }
        Ok((child, Some(Self { job })))
    }
}

#[cfg(windows)]
impl Drop for ProcessTree {
    #[expect(unsafe_code, reason = "closing the job object ends its processes")]
    fn drop(&mut self) {
        // SAFETY: the tree owns `job` and closes it exactly once.
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job);
        }
    }
}
