//! Example board layer — a small, working example of using `rot_reducer`.
//!
//! It lives OUTSIDE the library (in `examples/`) to show the core is generic:
//! nothing in `rot_reducer` refers to anything here, and this is the ONLY place
//! real components are named. A real integration would be its own crate: its
//! [`Platform`] would touch actual flash banks, buses, and reset lines, and it
//! would read provisioning to build the `PowerGood(Provisioning)` event.
//!
//! Run it with `cargo run --example board`.

use rot_reducer::{ComponentId, Effect, Event, Orchestrator, Platform, Provisioning, State};

/// How many components the chain holds — the board's choice, not the core's.
/// Two is enough for BMC + Host.
const CAPACITY: usize = 2;

/// How many recovery attempts are allowed before the machine locks down (INV8) —
/// also the board's choice, which the core takes as input.
const MAX_RETRY: u8 = 3;

/// Baseboard Management Controller — checked and released first (top of the chain).
const BMC: ComponentId = ComponentId::new(0);

/// Host / application processor — released only after the BMC is trusted.
const HOST: ComponentId = ComponentId::new(1);

/// The trust order given to `Orchestrator::new` at startup: BMC before Host.
fn chain() -> heapless::Vec<ComponentId, CAPACITY> {
    let mut c = heapless::Vec::new();
    // Both fit: 2 <= CAPACITY.
    let _ = c.push(BMC);
    let _ = c.push(HOST);
    c
}

/// The [`Platform`]: turns each opaque [`ComponentId`] into real hardware
/// actions. Here it just prints; a real board would touch flash, a bus, or a
/// reset line.
struct Board;

impl Platform for Board {
    fn execute(&mut self, effect: Effect) {
        println!("  effect: {effect:?}");
    }
}

fn main() {
    let mut orch = Orchestrator::new(chain(), MAX_RETRY);
    let mut board = Board;

    // The shell reads provisioning and puts the answer inside the event; the
    // core never reads anything itself.
    let script = [
        Event::PowerGood(Provisioning::Provisioned),
        Event::PlatformMeasured(BMC),
        Event::PlatformMeasured(HOST),
    ];

    for ev in script {
        println!("event: {ev:?}");
        orch.dispatch(&mut board, ev);
        if orch.state() == State::Locked {
            break;
        }
    }

    println!("final state: {:?}", orch.state());
    assert_eq!(orch.state(), State::Ready);
}
