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

impl ProcessTree {
    /// Start `command` with every process it starts held in one tree.
    ///
    /// A command whose tree cannot be held is not left running: without an
    /// owner for its children, a timeout could end only the shell, and a
    /// child it put in the background would keep running — and keep the
    /// output pipe open. It is stopped and reaped, and the error returned.
    pub(super) fn spawn(command: &mut Command) -> std::io::Result<(Child, Self)> {
        Self::prepare(command);
        Self::spawn_held(command, Self::hold)
    }

    /// Spawn `command` and hold its tree with `hold`, stopping and reaping
    /// the child when `hold` fails.
    fn spawn_held(
        command: &mut Command,
        hold: impl FnOnce(&Child) -> std::io::Result<Self>,
    ) -> std::io::Result<(Child, Self)> {
        let mut child = command.spawn()?;
        match hold(&child) {
            Ok(tree) => Ok((child, tree)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }
}

#[cfg(unix)]
impl ProcessTree {
    /// Start the command in a process group of its own.
    fn prepare(command: &mut Command) {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }

    /// Hold the child's process group: the shell leads it, so the group id
    /// is its pid.
    fn hold(child: &Child) -> std::io::Result<Self> {
        i32::try_from(child.id())
            .map(|group| Self { group })
            .map_err(|_| std::io::Error::other("the shell's pid is not a process group id"))
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
    /// Nothing to set before the spawn: the job is assigned after it.
    fn prepare(_command: &mut Command) {}

    /// Put the child in a job object that ends every process in it when the
    /// job's last handle closes.
    #[expect(
        unsafe_code,
        reason = "a job object is the Win32 way to end a process together with its children"
    )]
    fn hold(child: &Child) -> std::io::Result<Self> {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        // SAFETY: null attributes and name are documented as valid; the
        // handle is checked before use and owned by the returned tree.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
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
            let error = std::io::Error::last_os_error();
            // SAFETY: `job` is a handle this function opened and nothing
            // else holds.
            unsafe { CloseHandle(job) };
            return Err(error);
        }
        Ok(Self { job })
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

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    /// Whether a process with `pid` still exists, as `kill -0` sees it.
    fn alive(pid: u32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// A command whose tree cannot be held is stopped and reaped, not left
    /// running without an owner for its children — the path a failed job
    /// setup takes on Windows.
    #[test]
    fn shell_output_job_failure_leaves_no_child() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let mut pid = None;

        let spawned = ProcessTree::spawn_held(&mut command, |child| {
            pid = Some(child.id());
            Err(std::io::Error::other("job setup failed"))
        });

        let error = spawned
            .err()
            .expect("a tree that cannot be held is an error");
        assert_eq!(error.to_string(), "job setup failed");
        let pid = pid.expect("the command was started");
        assert!(!alive(pid), "the unheld command was stopped and reaped");
    }
}
