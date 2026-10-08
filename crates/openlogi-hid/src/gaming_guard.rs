//! Shared host policy for exclusive onboard-memory mutations.

/// Whether this caller already owns the agent's device transaction lease.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Writer {
    /// The agent serializes its own hardware operations.
    Agent,
    /// A direct diagnostic must also exclude the running agent.
    Direct,
}

#[cfg(any(target_os = "windows", test))]
fn conflicts(name: &str, writer: Writer) -> bool {
    let name = name.to_ascii_lowercase();
    // The update service remains running after G HUB exits and does not own
    // its settings session. The UI/agent processes must be closed.
    (name.starts_with("lghub") && !name.starts_with("lghub_updater"))
        || name.starts_with("logioptions")
        || (writer == Writer::Direct && name == "openlogi-agent.exe")
}

/// Reject competing settings applications before an explicit hardware mutation.
/// This is a best-effort process check, not an OS-wide lock against future opens.
pub fn ensure_exclusive(writer: Writer) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let mut system = sysinfo::System::new();
        system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let names: Vec<_> = system
            .processes()
            .values()
            .map(|p| p.name().to_string_lossy().into_owned())
            .filter(|name| conflicts(name, writer))
            .collect();
        if names.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Close competing device managers before writing: {}",
                names.join(", ")
            ))
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = writer;
        Err("Onboard writes are enabled only on Windows in this build".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn both_callers_exclude_vendor_writers_but_only_direct_excludes_agent() {
        for name in ["LGHUB.EXE", "lghub_agent.exe", "logioptionsplus_agent.exe"] {
            assert!(conflicts(name, Writer::Agent));
            assert!(conflicts(name, Writer::Direct));
        }
        assert!(!conflicts("lghub_updater.exe", Writer::Agent));
        assert!(!conflicts("openlogi-agent.exe", Writer::Agent));
        assert!(conflicts("openlogi-agent.exe", Writer::Direct));
    }
}
