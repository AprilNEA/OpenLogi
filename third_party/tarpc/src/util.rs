// Copyright 2018 Google LLC
//
// Use of this source code is governed by an MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT.

use std::{
    collections::HashMap,
    hash::{BuildHasher, Hash},
    time::{Duration, Instant},
};

#[cfg(feature = "serde1")]
#[cfg_attr(docsrs, doc(cfg(feature = "serde1")))]
pub mod serde;

/// Extension trait for [Instants](Instant) in the future, i.e. deadlines.
pub trait TimeUntil {
    /// How much time from now until this time is reached.
    fn time_until(&self) -> Duration;
}

impl TimeUntil for Instant {
    fn time_until(&self) -> Duration {
        self.duration_since(Instant::now())
    }
}

/// Collection compaction; configurable `shrink_to_fit`.
pub trait Compact {
    /// Compacts space if the ratio of length : capacity is less than `usage_ratio_threshold`.
    fn compact(&mut self, usage_ratio_threshold: f64);
}

impl<K, V, H> Compact for HashMap<K, V, H>
where
    K: Eq + Hash,
    H: BuildHasher,
{
    fn compact(&mut self, usage_ratio_threshold: f64) {
        let usage_ratio_threshold = usage_ratio_threshold.clamp(f64::MIN_POSITIVE, 1.);
        let cap = f64::max(1000., self.len() as f64 / usage_ratio_threshold);
        self.shrink_to(cap as usize);
    }
}

#[test]
fn test_compact() {
    let mut map = HashMap::with_capacity(2048);
    assert_eq!(map.capacity(), 3584);

    // Make usage ratio 25%
    for i in 0..896 {
        map.insert(format!("k{i}"), "v");
    }

    map.compact(-1.0);
    assert_eq!(map.capacity(), 3584);

    map.compact(0.25);
    assert_eq!(map.capacity(), 3584);

    map.compact(0.50);
    assert_eq!(map.capacity(), 1792);

    map.compact(1.0);
    assert_eq!(map.capacity(), 1792);

    map.compact(2.0);
    assert_eq!(map.capacity(), 1792);
}
