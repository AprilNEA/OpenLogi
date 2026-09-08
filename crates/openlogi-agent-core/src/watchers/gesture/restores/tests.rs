use super::*;
use crate::capture_plan::plan_for_device;
use openlogi_core::config::Config;
use std::time::Duration;

fn key(unit: &str) -> PhysicalDeviceKey {
    PhysicalDeviceKey::parse(unit).expect("nonzero unit identity")
}

fn bluetooth() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: 0x046d,
        product_id: 0xb027,
    }
}

fn receiver() -> DeviceRoute {
    DeviceRoute::Unifying {
        receiver_uid: "receiver-a".into(),
        slot: 3,
    }
}

fn plan(key: &PhysicalDeviceKey, route: DeviceRoute) -> DeviceCapturePlan {
    plan_for_device(
        &Config::default(),
        key.clone(),
        key.as_str(),
        route,
        None,
        0,
        true,
    )
}

fn debt(key: &PhysicalDeviceKey, route: DeviceRoute, token: u8) -> PendingRestore<u8> {
    PendingRestore {
        key: key.clone(),
        route,
        token,
        retry_at: Instant::now(),
    }
}

#[test]
fn transport_switch_parks_old_cleanup_and_restores_it_before_return() {
    let bolt = DeviceRoute::Bolt {
        receiver_uid: "bolt-receiver".into(),
        slot: 2,
    };
    for (old, new) in [
        (bluetooth(), receiver()),
        (receiver(), bluetooth()),
        (bluetooth(), bolt.clone()),
        (bolt, bluetooth()),
    ] {
        let mouse = key("unit:2916dbbe");
        let mut queue = RestoreQueue::default();
        queue.retain(debt(&mouse, old.clone(), 7));
        let published = vec![plan(&mouse, new.clone())];
        let idle = HashSet::new();

        assert!(
            !queue.blocks(&mouse, &new),
            "an unavailable old transport must not disable capture on the new one"
        );
        assert!(queue.take_due(Instant::now(), &idle, &published).is_empty());
        assert_eq!(queue.next_deadline(&idle, &published), None);
        assert!(!queue.is_empty(), "the old teardown debt must not be lost");

        let returning = vec![plan(&mouse, old.clone())];
        assert!(queue.blocks(&mouse, &old));
        let pending = queue.take_due(Instant::now(), &idle, &returning);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].token, 7);
        assert_eq!(pending[0].route, old);
        assert!(queue.is_empty());
    }
}

#[test]
fn active_or_draining_successor_excludes_cleanup_even_after_plan_removal() {
    let mouse = key("unit:2916dbbe");
    let mut queue = RestoreQueue::default();
    queue.retain(debt(&mouse, bluetooth(), 1));
    let busy = HashSet::from([mouse.clone()]);
    queue.expedite(Instant::now());

    assert!(queue.take_due(Instant::now(), &busy, &[]).is_empty());
    assert_eq!(queue.next_deadline(&busy, &[]), None);
    assert!(!queue.is_empty());

    // Only ordered session completion releases the physical-device exclusion.
    let pending = queue.take_due(Instant::now(), &HashSet::new(), &[]);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].token, 1);
}

#[test]
fn failed_cleanup_keeps_each_routes_token_and_retry_deadline() {
    let mouse = key("unit:2916dbbe");
    let mut queue = RestoreQueue::default();
    queue.retain(debt(&mouse, bluetooth(), 1));
    queue.retain(debt(&mouse, receiver(), 2));
    let published = vec![plan(&mouse, receiver())];
    let idle = HashSet::new();
    let now = Instant::now();

    let mut due = queue.take_due(now, &idle, &published);
    assert_eq!(due.len(), 1);
    let mut failed = due.pop().expect("receiver restore should be due");
    assert_eq!(failed.token, 2);
    failed.retry_at = now + Duration::from_secs(1);
    queue.retain(failed);

    assert!(queue.blocks(&mouse, &receiver()));
    assert!(queue.blocks(&mouse, &bluetooth()));
    assert!(queue.take_due(now, &idle, &published).is_empty());
    assert_eq!(
        queue.next_deadline(&idle, &published),
        Some(now + Duration::from_secs(1))
    );
    let restored = queue.take_due(now + Duration::from_secs(1), &idle, &published);
    assert_eq!(restored[0].token, 2);
    assert!(!queue.blocks(&mouse, &receiver()));

    let returned = vec![plan(&mouse, bluetooth())];
    assert_eq!(queue.take_due(now, &idle, &returned)[0].token, 1);
}

#[test]
fn receiver_route_reassigned_to_another_mouse_must_not_receive_stale_cleanup() {
    let first = key("unit:2916dbbe");
    let second = key("unit:2916dbbf");
    let mut queue = RestoreQueue::default();
    queue.retain(debt(&first, receiver(), 1));
    let published = vec![plan(&second, receiver())];

    assert!(!queue.blocks(&second, &receiver()));
    assert!(
        queue
            .take_due(Instant::now(), &HashSet::new(), &published)
            .is_empty()
    );
    assert_eq!(queue.next_deadline(&HashSet::new(), &published), None);
    assert!(!queue.is_empty());
}

#[test]
fn another_mouse_does_not_block_due_cleanup() {
    let first = key("unit:2916dbbe");
    let second = key("unit:2916dbbf");
    let mut queue = RestoreQueue::default();
    queue.retain(debt(&first, receiver(), 1));
    let published = vec![plan(&first, receiver()), plan(&second, bluetooth())];
    let busy = HashSet::from([second]);

    assert!(queue.next_deadline(&busy, &published).is_some());
    assert_eq!(
        queue.take_due(Instant::now(), &busy, &published)[0].token,
        1
    );
    assert!(queue.is_empty());
}
