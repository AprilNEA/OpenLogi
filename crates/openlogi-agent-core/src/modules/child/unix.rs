//! The Unix side of a run: a module leads its own process group, and ending
//! the run ends the group.

use tokio::process::{Child, Command};

/// Make the module the leader of a process group of its own.
pub(super) fn prepare(command: &mut Command) {
    command.process_group(0);
}

/// The module's process group, which bears the module's id.
pub(super) struct Group(Option<libc::pid_t>);

impl Group {
    pub(super) fn of(child: &Child) -> Self {
        Self(child.id().and_then(|pid| libc::pid_t::try_from(pid).ok()))
    }

    /// Kill the module and every process left in its group. Called right
    /// after the module is reaped as well as before: children it left
    /// behind hold the group, and with it the id, until they are gone.
    pub(super) fn kill(&self, child: &mut Child) {
        if let Some(group) = self.0 {
            #[expect(
                unsafe_code,
                reason = "killpg is the one call that signals a whole process group"
            )]
            // SAFETY: killpg only sends a signal. `group` is the group the
            // module leads (`process_group(0)`). Its id cannot name another
            // group while the module is unreaped or any process is left in
            // its group; once both are gone the signal finds no group,
            // unless the system has handed out every other process id since
            // and the new holder of this one leads a group, which the signal
            // sent at once after the reap gives no time for.
            unsafe {
                libc::killpg(group, libc::SIGKILL);
            }
        }
        let _ = child.start_kill();
    }
}
