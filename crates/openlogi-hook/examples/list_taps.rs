fn main() {
    let taps = openlogi_hook::Hook::list_event_taps();
    println!("{} tap(s)", taps.len());
    for t in &taps {
        println!(
            "tap#{:<11} {:?} {} enabled={} owner={:?}({}) target={:?}",
            t.tap_id,
            t.location,
            if t.active { "active" } else { "listen" },
            t.enabled,
            t.owner_name,
            t.owner_pid,
            t.target_pid,
        );
    }
}
