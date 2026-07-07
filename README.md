# `rot_reducer`

A reusable **Root-of-Trust HSM** state machine for OCP-style platform security,
written as a *sans-IO* reducer: the core is a pure function of
`(state, event, shared storage)` that **describes** side effects instead of
performing them. It never touches hardware, never reads the world, and names no
concrete component — a board layer supplies all of that.

Engine: [`statig`](https://crates.io/crates/statig) 0.4.1 with hand-written
(macro-free) trait impls. `#![no_std]`, `#![forbid(unsafe_code)]`, two deps
(`heapless` + `statig`).

```text
        world (board / shell)                          this crate (pure core)
   ┌───────────────────────────┐                 ┌──────────────────────────────┐
   │  reads OTP/UFM, hardware   │   Event  ──────►│  Orchestrator (dispatch loop)│
   │  IRQs, measurement results │                 │      │                       │
   │                            │                 │      ▼                       │
   │  Platform::execute(effect) │◄────── Effect ──│  StateMachine<Rot<N>>        │
   │  drives flash/reset/bus    │                 │   State × Superstate handlers│
   └───────────────────────────┘                 └──────────────────────────────┘
     examples/board.rs                              src/lib.rs
```

The world speaks to the core **only** through `Event`s; the core speaks back
**only** through `Effect`s. That single firewall is what makes every run
replayable and every test an assertion over an ordered `Vec<Effect>`.

## An execution model for OpenPRoT

This crate is an **executable model of the OpenPRoT PRoT security lifecycle** —
the boot-time trust chain, runtime attestation, firmware update, and corruption
recovery that a Platform Root of Trust is responsible for.

**Why a model, and why executable.** The OpenPRoT specification defines that
lifecycle around NIST SP 800-193's *protect → detect → recover* pillars, but the
sections that would pin down the PRoT's own behaviour — **PRoT Resiliency**,
**Firmware Recovery**, and **Secure Boot** — are still `TBD` prose. A sans-IO
reducer turns that prose into something concrete: each requirement becomes a
state transition, and every mandated behaviour becomes an assertion over an
ordered `Vec<Effect>`. The effect trace *is* the normative behaviour, so the
model doubles as a runnable, testable specification rather than a document that
can drift from any implementation.

Its vocabulary maps onto the OpenPRoT service/application layers:

| OpenPRoT concern | Here |
| --- | --- |
| Secure Boot (measure → verify → release) | `MeasuringPlatform` / `CompareToRim` / `ReleaseReset` |
| Firmware Recovery + resiliency (detect → recover → lock) | `CorruptionDetected` → `Recovering` / `RestoreGoldenImage` → `Locked` |
| Attestation (SPDM responder) | `AttestationChallenge` / `SignAttestation` |
| Firmware Update | `Updating` / `StageUpdate` / `ActivateUpdate` |
| Device provisioning gate | `PowerGood(Provisioning)` |

**Faithful to real silicon, by design.** The model tracks the behaviour of a
production PRoT (the Aspeed AST1060 firmware) rather than an idealisation:
identity/DICE key derivation is *not* modelled here because on that hardware it
happens one boot layer down (ROM + measuring bootloader) before this machine
runs — so the machine starts at platform verification, and "attestation key
available" is a power-on precondition carried in `Provisioning`. Likewise it
models the trust *logic* only; the hardware choreography (reset lines, SPI/SMBus
filters, power sequencing) lives in the board `Platform`, mirroring OpenPRoT's
own split that scopes PRoT-hardware mechanisms out to the integrator.

## Layers

| Layer | Lives in | Knows about |
| --- | --- | --- |
| **Core** (state machine + dispatch loop) | `src/lib.rs` | opaque ids only — no hardware, no counts |
| **Board / deployment policy** | `examples/board.rs` | concrete components, chain order, capacity, retry cap |
| **Shell / OS loop** | the caller | event delivery, effect routing, threading |

Deployment policy lives entirely in the board: the trust-chain **capacity**
(`N`), the **components** and their order, and the **recovery-retry cap**
(`max_retry`). The core defines none of them — it is generic over `N` and takes
`max_retry` as a constructor argument.

## The types, by role

### 1. Vocabulary — the data the world and core exchange

| Type | Role |
| --- | --- |
| **`ComponentId`** | Opaque component identity (a `u8` the core never interprets). The board maps each id to real hardware; the core only ever compares and carries them. `new(u8)` / `get() -> u8`. |
| **`Provisioning`** | Result of the power-on provisioning read — `Provisioned` / `Unprovisioned`. Delivered *as event data* (see "reads as events"), never pulled by the core. |
| **`Event`** | Everything the world can tell the core: `PowerGood(Provisioning)`, `PlatformMeasured(id)`, `PlatformMismatch(id)`, `AttestationChallenge`, `UpdateRequest`, `UpdateVerified`, `UpdateRejected`, `CorruptionDetected(id)`, `Restored(id)`, `RecoveryFailed`. |
| **`Effect`** | Everything the core can ask the world to do: `MeasurePlatformFirmware(id)`, `CompareToRim(id)`, `ReleaseReset(id)`, `SignAttestation`, `AuthenticateUpdate`, `StageUpdate`, `ActivateUpdate`, `DiscardStaged`, `RestoreGoldenImage(id)`, `LatchLockdown` — plus one **internal** variant, `Emit(Event)`, that never reaches hardware (see "feedback as data"). |

### 2. The machine — states and shared storage

| Type | Role |
| --- | --- |
| **`State`** | The 6 leaf states: `PowerOnReset`, `MeasuringPlatform`, `Ready`, `Updating`, `Recovering`, `Locked`. Unit variants — no state-local data. |
| **`Superstate<'sub>`** | The single superstate `Operational`, shared by `Ready`/`Updating`/`Recovering`. Handles what's answerable in any operational state (attestation challenge, runtime-corruption watch) so those handlers aren't duplicated. |
| **`Rot<const N: usize>`** | The `statig` shared storage: the trust `chain` (capacity `N`), the walk `cursor`, the `failed` component, `retry_count`, and the `max_retry` cap. Holds everything that must survive across events — notably the cursor, which a self-transition would reset (so the chain is walked with `Outcome::Handled`, not a self-transition). Built with `Rot::new(chain, max_retry)`. Note what's *absent*: no `effects` field — effects live in the `Sink`, not here. |
| **`Sink`** | The inert effect buffer handed to every handler as the `statig` `Context`. Its **only** capability is `emit(effect)` — it cannot read the world or do I/O. The orchestrator owns a fresh `Sink` per dispatch and drains it after `handle` returns, so nothing effectful ever lives in shared storage. This is the core sans-IO trick (see the design moves below). |

`State` and `Superstate` implement `statig`'s handler traits for `Rot<N>`
(`call_handler`, `call_entry_action`, `superstate`); those `impl` blocks *are*
the transition logic.

### 3. The seams — how the core touches the world

| Type | Role |
| --- | --- |
| **`Platform`** | The **OUT** seam: `execute(&mut self, effect: Effect)` performs one external side effect. This is the *only* outward capability. There is deliberately no reader method — the world speaks *in* through `Event`s, not through core-initiated reads. Never called with `Effect::Emit`. |
| **`EventSource`** | The **opt-in IN** seam: `next_event(&mut self) -> Event`. Only needed if you use the `run` loop instead of driving an `Orchestrator` yourself — a caller running its own loop never implements it. |

**Why `EventSource` is opt-in (and `Platform` isn't).** There are two ways to
drive the machine, and `EventSource` matters to only one of them:

1. **You own the loop** (the normal path): hold an `Orchestrator` and push each
   event in with `dispatch` / `dispatch_with`, sourcing events however your
   system already does (an ISR queue, an RTOS mailbox, a scheduler). You never
   implement `EventSource`.
2. **The crate owns the loop** (a convenience): call `run`, and *it* loops
   forever — which means it has to **pull** the next event from somewhere. That
   "somewhere" is `EventSource::next_event`. It exists solely to feed `run`.

The two seams are asymmetric on purpose. `Platform` (OUT) is effectively
required because effects always have to go *somewhere* — every dispatch produces
them. `EventSource` (IN) is optional because event delivery is a question of
*who owns the fetch loop*: **you push** into `dispatch`, or **`run` pulls** via
`EventSource`. On real RoT hardware, events are already produced by the platform
(interrupts, mailboxes) and the integrator already has a loop, so `dispatch`
(push) is the expected default and `run` + `EventSource` is just an opt-in
shortcut for simple setups — `examples/board.rs` iterates a fixed script and
doesn't implement `EventSource` at all.

### 4. The dispatch loop — running the machine

| Type | Role |
| --- | --- |
| **`Orchestrator<const N: usize>`** | The opaque handle a caller steps from its own loop. Wraps `StateMachine<Rot<N>>` so callers **never name a `statig` type**. Its weight is in `dispatch_with(event, on_effect)`: it dispatches one event **to completion** — buffering internal `Effect::Emit` follow-ups and re-dispatching them FIFO before returning — invoking `on_effect` once per *external* effect in emission order. `dispatch(&mut impl Platform, event)` is sugar for the `Platform` path; `state()` reports the current leaf; `new(chain, max_retry)` builds it. |
| **`run<N>(io, chain, max_retry) -> !`** | Batteries-included loop for callers who want the crate to own the loop: pull an event via `EventSource`, dispatch it to completion via `Platform`, forever. Built on `Orchestrator`; callers who already have a loop should hold an `Orchestrator` and step it instead. |

## The three design moves

This crate sits one notch off the strict sans-IO end of the purity spectrum.
Three deliberate mechanical choices define it:

1. **Effects flow through an inert `Sink` in `Context`** — handlers call
   `ctx.emit(..)`, not `rot.emit(..)`. Because the effect buffer lives in the
   orchestrator-owned context (fresh per dispatch), there is no effect queue in shared
   storage, and therefore no `before_dispatch` clear hook. Purity is unchanged:
   the `Sink` can only append effects, never read or perform I/O.

2. **Feedback as data (`Effect::Emit`)** — a handler can schedule a follow-up
   event by emitting `Effect::Emit(event)`. The orchestrator intercepts it and
   re-dispatches FIFO before returning. This is used to enforce the recovery-retry
   cap **inside the core** (INV8): on the `max_retry`-th failed `Restored`, the
   `Recovering` handler self-emits `RecoveryFailed` → `Locked`, with no external
   watchdog — and the whole decision is visible in the effect trace.

3. **Reads as events (no reader lane)** — the core has no synchronous read
   capability. Where a decision needs a world read (provisioning status at
   power-on), the shell performs it and delivers the result *in the event*:
   `Event::PowerGood(Provisioning)`. The core stays a pure function of its inputs.

## Usage

```rust
use rot_reducer::{ComponentId, Orchestrator, Event, Provisioning, State};

// Board policy (the core defines none of this):
const CAPACITY: usize = 8;
const MAX_RETRY: u8 = 3;
const BMC: ComponentId = ComponentId::new(0);
const HOST: ComponentId = ComponentId::new(1);

let mut chain = heapless::Vec::<ComponentId, CAPACITY>::new();
let _ = chain.push(BMC);
let _ = chain.push(HOST);

let mut orch = Orchestrator::new(chain, MAX_RETRY);
let mut effects = Vec::new();

for ev in [
    Event::PowerGood(Provisioning::Provisioned),
    Event::PlatformMeasured(BMC),
    Event::PlatformMeasured(HOST),
] {
    orch.dispatch_with(ev, |e| effects.push(e)); // one step of the caller's loop
    if orch.state() == State::Locked { break; }
}

assert_eq!(orch.state(), State::Ready);
```

A complete worked integration — a `Platform` impl, the component map, and a cold
boot to `Ready` — is in [`examples/board.rs`](examples/board.rs):

```sh
cargo run --example board
```

## Using it from a `pw_kernel` task

Because OpenPRoT runs on Pigweed's `pw_kernel`, the natural home for this crate
is a userspace task. The task **is the shell/board layer**: it owns the loop,
does the IPC, and holds the `Orchestrator` across iterations — while the pure
core names no syscall, channel, or component.

The shape is a direct copy of OpenPRoT's own MCTP server task
(`target/ast10x0/tests/spdm/mctp_server/src/main.rs`), whose server crate states
the same split outright: *"the server does not depend on any OS primitives; the
platform layer drives the event loop."* That task does
`object_wait → channel_read → decode → dispatch → channel_respond`; a RoT task
does the same, with `dispatch` driving the `Orchestrator` and each `Effect`
becoming an IPC transact:

| MCTP server task | RoT task driving `rot_reducer` |
| --- | --- |
| holds `Server<S, N>` | holds `Orchestrator<N>` |
| `channel_read` → `MctpRequestHeader::from_bytes` | `channel_read` → decode into an `Event` |
| `dispatch(&header, …)` | `orch.dispatch(&mut platform, event)` |
| `channel_respond(resp)` | each `Effect` → `channel_transact` to a driver task |
| one channel in the wait group | reset / measure / attest / corruption channels fanned in |

```rust,ignore
#[entry] // #![no_std] #![no_main]
fn entry() {
    let mut orch = Orchestrator::new(chain, MAX_RETRY);   // held across the loop
    // fan in every event source, like wait_group_add in the MCTP task
    for h in [handle::RESET_IRQ, handle::UPDATE, handle::ATTEST, handle::CORRUPT] {
        let _ = syscall::wait_group_add(handle::WG, h, Signals::READABLE, 0);
    }
    let mut buf = [0u8; 32];
    loop {
        let _ = syscall::object_wait(handle::WG, Signals::READABLE, Instant::MAX);
        let n = syscall::channel_read(handle::RESET_IRQ, 0, &mut buf).unwrap_or(0);
        let Some(event) = decode_event(&buf[..n]) else { continue };
        orch.dispatch_with(event, |eff| execute(eff)); // execute = channel_transact per effect
    }
}
```

Three things this makes concrete:

- **The task owns its loop, so it uses `dispatch` (push) — not `run` /
  `EventSource`.** A `pw_kernel` task already has its loop (`object_wait`) and
  event sources (channels); handing that to a `-> !` `run` would fight the kernel
  model. This is the payoff of the opt-in `EventSource` seam.
- **`Platform::execute` is `channel_transact` to other server tasks** — a flash
  server, a crypto/attestation server, a GPIO/reset controller. The opaque
  `ComponentId` rides along as a request byte the driver task decodes, exactly
  like the MCTP task decodes `MctpOp`.
- **Effects that need a result come back as a later `Event`, not a return
  value.** `MeasurePlatformFirmware(id)` *kicks off* the measurement; the result
  arrives as `Event::PlatformMeasured(id)` on a later loop turn (reads-as-events),
  so long-running work never blocks the reducer — it's just another readable
  channel in the wait group.

A host-runnable sketch with a `Platform`-as-IPC impl, an event decoder, and a
scripted channel inbox is in [`examples/pw_task.rs`](examples/pw_task.rs):

```sh
cargo run --example pw_task
```

## Testing

The firewall's payoff is **effect-trace-as-oracle** testing: because the machine
*describes* effects instead of performing them, every test drives a script of
events and asserts on the ordered `Vec<Effect>` and the final `State`. See the
`tests` module in [`src/lib.rs`](src/lib.rs); run with `cargo test` (unit tests +
the doctest above).
