wit_bindgen::generate!({ path: "../../../crates/openlogi-plugin/wit", world: "driver" });

use openlogi::peripheral::{hid, session, settings, types::*};
use std::cell::RefCell;

struct Counter;
#[derive(Default)]
struct State { held: bool, count: i64, minimum: i64 }
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }

fn minimum() -> i64 {
    settings::read().into_iter().find_map(|setting| match (setting.key.as_str(), setting.value) {
        ("minimum_count", Value::Integer(n)) => Some(n), _ => None,
    }).unwrap_or(1)
}

fn capability(id: &str, kind: CapabilityKind) -> Capability {
    Capability { id: id.into(), version: 1, required: true, kind }
}

impl Guest for Counter {
    fn attach(_: Context, _: Vec<Setting>) -> Result<Vec<Capability>, Failure> {
        STATE.with(|state| *state.borrow_mut() = State { minimum: minimum(), ..State::default() });
        for endpoint in session::endpoints() {
            if endpoint.facts().role == "controls" { hid::subscribe(&endpoint, &[1])?; }
        }
        Ok(vec![
            capability("input-remap/main", CapabilityKind::InputRemap(vec![Control {
                id: "counter".into(), labels: vec![Label { locale: "en".into(), text: "Counter button".into() }],
                trigger: Trigger::ShortPress, source: None,
            }])),
            capability("extension/main", CapabilityKind::Extension),
        ])
    }

    fn apply(request: Request) -> Result<Vec<Update>, Failure> {
        STATE.with(|state| state.borrow_mut().minimum = minimum());
        Ok(vec![Update::Completed(Completion { request: request.id, outcome: Ok(()) })])
    }

    fn event(event: Event) -> Result<Vec<Update>, Failure> {
        let Event::Report(report) = event else { return Ok(Vec::new()); };
        #[cfg(feature = "conformance-faults")]
        match report.report.payload.first() {
            Some(250) => loop { std::hint::spin_loop(); },
            Some(251) => { std::hint::black_box(vec![0u8; 70 * 1024 * 1024]); },
            Some(252) => {
                for endpoint in session::endpoints() {
                    hid::submit(&endpoint, &Report { kind: ReportKind::Output, id: 1, payload: vec![1] })?;
                }
            }
            Some(253) => { let _handles: Vec<_> = (0..65).flat_map(|_| session::endpoints()).collect(); },
            Some(254) => return Ok((0..257).map(|_| Update::Input(InputEvent {
                capability: "input-remap/main".into(), control: "counter".into(), transition: Transition::Trigger,
            })).collect()),
            _ => {}
        }
        let pressed = report.report.payload.first() == Some(&1);
        let triggered = STATE.with(|state| {
            let mut state = state.borrow_mut();
            let rising = pressed && !state.held;
            state.held = pressed;
            if rising {
                state.count += if cfg!(feature = "alternate") { 2 } else { 1 };
                if state.count >= state.minimum { state.count = 0; return true; }
            }
            false
        });
        Ok(if triggered { vec![Update::Input(InputEvent {
            capability: "input-remap/main".into(), control: "counter".into(), transition: Transition::Trigger,
        })] } else { Vec::new() })
    }

    fn detach(_: DetachReason) -> Result<(), Failure> {
        STATE.with(|state| *state.borrow_mut() = State::default());
        Ok(())
    }

    fn migrate_settings(from: u32, settings: Vec<Setting>) -> Result<Vec<Setting>, Failure> {
        if from != 1 { return Err(Failure { kind: ErrorKind::Invalid, message: "unsupported settings schema".into() }); }
        Ok(settings)
    }
}

export!(Counter);
