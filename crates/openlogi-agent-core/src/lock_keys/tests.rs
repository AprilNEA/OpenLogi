use std::time::Duration;

use openlogi_core::device::LockKey;
use openlogi_ipc::{LockKeyChange, OBSERVE_HOLD};

use super::{DISPLAY_LIFETIME, LockKeyIndicator};

const CAPS_ON: LockKeyChange = LockKeyChange {
    key: LockKey::CapsLock,
    engaged: true,
};

const CAPS_OFF: LockKeyChange = LockKeyChange {
    key: LockKey::CapsLock,
    engaged: false,
};

/// Let the expiry task spawned by `show` reach its sleep before time moves.
async fn settle() {
    tokio::task::yield_now().await;
}

#[tokio::test(start_paused = true)]
async fn the_first_observation_answers_at_once_with_nothing_shown() {
    let indicator = LockKeyIndicator::default();
    let asked = tokio::time::Instant::now();

    let first = indicator.observe(0).await;

    assert_eq!(
        asked.elapsed(),
        Duration::ZERO,
        "no hold on a first observe"
    );
    assert_eq!(first.generation, 1);
    assert_eq!(first.shown, None);
}

#[tokio::test(start_paused = true)]
async fn a_change_shows_until_its_time_is_up_and_ends_in_its_language() {
    let indicator = LockKeyIndicator::default();
    indicator.show(CAPS_ON, Some("de".into()));
    settle().await;

    let shown = indicator.observe(1).await;
    assert_eq!(shown.shown, Some(CAPS_ON));
    assert_eq!(shown.language.as_deref(), Some("de"));

    let just_before = DISPLAY_LIFETIME
        .checked_sub(Duration::from_millis(1))
        .expect("the indicator shows for longer than a millisecond");
    tokio::time::advance(just_before).await;
    settle().await;
    assert_eq!(
        indicator.published.borrow().generation,
        shown.generation,
        "still showing just before its time is up"
    );

    tokio::time::advance(Duration::from_millis(1)).await;
    let ended = indicator.observe(shown.generation).await;
    assert_eq!(ended.shown, None);
    assert!(ended.generation > shown.generation);
    assert_eq!(
        ended.language.as_deref(),
        Some("de"),
        "the end keeps the language of what it ends"
    );
}

#[tokio::test(start_paused = true)]
async fn a_newer_change_replaces_the_shown_one_and_gets_its_own_time() {
    let indicator = LockKeyIndicator::default();
    indicator.show(CAPS_ON, None);
    settle().await;
    tokio::time::advance(Duration::from_secs(1)).await;

    indicator.show(CAPS_OFF, None);
    settle().await;
    let latest = indicator.observe(2).await;
    assert_eq!(latest.shown, Some(CAPS_OFF));

    // The first change's time is up here; it must not end the newer one.
    tokio::time::advance(Duration::from_millis(500)).await;
    settle().await;
    assert_eq!(indicator.published.borrow().generation, latest.generation);
    assert_eq!(indicator.published.borrow().shown, Some(CAPS_OFF));

    tokio::time::advance(Duration::from_secs(1)).await;
    let ended = indicator.observe(latest.generation).await;
    assert_eq!(ended.shown, None);
}

#[tokio::test(start_paused = true)]
async fn a_held_observe_is_answered_by_a_change_not_the_hold() {
    let indicator = LockKeyIndicator::default();
    let current = indicator.observe(0).await;
    let asked = tokio::time::Instant::now();
    let waiting = {
        let indicator = indicator.clone();
        tokio::spawn(async move { indicator.observe(current.generation).await })
    };
    settle().await;
    tokio::time::advance(Duration::from_secs(2)).await;

    indicator.show(CAPS_ON, None);
    let answer = waiting.await.expect("the observe task finished");

    assert_eq!(answer.shown, Some(CAPS_ON));
    assert!(asked.elapsed() < OBSERVE_HOLD, "woken by the change");
}

#[tokio::test(start_paused = true)]
async fn an_unchanged_indicator_is_answered_after_the_hold() {
    let indicator = LockKeyIndicator::default();
    let current = indicator.observe(0).await;
    let asked = tokio::time::Instant::now();

    let answer = indicator.observe(current.generation).await;

    assert_eq!(answer, current);
    assert_eq!(asked.elapsed(), OBSERVE_HOLD);
}
