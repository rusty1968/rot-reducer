//! Example: driving `rot_reducer` from a Pigweed `pw_kernel` user task.
//!
//! This is a HOST-RUNNABLE sketch (`cargo run --example pw_task`) of the shape a
//! real `pw_kernel` task takes: the `#[entry]` task owns the loop, reads bytes
//! off IPC channels, decodes them into domain [`Event`]s, drives the pure
//! `Orchestrator`, and carries out every [`Effect`] as an IPC transact to a
//! driver task.
//!
//! On real pw_kernel the stubbed pieces here map to:
//!
//! | this example | real pw_kernel |
//! | --- | --- |
//! | `fn main` | `#[entry] fn entry()` in a `#![no_std] #![no_main]` bin |
//! | the scripted `inbox` bytes | messages delivered by channels |
//! | "object_wait / channel_read" (elided) | `userspace::syscall::{object_wait, channel_read}` |
//! | `IpcPlatform::execute`'s prints | `userspace::syscall::channel_transact(handle::X, ..)` |
//! | `"FLASH"` / `"GPIO"` / `"CRYPTO"` | `<app>_codegen::handle::*` |
//!
//! The point: the `#[entry]` task IS the shell/board layer. The pure core
//! (`Orchestrator`) never names a syscall, a channel, or a component.

use rot_reducer::{ComponentId, Effect, Event, Orchestrator, Platform, Provisioning, State};

// Board policy — the only place real components are named (like the handle and
// id constants a task hard-codes for its own wiring).
const CAPACITY: usize = 4;
const MAX_RETRY: u8 = 3;
const BMC: ComponentId = ComponentId::new(0);
const HOST: ComponentId = ComponentId::new(1);

/// The [`Platform`] seam expressed as pw_kernel IPC. On real hardware each arm
/// is a `syscall::channel_transact(handle::X, &req, &mut resp, deadline)` to a
/// driver task (flash server, crypto/attestation server, GPIO/reset
/// controller). Here it just prints the transaction it would issue.
struct IpcPlatform;

impl Platform for IpcPlatform {
    fn execute(&mut self, effect: Effect) {
        // (task, request-bytes) it would send. The opaque `ComponentId` becomes
        // a request byte the driver task decodes.
        let (task, req): (&str, [u8; 2]) = match effect {
            Effect::MeasurePlatformFirmware(id) => ("FLASH", [0x01, id.get()]),
            Effect::CompareToRim(id) => ("FLASH", [0x02, id.get()]),
            Effect::ReleaseReset(id) => ("GPIO", [0x03, id.get()]),
            Effect::RestoreGoldenImage(id) => ("FLASH", [0x04, id.get()]),
            Effect::AuthenticateUpdate => ("CRYPTO", [0x10, 0]),
            Effect::StageUpdate => ("FLASH", [0x11, 0]),
            Effect::ActivateUpdate => ("FLASH", [0x12, 0]),
            Effect::DiscardStaged => ("FLASH", [0x13, 0]),
            Effect::SignAttestation => ("CRYPTO", [0x20, 0]),
            Effect::LatchLockdown => ("GPIO", [0xFF, 0]),
            // Never reaches a Platform: the orchestrator consumes it internally.
            Effect::Emit(_) => return,
        };
        println!("  channel_transact(handle::{task}, req={req:02x?})");
    }
}

/// Decode raw channel bytes into a domain [`Event`]. Toy wire format:
/// `[tag, arg]`.
fn decode_event(bytes: &[u8]) -> Option<Event> {
    match bytes {
        [0x00, 0x01] => Some(Event::PowerGood(Provisioning::Provisioned)),
        [0x00, 0x00] => Some(Event::PowerGood(Provisioning::Unprovisioned)),
        [0x01, id] => Some(Event::PlatformMeasured(ComponentId::new(*id))),
        [0x02, id] => Some(Event::PlatformMismatch(ComponentId::new(*id))),
        [0x03, _] => Some(Event::AttestationChallenge),
        [0x04, id] => Some(Event::CorruptionDetected(ComponentId::new(*id))),
        [0x05, id] => Some(Event::Restored(ComponentId::new(*id))),
        _ => None,
    }
}

fn main() {
    let mut chain = heapless::Vec::<ComponentId, CAPACITY>::new();
    let _ = chain.push(BMC);
    let _ = chain.push(HOST);

    // Held across the loop, so state survives from one event to the next.
    let mut orch = Orchestrator::new(chain, MAX_RETRY);
    let mut platform = IpcPlatform;

    // On pw_kernel these bytes arrive from `channel_read` after `object_wait`
    // on a wait group fanning in the reset / measure / attest / corruption
    // channels. Scripted here so the example runs on the host.
    let inbox: &[&[u8]] = &[
        &[0x00, 0x01], // PowerGood(Provisioned)
        &[0x01, 0x00], // PlatformMeasured(BMC)
        &[0x01, 0x01], // PlatformMeasured(HOST)   -> Ready
        &[0x03, 0x00], // AttestationChallenge     -> SignAttestation
        &[0x04, 0x01], // CorruptionDetected(HOST) -> Recovering
    ];

    for msg in inbox {
        // object_wait(handle::WG, READABLE, ..) then channel_read(..) gave `msg`.
        let Some(event) = decode_event(msg) else {
            continue;
        };
        println!("event: {event:?}");
        orch.dispatch(&mut platform, event); // push into the pure core

        if orch.state() == State::Locked {
            println!("state: Locked — RoT latched lockdown");
            break;
        }
    }

    println!("final state: {:?}", orch.state());
}
