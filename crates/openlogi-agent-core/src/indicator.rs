//! First-delivery and display deadlines shared by transient keyboard indicators.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use openlogi_ipc::{BacklightObservation, Generation, LockKeyObservation, OBSERVE_HOLD};
use tokio::sync::watch;
use tokio::time::Instant;

pub(crate) const DISPLAY_LIFETIME: Duration = Duration::from_millis(1500);
// Bound stale hints if the helper never connects. This is a delivery allowance,
// not a measured upper bound on process startup.
const FIRST_DELIVERY_LIFETIME: Duration = Duration::from_secs(8);

pub(crate) trait Observation: Clone + Send + Sync + 'static {
    fn generation(&self) -> Generation;
    fn set_generation(&mut self, generation: Generation);
    fn is_showing(&self) -> bool;
    fn hide(&mut self);
}

impl Observation for BacklightObservation {
    fn generation(&self) -> Generation {
        self.generation
    }
    fn set_generation(&mut self, generation: Generation) {
        self.generation = generation;
    }
    fn is_showing(&self) -> bool {
        self.visible
    }
    fn hide(&mut self) {
        self.visible = false;
    }
}

impl Observation for LockKeyObservation {
    fn generation(&self) -> Generation {
        self.generation
    }
    fn set_generation(&mut self, generation: Generation) {
        self.generation = generation;
    }
    fn is_showing(&self) -> bool {
        self.shown.is_some()
    }
    fn hide(&mut self) {
        self.shown = None;
    }
}

struct Lifetime {
    generation: Generation,
    ends: Instant,
    delivered: bool,
}

/// Owns both the published indicator and its one-shot first-delivery allowance.
#[derive(Clone)]
pub(crate) struct Indicator<T> {
    published: watch::Sender<T>,
    lifetime: Arc<Mutex<Option<Lifetime>>>,
}

impl<T: Observation> Indicator<T> {
    pub(crate) fn new(initial: T) -> Self {
        Self {
            published: watch::Sender::new(initial),
            lifetime: Arc::default(),
        }
    }

    pub(crate) fn show(&self, mut observation: T) {
        let mut lifetime = self.lifetime();
        let generation = self.published.borrow().generation().wrapping_add(1);
        observation.set_generation(generation);
        *lifetime = observation.is_showing().then(|| Lifetime {
            generation,
            ends: Instant::now() + FIRST_DELIVERY_LIFETIME,
            delivered: false,
        });
        self.published.send_replace(observation);
        drop(lifetime);

        let indicator = self.clone();
        tokio::spawn(async move {
            loop {
                let Some(ends) = indicator.expire(generation) else {
                    return;
                };
                tokio::time::sleep_until(ends).await;
            }
        });
    }

    /// Passive subscribers can wake a helper without consuming its delivery.
    pub(crate) fn subscribe(&self) -> watch::Receiver<T> {
        self.published.subscribe()
    }

    pub(crate) async fn observe(&self, since: Generation) -> T {
        let mut receiver = self.subscribe();
        let changed = receiver.wait_for(|observation| observation.generation() != since);
        // Drop the watch borrow before taking the lifetime lock: publishers
        // take those locks in the opposite order.
        let _ = tokio::time::timeout(OBSERVE_HOLD, changed).await;
        let mut lifetime = self.lifetime();
        self.expire_locked(&mut lifetime);
        let observed = self.published.borrow().clone();
        if observed.generation() != since
            && let Some(lifetime) = lifetime.as_mut()
            && !lifetime.delivered
        {
            lifetime.delivered = true;
            lifetime.ends = Instant::now() + DISPLAY_LIFETIME;
            // The original pending timer may be later than the display's
            // deadline, so arm the earlier deadline too. Both recheck it.
            let indicator = self.clone();
            let generation = lifetime.generation;
            let ends = lifetime.ends;
            tokio::spawn(async move {
                tokio::time::sleep_until(ends).await;
                let _ = indicator.expire(generation);
            });
        }
        observed
    }

    fn expire(&self, generation: Generation) -> Option<Instant> {
        let mut lifetime = self.lifetime();
        if lifetime.as_ref()?.generation != generation {
            return None;
        }
        self.expire_locked(&mut lifetime);
        lifetime.as_ref().map(|lifetime| lifetime.ends)
    }

    fn expire_locked(&self, lifetime: &mut Option<Lifetime>) {
        if lifetime
            .as_ref()
            .is_some_and(|lifetime| lifetime.ends <= Instant::now())
        {
            *lifetime = None;
            self.published.send_modify(|observation| {
                observation.set_generation(observation.generation().wrapping_add(1));
                observation.hide();
            });
        }
    }

    fn lifetime(&self) -> MutexGuard<'_, Option<Lifetime>> {
        self.lifetime.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backlight() -> Indicator<BacklightObservation> {
        Indicator::new(BacklightObservation {
            generation: 0,
            current_level: 0,
            levels: 8,
            visible: false,
        })
    }

    fn show(indicator: &Indicator<BacklightObservation>, current_level: u8) {
        indicator.show(BacklightObservation {
            generation: 0,
            current_level,
            levels: 8,
            visible: true,
        });
    }

    async fn advance(duration: Duration) {
        tokio::task::yield_now().await;
        tokio::time::advance(duration).await;
        tokio::task::yield_now().await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_passive_subscriber_does_not_consume_first_delivery() {
        let indicator = backlight();
        let mut supervisor = indicator.subscribe();
        show(&indicator, 3);
        supervisor.changed().await.unwrap();
        assert!(supervisor.borrow_and_update().visible);
        advance(Duration::from_secs(2)).await;

        let shown = indicator.observe(0).await;
        assert!(shown.visible);
        advance(
            DISPLAY_LIFETIME
                .checked_sub(Duration::from_millis(1))
                .unwrap(),
        )
        .await;
        assert!(supervisor.borrow().visible, "a full display after delivery");
        advance(Duration::from_millis(1)).await;
        let hidden = indicator.observe(shown.generation).await;
        assert!(!hidden.visible);
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_observers_cannot_extend_a_delivered_hint() {
        let indicator = backlight();
        show(&indicator, 3);
        let shown = indicator.observe(0).await;
        advance(Duration::from_secs(1)).await;
        assert_eq!(indicator.observe(0).await, shown);
        advance(
            DISPLAY_LIFETIME
                .checked_sub(Duration::from_secs(1))
                .unwrap(),
        )
        .await;
        assert!(!indicator.observe(0).await.visible);
    }

    #[tokio::test(start_paused = true)]
    async fn an_undelivered_hint_expires_without_being_revived() {
        let indicator = backlight();
        show(&indicator, 3);
        advance(FIRST_DELIVERY_LIFETIME).await;
        assert!(!indicator.subscribe().borrow().visible);
        assert!(!indicator.observe(0).await.visible);
    }

    #[tokio::test(start_paused = true)]
    async fn the_pending_timer_cannot_end_a_late_delivered_hint() {
        let indicator = backlight();
        show(&indicator, 3);
        advance(
            FIRST_DELIVERY_LIFETIME
                .checked_sub(Duration::from_millis(500))
                .unwrap(),
        )
        .await;
        let shown = indicator.observe(0).await;
        assert!(shown.visible);
        advance(Duration::from_millis(500)).await;
        assert!(indicator.subscribe().borrow().visible);
        advance(
            DISPLAY_LIFETIME
                .checked_sub(Duration::from_millis(500))
                .unwrap(),
        )
        .await;
        assert!(!indicator.observe(shown.generation).await.visible);
    }

    #[tokio::test(start_paused = true)]
    async fn a_replaced_hints_timer_cannot_hide_its_pending_successor() {
        let indicator = backlight();
        show(&indicator, 3);
        advance(
            FIRST_DELIVERY_LIFETIME
                .checked_sub(Duration::from_secs(1))
                .unwrap(),
        )
        .await;
        show(&indicator, 4);
        advance(Duration::from_secs(1)).await;
        let shown = indicator.observe(0).await;
        assert!(shown.visible);
        assert_eq!(shown.current_level, 4);
    }
}
