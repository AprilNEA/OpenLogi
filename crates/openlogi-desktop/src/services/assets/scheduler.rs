//! When the background asset sync runs.
//!
//! The fetch itself lives in [`super::sync`]; this is the policy around it —
//! the automatic path's retry backoff and already-synced bookkeeping, plus the
//! Settings → Assets commands, which have to queue behind an in-flight fetch
//! rather than wipe the cache out from under its writes. Deliberately free of
//! GPUI and of the worker thread: the caller owns the I/O, so these rules can
//! be tested on their own.

use std::collections::HashSet;
use std::time::Instant;

use super::sync::{AssetCommand, AssetTarget, model_key, sync_retry_delay};

/// What a manual Refresh / Clear does right now.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ManualStep {
    /// Queued behind the fetch currently writing the cache. The
    /// [`AssetSync::finish`] that ends that fetch hands the command back.
    Deferred,
    /// Run it. `clear_cache` asks the caller to wipe the per-user cache (and
    /// rebuild its resolver) before fetching `targets`.
    Run {
        clear_cache: bool,
        targets: Vec<AssetTarget>,
    },
}

enum SyncState {
    Idle,
    Running {
        /// Model keys this fetch covers, folded into the synced set once it
        /// succeeds — tracked here rather than reported by the worker so the
        /// two can't disagree.
        keys: Vec<String>,
        deferred: Option<AssetCommand>,
    },
}

/// The background asset sync's bookkeeping, which outlives any one snapshot.
pub(crate) struct AssetSync {
    /// Whether the *automatic* path runs in this build at all (release bundles
    /// already ship the art). Manual commands ignore it.
    enabled: bool,
    state: SyncState,
    attempts: u32,
    last_at: Option<Instant>,
    index_refreshed: bool,
    synced_keys: HashSet<String>,
}

impl AssetSync {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            state: SyncState::Idle,
            attempts: 0,
            last_at: None,
            index_refreshed: false,
            synced_keys: HashSet::new(),
        }
    }

    /// Whether a fetch is writing the cache right now.
    pub(crate) fn is_running(&self) -> bool {
        matches!(self.state, SyncState::Running { .. })
    }

    /// The automatic path, offered every merged snapshot: fetch the index once
    /// and any model not yet synced this session.
    ///
    /// Returns the targets to fetch, or `None` when the automatic sync is off
    /// for this build or user, a fetch is already running, the retry backoff
    /// hasn't elapsed, or there is nothing left to ask for.
    pub(crate) fn poll_auto(
        &mut self,
        targets: Vec<AssetTarget>,
        auto_download: bool,
        now: Instant,
    ) -> Option<Vec<AssetTarget>> {
        if !auto_download || !self.enabled || self.is_running() {
            return None;
        }
        let backoff_passed = self
            .last_at
            .is_none_or(|t| now.duration_since(t) >= sync_retry_delay(self.attempts));
        if !backoff_passed {
            return None;
        }
        let pending: Vec<_> = targets
            .into_iter()
            .filter(|t| !self.synced_keys.contains(&model_key(t)))
            .collect();
        // The very first run fetches the index even with no devices, so
        // resolution works the moment one appears.
        if self.index_refreshed && pending.is_empty() {
            return None;
        }
        self.attempts = self.attempts.saturating_add(1);
        self.last_at = Some(now);
        self.start(&pending);
        Some(pending)
    }

    /// A manual Refresh / Clear from Settings → Assets. Both bypass the
    /// auto-download setting and the release-bundle gate, and both reset the
    /// retry backoff so a later automatic attempt isn't held off by a stale
    /// failure.
    pub(crate) fn command(&mut self, cmd: AssetCommand, targets: Vec<AssetTarget>) -> ManualStep {
        if let SyncState::Running { deferred, .. } = &mut self.state {
            // A queued Clear wins over a later Refresh — Clear re-fetches too,
            // so collapsing to Refresh would silently drop the wipe.
            if cmd == AssetCommand::ClearCache || *deferred != Some(AssetCommand::ClearCache) {
                *deferred = Some(cmd);
            }
            return ManualStep::Deferred;
        }
        let clear_cache = cmd == AssetCommand::ClearCache;
        if clear_cache {
            // The on-disk cache is about to go: drop the bookkeeping that says
            // otherwise, so a device that reconnects later re-fetches.
            self.synced_keys.clear();
            self.index_refreshed = false;
        }
        self.attempts = 0;
        self.last_at = None;
        self.start(&targets);
        ManualStep::Run {
            clear_cache,
            targets,
        }
    }

    /// A fetch finished. Returns the manual command that was waiting on it, to
    /// be re-issued now that the cache is no longer being written.
    pub(crate) fn finish(&mut self, ok: bool) -> Option<AssetCommand> {
        let SyncState::Running { keys, deferred } =
            std::mem::replace(&mut self.state, SyncState::Idle)
        else {
            return None;
        };
        if ok {
            // Success resets the backoff so a device appearing later syncs
            // immediately instead of waiting out a stale failure delay.
            self.attempts = 0;
            self.last_at = None;
            self.index_refreshed = true;
            self.synced_keys.extend(keys);
        }
        deferred
    }

    fn start(&mut self, targets: &[AssetTarget]) {
        self.state = SyncState::Running {
            keys: targets.iter().map(model_key).collect(),
            deferred: None,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{AssetSync, ManualStep};
    use crate::services::assets::sync::{AssetCommand, AssetTarget};
    use std::time::{Duration, Instant};

    fn target(id: &str) -> AssetTarget {
        AssetTarget::Standalone {
            registry_model_id: id.to_owned(),
        }
    }

    #[test]
    fn the_first_automatic_run_prefetches_the_index_with_no_devices() {
        let mut sync = AssetSync::new(true);
        assert_eq!(
            sync.poll_auto(Vec::new(), true, Instant::now()),
            Some(Vec::new())
        );
        assert!(sync.is_running());
    }

    #[test]
    fn a_model_already_synced_this_session_is_not_fetched_again() {
        let mut sync = AssetSync::new(true);
        let now = Instant::now();
        assert_eq!(
            sync.poll_auto(vec![target("a")], true, now),
            Some(vec![target("a")])
        );
        assert!(sync.finish(true).is_none());
        assert_eq!(sync.poll_auto(vec![target("a")], true, now), None);
        // A model that appears later still goes out.
        assert_eq!(
            sync.poll_auto(vec![target("a"), target("b")], true, now),
            Some(vec![target("b")])
        );
    }

    #[test]
    fn a_failed_fetch_holds_off_until_the_backoff_elapses() {
        let mut sync = AssetSync::new(true);
        let start = Instant::now();
        assert!(sync.poll_auto(vec![target("a")], true, start).is_some());
        assert!(sync.finish(false).is_none());
        // The model is still unsynced, but the first retry delay is 1s.
        assert_eq!(
            sync.poll_auto(vec![target("a")], true, start + Duration::from_millis(500)),
            None
        );
        assert!(
            sync.poll_auto(vec![target("a")], true, start + Duration::from_secs(1))
                .is_some()
        );
    }

    #[test]
    fn success_resets_the_backoff_so_the_next_device_syncs_at_once() {
        let mut sync = AssetSync::new(true);
        let start = Instant::now();
        assert!(sync.poll_auto(vec![target("a")], true, start).is_some());
        assert!(sync.finish(false).is_none());
        assert!(
            sync.poll_auto(vec![target("a")], true, start + Duration::from_secs(1))
                .is_some()
        );
        assert!(sync.finish(true).is_none());
        // Without the reset this would still be inside the doubled delay.
        assert_eq!(
            sync.poll_auto(vec![target("b")], true, start + Duration::from_secs(1)),
            Some(vec![target("b")])
        );
    }

    #[test]
    fn the_automatic_path_is_off_without_auto_download_but_commands_still_run() {
        let mut sync = AssetSync::new(true);
        assert_eq!(
            sync.poll_auto(vec![target("a")], false, Instant::now()),
            None
        );
        assert_eq!(
            sync.command(AssetCommand::Refresh, vec![target("a")]),
            ManualStep::Run {
                clear_cache: false,
                targets: vec![target("a")],
            }
        );
    }

    #[test]
    fn a_build_that_ships_its_art_still_honours_a_manual_refresh() {
        let mut sync = AssetSync::new(false);
        assert_eq!(
            sync.poll_auto(vec![target("a")], true, Instant::now()),
            None
        );
        assert!(matches!(
            sync.command(AssetCommand::Refresh, vec![target("a")]),
            ManualStep::Run { .. }
        ));
    }

    #[test]
    fn a_command_arriving_mid_fetch_waits_for_it() {
        let mut sync = AssetSync::new(true);
        assert!(sync.poll_auto(Vec::new(), true, Instant::now()).is_some());
        assert_eq!(
            sync.command(AssetCommand::Refresh, vec![target("a")]),
            ManualStep::Deferred
        );
        assert_eq!(sync.finish(true), Some(AssetCommand::Refresh));
    }

    #[test]
    fn a_queued_clear_outranks_a_refresh_either_way_round() {
        for order in [
            [AssetCommand::ClearCache, AssetCommand::Refresh],
            [AssetCommand::Refresh, AssetCommand::ClearCache],
        ] {
            let mut sync = AssetSync::new(true);
            assert!(sync.poll_auto(Vec::new(), true, Instant::now()).is_some());
            for cmd in order {
                assert_eq!(sync.command(cmd, Vec::new()), ManualStep::Deferred);
            }
            assert_eq!(sync.finish(true), Some(AssetCommand::ClearCache));
        }
    }

    #[test]
    fn a_failed_fetch_still_re_issues_the_command_that_waited_on_it() {
        let mut sync = AssetSync::new(true);
        assert!(sync.poll_auto(Vec::new(), true, Instant::now()).is_some());
        assert_eq!(
            sync.command(AssetCommand::ClearCache, Vec::new()),
            ManualStep::Deferred
        );
        assert_eq!(sync.finish(false), Some(AssetCommand::ClearCache));
    }

    #[test]
    fn clearing_the_cache_forgets_what_had_been_synced() {
        let mut sync = AssetSync::new(true);
        let now = Instant::now();
        assert!(sync.poll_auto(vec![target("a")], true, now).is_some());
        assert!(sync.finish(true).is_none());
        assert_eq!(sync.poll_auto(vec![target("a")], true, now), None);

        assert!(matches!(
            sync.command(AssetCommand::ClearCache, vec![target("a")]),
            ManualStep::Run {
                clear_cache: true,
                ..
            }
        ));
        // The wipe dropped the synced bookkeeping, so a re-fetch that fails
        // leaves the model owed rather than latched as done against a cache
        // that no longer holds it.
        assert!(sync.finish(false).is_none());
        assert_eq!(
            sync.poll_auto(vec![target("a")], true, now),
            Some(vec![target("a")])
        );
    }
}
