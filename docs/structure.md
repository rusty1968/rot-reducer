# Crate Structure

## The Interpreter Pattern

`rot_reducer` is structured as an **interpreter**: the core produces a program
(a sequence of `Effect` values), and a separate interpreter outside the core
executes it. The two roles never mix.

```
┌─────────────────────────────────────┐
│  Core (src/lib.rs)                  │
│                                     │
│  event → [ handler ] → Effect*      │
│                                     │
│  Pure function. No I/O.             │
│  Produces a description of work.    │
└──────────────────┬──────────────────┘
                   │ Effect values
                   ▼
┌─────────────────────────────────────┐
│  Interpreter (board / shell layer)  │
│                                     │
│  Platform::execute(effect)          │
│                                     │
│  Impure. Talks to hardware.         │
│  Executes the described work.       │
└─────────────────────────────────────┘
```

### The program: `Effect`

`Effect` is an algebraic data type — an enum whose variants are descriptions of
operations, not the operations themselves:

```rust
pub enum Effect {
    ReadFirmware(ComponentId),
    VerifyFirmware(ComponentId),
    ReleaseReset(ComponentId),
    SignAttestation,
    AuthenticateUpdate,
    StageUpdate,
    ActivateUpdate,
    DiscardStaged,
    RestoreGoldenImage(ComponentId),
    LatchLockdown,
    Emit(Event),       // internal only — never reaches the interpreter
}
```

A handler builds the program by calling `ctx.emit(effect)` for each step. The
`Sink` accumulates them in order. Nothing executes yet.

### The interpreter: `Platform`

`Platform` is a single-method trait the board layer implements:

```rust
pub trait Platform {
    fn execute(&mut self, effect: Effect);
}
```

The `Orchestrator` drains the `Sink` after each event and calls
`platform.execute(effect)` once per external effect. The board decides how each
variant maps to real hardware — SPI interposition, GPIO, a crypto accelerator
call, an IPC channel — and the core has no knowledge of any of it.

### The dispatch loop: `Orchestrator`

`Orchestrator::dispatch_with` is the interpreter loop for a single event:

```rust
pub fn dispatch_with(&mut self, event: Event, mut on_effect: impl FnMut(Effect)) {
    let mut pending = /* small fixed-size queue */;
    pending.push(event);

    while let Some(ev) = pending.next() {
        let mut buf = Sink::new();
        self.machine.handle_with_context(&ev, &mut buf);

        for effect in buf.effects() {
            match effect {
                Effect::Emit(internal) => pending.push(internal), // re-queue
                external             => on_effect(external),      // interpret
            }
        }
    }
}
```

One `dispatch_with` call runs the machine **to completion** for that event,
including any internally self-emitted follow-ups (`Effect::Emit`), before
returning. From the caller's perspective one call always fully settles the
machine.

### Why this structure

**Testability.** Because the core only produces `Effect` values and never
executes them, every test is an assertion over an ordered `Vec<Effect>` and a
final `State`. No mocking of hardware, no threading, no timing:

```rust
let (effects, state) = drive(chain, &[BOOT, VerificationPassed(C0)]);
assert_eq!(effects, vec![ReadFirmware(C0), VerifyFirmware(C0), ReleaseReset(C0)]);
assert_eq!(state, State::Ready);
```

**Replayability.** The core is a pure function of `(state, event)`. The same
input always produces the same output. An effect trace is a complete record of
observable behaviour — it can be logged, replayed, or diffed.

**Portability.** The core is `#![no_std]` and has no platform dependencies. The
interpreter (`Platform` impl) is the only code that touches hardware, and it
lives entirely outside this crate.

**Separation of policy and mechanism.** The core decides *what* to do (the
policy: verify before releasing, re-walk after recovery, latch on exhaustion).
The board decides *how* to do it (the mechanism: SPI interposition or I3C,
GPIO or reset controller IPC, in-process or cross-process). Neither knows
anything about the other.

### `Effect::Emit` — internal effects

One variant, `Emit(Event)`, never reaches the `Platform`. It lets a handler
schedule a follow-up event to be handled next, without any external involvement.
This is used to enforce the recovery-retry cap entirely inside the core: when
`retry_count >= max_retry`, the `Recovering` handler emits
`Effect::Emit(Event::RecoveryFailed)`, and the `Orchestrator` dispatches
`RecoveryFailed` on the next iteration of its inner loop — driving the machine
to `Locked` before `dispatch_with` returns, with no external actor required.

The follow-up appears in the effect trace as `Emit(RecoveryFailed)`, so the
decision is visible and auditable even though it never left the core.
