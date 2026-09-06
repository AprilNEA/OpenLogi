//! Advisory locks shared by every OpenLogi process on the host.
//!
//! Every process that opens a HID node receives every input report on it —
//! macOS, Windows and Linux all fan reports out to every open handle — so two
//! OpenLogi processes talking to one device at the same time, the agent and a
//! CLI or GUI run, take each other's replies. Two things therefore need
//! arbitration across processes, not just across the channels of one: HID++
//! software ids, leased per channel by the HID transport, and receiver
//! register access, serialised per node by the inventory probe because HID++
//! 1.0 register replies carry no software id at all. Both are exclusive OS
//! file locks in one directory. The OS releases a lock with its holder, so a
//! crashed process cannot strand one.

use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io,
    path::PathBuf,
    time::{Duration, Instant},
};

use tracing::debug;

use crate::backend::NodeId;

/// How often [`lock_within`] re-tries a held lock.
const POLL: Duration = Duration::from_millis(20);

/// Directory of the lock files.
///
/// The temp directory rather than a profile directory on purpose: a
/// dev-profile agent and a release CLI open the same HID node, so they must
/// meet at one path. It is per user on macOS and Windows and per host on
/// Linux; a file another user created is opened read-only, which still
/// carries the lock.
#[must_use]
pub fn lock_dir() -> PathBuf {
    std::env::temp_dir().join("openlogi-locks")
}

/// An exclusive host-wide lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct HostLock {
    /// Held only to be dropped: closing the file releases the lock.
    _file: File,
}

/// Try to take the exclusive lock `name` without waiting.
///
/// `Ok(None)` when another holder has it — another process, or another taker
/// in this one: the lock belongs to the open file description, so two takers
/// in one process exclude each other as well. `Err` when the lock directory
/// is unusable; callers fall back to whatever they did before locks existed.
pub fn try_lock(name: &str) -> io::Result<Option<HostLock>> {
    let dir = lock_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => File::open(&path)?,
        Err(error) => return Err(error),
    };
    match file.try_lock() {
        Ok(()) => Ok(Some(HostLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// Take the exclusive lock `name`, re-trying for at most `budget`.
///
/// `None` when the lock stayed held past the budget or the directory is
/// unusable. Bounded on purpose: a caller that cannot get the lock proceeds
/// without it — the pre-lock behaviour — instead of hanging on a holder that
/// is itself stuck.
pub async fn lock_within(name: &str, budget: Duration) -> Option<HostLock> {
    let deadline = Instant::now() + budget;
    loop {
        match try_lock(name) {
            Ok(Some(lock)) => return Some(lock),
            Ok(None) if Instant::now() < deadline => tokio::time::sleep(POLL).await,
            Ok(None) => {
                debug!(
                    name,
                    ?budget,
                    "host lock still held past the budget — proceeding unlocked"
                );
                return None;
            }
            Err(error) => {
                debug!(name, %error, "host lock unusable — proceeding unlocked");
                return None;
            }
        }
    }
}

/// The lock name for one HID node: a stable hash of its backend identity, so
/// every process that enumerates the node arrives at the same file.
#[must_use]
pub fn node_lock_name(node: &NodeId) -> String {
    // FNV-1a: short, dependency-free, and stable across builds — which
    // `DefaultHasher` does not promise, and two different OpenLogi binaries
    // must agree on the name.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in node.to_string().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("node-{hash:016x}")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{lock_within, node_lock_name, try_lock};
    use crate::backend::NodeId;

    fn name(tag: &str) -> String {
        format!("test-{}-{tag}", std::process::id())
    }

    #[test]
    fn a_held_lock_excludes_a_second_taker_until_dropped() {
        let name = name("exclusive");
        let first = try_lock(&name)
            .unwrap()
            .expect("the first taker gets the lock");
        assert!(
            try_lock(&name).unwrap().is_none(),
            "a held lock must not be handed out twice"
        );
        drop(first);
        assert!(
            try_lock(&name).unwrap().is_some(),
            "dropping the lock releases it"
        );
    }

    #[tokio::test]
    async fn lock_within_waits_for_a_release_inside_its_budget() {
        let name = name("wait");
        let held = try_lock(&name)
            .unwrap()
            .expect("the first taker gets the lock");
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(60)).await;
            drop(held);
        });
        assert!(
            lock_within(&name, Duration::from_secs(2)).await.is_some(),
            "a lock released inside the budget is taken"
        );
        release.await.unwrap();
    }

    #[tokio::test]
    async fn lock_within_gives_up_after_its_budget() {
        let name = name("give-up");
        let _held = try_lock(&name)
            .unwrap()
            .expect("the first taker gets the lock");
        assert!(
            lock_within(&name, Duration::from_millis(50))
                .await
                .is_none(),
            "a lock held past the budget is not taken"
        );
    }

    #[test]
    fn node_lock_names_are_stable_and_distinct() {
        let a = NodeId::from("RegistryEntryId(4295012345)".to_string());
        let same = NodeId::from("RegistryEntryId(4295012345)".to_string());
        let b = NodeId::from("RegistryEntryId(4295012346)".to_string());
        assert_eq!(node_lock_name(&a), node_lock_name(&same));
        assert_ne!(node_lock_name(&a), node_lock_name(&b));
    }
}
