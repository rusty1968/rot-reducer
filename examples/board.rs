//! Example board layer — a small, working example of using `rot_reducer`.
//!
//! It lives OUTSIDE the library (in `examples/`) to show the core is generic:
//! nothing in `rot_reducer` refers to anything here, and this is the ONLY place
//! real components are named. A real integration would be its own crate: its
//! [`Platform`] would touch actual flash banks, buses, and reset lines, and it
//! would read provisioning to build the `PowerGood(PowerOnResult)` event.
//!
//! Both components are marked `Active` (they carry Caliptra iRoTs), so the
//! machine waits for a `ComponentReady` signal after releasing each one from
//! reset before advancing the chain walk.
//!
//! Run it with `cargo run --example board`.

use rot_reducer::{ComponentId, ComponentKind, Effect, Event, Orchestrator, Platform, PowerOnResult, State};

/// How many components the chain holds — the board's choice, not the core's.
/// Two is enough for BMC + Host.
const CAPACITY: usize = 2;

/// How many recovery attempts are allowed before the machine locks down (INV7) —
/// also the board's choice, which the core takes as input.
const MAX_RETRY: u8 = 3;

/// Baseboard Management Controller — checked and released first (top of the chain).
const BMC: ComponentId = ComponentId::new(0);

/// Host / application processor — released only after the BMC is trusted.
const HOST: ComponentId = ComponentId::new(1);

/// The trust order given to `Orchestrator::new` at startup: BMC before Host.
/// Both are `Active`: each carries an integrated iRoT (Caliptra) that performs
/// a second independent firmware verification after the eRoT releases the reset.
fn chain() -> heapless::Vec<(ComponentId, ComponentKind), CAPACITY> {
    let mut c = heapless::Vec::new();
    let _ = c.push((BMC, ComponentKind::Active));
    let _ = c.push((HOST, ComponentKind::Active));
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
    //
    // For Active components the board must also deliver ComponentReady after
    // the iRoT finishes its local verification (e.g. MCTP channel established).
    let script = [
        Event::PowerGood(PowerOnResult::Provisioned),
        Event::VerificationPassed(BMC),   // eRoT auth passes -> ReleaseReset(BMC), AwaitingReady
        Event::ComponentReady(BMC),     // BMC iRoT done, MCTP up -> advance to HOST
        Event::VerificationPassed(HOST),  // eRoT auth passes -> ReleaseReset(HOST), AwaitingReady
        Event::ComponentReady(HOST),    // HOST iRoT done -> Ready
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
