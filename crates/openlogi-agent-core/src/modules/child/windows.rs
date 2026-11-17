//! The Windows side of a run: the module's own process is all that is
//! ended.

use tokio::process::{Child, Command};

/// Start the module without a console window: a console module started by
/// the windowless agent would otherwise open one of its own.
pub(super) fn prepare(command: &mut Command) {
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
}

/// The module's own process: Windows has no process group to end with it.
pub(super) struct Group;

impl Group {
    pub(super) fn of(_child: &Child) -> Self {
        Self
    }

    /// Kill the module.
    #[expect(
        clippy::unused_self,
        reason = "the same call as Unix's, whose group is state; here there is none"
    )]
    pub(super) fn kill(&self, child: &mut Child) {
        let _ = child.start_kill();
    }
}
