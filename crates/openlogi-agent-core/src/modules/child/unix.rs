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

    /// Kill the module and every process left in its group.
    pub(super) fn kill(&self, child: &mut Child) {
        if let Some(group) = self.0 {
            signal(group);
        }
        let _ = child.start_kill();
    }

    /// The module was just reaped: kill what it left in its group now, and
    /// never signal the group again.
    pub(super) fn reaped(&mut self) {
        if let Some(group) = self.0.take() {
            signal(group);
        }
    }
}

fn signal(group: libc::pid_t) {
    #[expect(
        unsafe_code,
        reason = "killpg is the one call that signals a whole process group"
    )]
    // SAFETY: killpg only sends a signal. `group` is the group the module
    // leads (`process_group(0)`), signalled only while the module is
    // unreaped or at once after its reap (`Group::reaped`, which signals it
    // no more). Until the reap the module holds the id; after it, any
    // process left in the group does, and with none left the signal finds
    // no group unless the system has handed out every other process id in
    // between.
    unsafe {
        libc::killpg(group, libc::SIGKILL);
    }
}
