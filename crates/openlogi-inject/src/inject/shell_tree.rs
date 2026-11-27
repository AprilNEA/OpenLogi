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
    /// Start the shell suspended. The job can only be assigned once the
    /// process exists, and a shell already running by then could start a
    /// child outside it; suspended, it runs nothing until [`Self::hold`] has
    /// put it in the job, which every process it starts then inherits.
    fn prepare(command: &mut Command) {
        use std::os::windows::process::CommandExt as _;
        use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
        command.creation_flags(super::platform::SHELL_CREATION_FLAGS | CREATE_SUSPENDED);
    }

    /// Put the suspended child in a job object that ends every process in it
    /// when the job's last handle closes, then let it run.
    #[expect(
        unsafe_code,
        reason = "a job object is the Win32 way to end a process together with its children"
    )]
    fn hold(child: &Child) -> std::io::Result<Self> {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        // SAFETY: null attributes and name are documented as valid; the
        // handle is checked before use.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // The tree owns the job from here: an early return closes it.
        let tree = Self { job };
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
            return Err(std::io::Error::last_os_error());
        }
        Self::resume(child.id())?;
        Ok(tree)
    }

    /// Let the suspended process `pid` run: resume its threads — a process
    /// started suspended has only its main one.
    ///
    /// `ChildExt::main_thread_handle` would hand that thread over directly,
    /// but it is newer than the workspace MSRV; the thread snapshot finds it
    /// by its owning process instead.
    #[expect(
        unsafe_code,
        reason = "a thread snapshot and ResumeThread are the Win32 way to start a suspended process's thread"
    )]
    fn resume(pid: u32) -> std::io::Result<()> {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        };
        use windows_sys::Win32::System::Threading::{
            OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
        };

        // SAFETY: a thread snapshot takes no pointers; the handle is checked
        // before use and closed below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let mut entry = THREADENTRY32 {
            dwSize: u32::try_from(std::mem::size_of::<THREADENTRY32>()).unwrap_or(u32::MAX),
            ..THREADENTRY32::default()
        };
        let mut resumed = false;
        // SAFETY: `snapshot` is a live thread snapshot and `entry` a
        // THREADENTRY32 whose `dwSize` is its own size, as both calls require.
        let mut more = unsafe { Thread32First(snapshot, &raw mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: plain values in; the handle is checked before use
                // and closed right after.
                unsafe {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if !thread.is_null() {
                        resumed |= ResumeThread(thread) != u32::MAX;
                        CloseHandle(thread);
                    }
                }
            }
            // SAFETY: as for `Thread32First` above.
            more = unsafe { Thread32Next(snapshot, &raw mut entry) } != 0;
        }
        // SAFETY: `snapshot` is a handle this function opened and nothing
        // else holds.
        unsafe { CloseHandle(snapshot) };
        if resumed {
            Ok(())
        } else {
            Err(std::io::Error::other(
                "the shell's thread could not be resumed",
            ))
        }
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

#[cfg(test)]
#[cfg(windows)]
mod windows_tests {
    use std::time::Duration;

    use super::*;

    /// The shell runs nothing before its job holds it, so no child it starts
    /// can land outside the job; once held, it runs to the end.
    #[test]
    fn shell_children_are_owned_before_execution() {
        let marker = std::env::temp_dir().join(format!(
            "openlogi-shell-tree-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&marker);
        let mut command =
            super::super::platform::shell_command(&format!("echo ran> \"{}\"", marker.display()));
        ProcessTree::prepare(&mut command);
        let mut ran_before_hold = None;

        let spawned = ProcessTree::spawn_held(&mut command, |child| {
            // Long enough for an unsuspended `cmd.exe` to have written it.
            std::thread::sleep(Duration::from_millis(500));
            ran_before_hold = Some(marker.exists());
            ProcessTree::hold(child)
        });
        if ran_before_hold != Some(false) {
            let _ = std::fs::remove_file(&marker);
        }
        assert_eq!(
            ran_before_hold,
            Some(false),
            "the shell ran before its job held it"
        );
        let (mut child, tree) = spawned.expect("the shell starts and its job holds it");
        let status = child.wait().expect("the held shell ends");
        drop(tree);
        let ran = marker.exists();
        let _ = std::fs::remove_file(&marker);

        assert!(status.success(), "{status}");
        assert!(ran, "the held shell resumed and ran its command");
    }
}
