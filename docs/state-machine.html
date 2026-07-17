# State Machine

This document describes the state machine that lives in `src/lib.rs`: its
states, shared storage, entry actions, transition table, and the single
superstate — and how all of it is expressed through `statig`'s hand-written
(macro-free) trait impls.

```mermaid
stateDiagram-v2
    [*] --> PowerOnReset

    PowerOnReset --> VerifyingPlatform : PowerGood(Provisioned)
    PowerOnReset --> Locked             : PowerGood(Unprovisioned)
    PowerOnReset --> Locked             : PowerGood(SelfVerificationFailed)

    VerifyingPlatform --> VerifyingPlatform : VerificationPassed [more, Passive]\n/ ReleaseReset · ReadFirmware · VerifyFirmware
    VerifyingPlatform --> AwaitingReady     : VerificationPassed [more, Active]\n/ ReleaseReset · ReadFirmware · VerifyFirmware
    VerifyingPlatform --> Ready             : VerificationPassed [chain done]\n/ ReleaseReset
    VerifyingPlatform --> Recovering        : VerificationFailed\n/ RestoreGoldenImage

    AwaitingReady --> AwaitingReady : VerificationPassed [more]\n/ ReleaseReset · ReadFirmware · VerifyFirmware
    AwaitingReady --> Ready         : ComponentReady [chain done]
    AwaitingReady --> AwaitingReady : ComponentReady [more]
    AwaitingReady --> Recovering    : VerificationFailed\n/ RestoreGoldenImage

    state Operational {
        [*]           --> Ready
        Ready         --> Updating      : UpdateRequest\n/ AuthenticateUpdate · StageUpdate
        Updating      --> Ready         : UpdateVerified / ActivateUpdate
        Updating      --> Ready         : UpdateRejected / DiscardStaged
        Ready         --> Recovering    : CorruptionDetected\n/ RestoreGoldenImage
        Updating      --> Recovering    : CorruptionDetected\n/ RestoreGoldenImage
        AwaitingReady --> Recovering    : CorruptionDetected\n/ RestoreGoldenImage
    }

    Recovering --> VerifyingPlatform : Restored [retry < max_retry]
    Recovering --> Locked    : Restored [retry ≥ max_retry]\n(self-emits RecoveryFailed)\n/ LatchLockdown
    Locked     --> Locked    : (terminal — all events ignored)
```

---

## Shared storage — `Rot<N>`

Every handler receives a `&mut Rot<N>` alongside the event and the `Sink`. This
struct is `statig`'s *shared storage*: a single allocation that persists across
events and is visible to every state and superstate. States themselves are unit
variants and carry no data; anything that must survive a transition lives here.

| Field | Type | Purpose |
|---|---|---|
| `chain` | `Vec<(ComponentId, ComponentKind), N>` | Ordered trust chain, supplied by the board at construction time. Never mutated after build. |
| `cursor` | `u8` | Index of the component currently under verification. Reset to 0 on every `VerifyingPlatform` entry. Advances on each `VerificationPassed` (via `Outcome::Handled` rather than a self-transition, to avoid triggering re-entry). |
| `failed` | `Option<ComponentId>` | The component that triggered the current recovery episode; `None` while healthy. Set on `VerificationFailed` or `CorruptionDetected`; drives the `RestoreGoldenImage` emission in `Recovering`'s entry action. |
| `retry_count` | `u8` | Number of consecutive failed restore attempts in the current episode. Cleared to 0 in `Ready`'s entry action — consecutive only (INV7). |
| `max_retry` | `u8` | Board-chosen ceiling for `retry_count`. When `retry_count >= max_retry` the machine self-emits `RecoveryFailed` instead of transitioning back to `VerifyingPlatform`. |
| `awaiting` | `Option<ComponentId>` | The `Active` component whose iRoT readiness is currently outstanding. `Some` only while in `AwaitingReady`; `None` everywhere else (INV9). |

The effect buffer is deliberately **absent** from `Rot`. Effects flow through the
`Sink` (the `statig` context), which the orchestrator creates fresh for every
event and drains afterward. This is what keeps `Rot` clean between dispatches
with no `before_dispatch` clear hook needed.

---

## Context — `Sink`

The only thing a handler can do to the outside world is call `ctx.emit(effect)`.
`Sink` is an append-only `heapless::Vec<Effect, EFFECT_CAP>`. It can push; it
cannot pull, read, or do I/O. The orchestrator owns a fresh `Sink` per dispatch
and reads the effects out after `handle_with_context` returns.

---

## States

### `PowerOnReset`

The machine's initial state. The machine enters here unconditionally at startup
(via `IntoStateMachine::initial()`); the first event is always
`PowerGood(PowerOnResult)`.

**Entry action**: none.

**Events handled**:

| Event | Guard | Effect emitted | Next state |
|---|---|---|---|
| `PowerGood(Provisioned)` | — | — | `VerifyingPlatform` |
| `PowerGood(Unprovisioned)` | — | — | `Locked` |
| `PowerGood(SelfVerificationFailed)` | — | — | `Locked` |
| anything else | — | — | `Outcome::Super` (falls through; top level is a no-op) |

`Provisioned` means the shell found valid provisioning data in OTP/UFM and the
eRoT's own measurement check passed — the machine can vouch for platform
components. `Unprovisioned` and `SelfVerificationFailed` both latch the machine
without entering the verification walk; the distinction matters for root-cause
tracing in the effect trace.

---

### `VerifyingPlatform`

Walks the trust chain component-by-component. The cursor advances on each
`VerificationPassed` using `Outcome::Handled` (not a self-transition) so the
cursor survives across events.

**Entry action**: reset `cursor` to 0, reset `awaiting` to `None`, emit
`ReadFirmware(chain[0])` + `VerifyFirmware(chain[0])`.

**Events handled**:

| Event | Guard | Effects emitted | Next state |
|---|---|---|---|
| `VerificationPassed(id)` | more components, current is `Passive` | `ReleaseReset(id)` · `ReadFirmware(next)` · `VerifyFirmware(next)` | `Outcome::Handled` (cursor ++) |
| `VerificationPassed(id)` | more components, current is `Active` | `ReleaseReset(id)` · `ReadFirmware(next)` · `VerifyFirmware(next)` | `AwaitingReady` (set `awaiting = Some(id)`) |
| `VerificationPassed(id)` | chain done | `ReleaseReset(id)` | `Ready` |
| `VerificationFailed(id)` | — | — | `Recovering` (set `failed = Some(id)`) |
| anything else | — | — | `Outcome::Super` → `Operational` (handles `AttestationChallenge`, `CorruptionDetected`) |

The speculative read (`ReadFirmware` + `VerifyFirmware` for the *next* component
emitted before the current one's iRoT has confirmed readiness) is intentional:
the eRoT can start authenticating the next component's firmware while the Active
component's integrated iRoT is still booting.

---

### `AwaitingReady`

Reached when an `Active` component passes eRoT authentication. The cursor
already points at the next component and its `ReadFirmware` + `VerifyFirmware`
have already been speculatively emitted. The machine waits here until the `Active`
component's iRoT signals readiness before acting on the next `VerificationPassed`.

**Entry action**: none (entered via `Outcome::Transition`; cursor and `awaiting`
were set by the `VerifyingPlatform` handler that triggered the transition).

**Events handled**:

| Event | Guard | Effects emitted | Next state |
|---|---|---|---|
| `ComponentReady(id)` | `id != rot.awaiting` | — | `Outcome::Handled` (stale/spurious — ignore, INV9) |
| `ComponentReady(id)` | `id == rot.awaiting` | — | `Outcome::Handled` (clear `awaiting`) |
| `VerificationPassed(id)` | more components | `ReleaseReset(id)` · `ReadFirmware(next)` · `VerifyFirmware(next)` | `Outcome::Handled` (cursor ++) |
| `VerificationPassed(id)` | chain done | `ReleaseReset(id)` | `Ready` |
| `VerificationFailed(id)` | — | — | `Recovering` (set `failed = Some(id)`, clear `awaiting`) |
| anything else | — | — | `Outcome::Super` → `Operational` |

`ComponentReady` and `VerificationPassed` are independent signals that may arrive
in either order; both must have arrived for the walk to advance. The `awaiting`
field tracks only whether `ComponentReady` has been seen; the state itself
(`AwaitingReady`) tracks whether `VerificationPassed` is still outstanding.

---

### `Ready`

The normal operational state: the full chain has been verified, all components
are released, and the machine can handle attestation challenges, update requests,
and corruption events.

**Entry action**: reset `retry_count` to 0. This is what makes the retry cap
count *consecutive* failures — a new corruption episode after a successful
recovery starts from zero (INV7).

**Events handled**:

| Event | Guard | Effects emitted | Next state |
|---|---|---|---|
| `UpdateRequest` | — | — | `Updating` |
| anything else | — | — | `Outcome::Super` → `Operational` |

---

### `Updating`

An update is in progress. The entry action authenticates and stages the update
before any update-outcome event can arrive.

**Entry action**: emit `AuthenticateUpdate` + `StageUpdate`.

**Events handled**:

| Event | Guard | Effects emitted | Next state |
|---|---|---|---|
| `UpdateVerified` | — | `ActivateUpdate` | `Ready` |
| `UpdateRejected` | — | `DiscardStaged` | `Ready` (INV4 — rejected update is not corruption) |
| anything else | — | — | `Outcome::Super` → `Operational` |

---

### `Recovering`

The machine is attempting to restore a corrupted component.

**Entry action**: emit `RestoreGoldenImage(rot.failed)` — for exactly the named
failed component, not the whole chain (INV5).

**Events handled**:

| Event | Guard | Effects emitted | Next state |
|---|---|---|---|
| `Restored(_)` | `retry_count + 1 < max_retry` | — | `VerifyingPlatform` (re-walk from top; cursor reset by entry action) |
| `Restored(_)` | `retry_count + 1 >= max_retry` | `Effect::Emit(RecoveryFailed)` | `Outcome::Handled` (orchestrator queues `RecoveryFailed` and dispatches it next) |
| `RecoveryFailed` | — | — | `Locked` |
| anything else | — | — | `Outcome::Super` → `Operational` |

`Effect::Emit(RecoveryFailed)` is the *feedback-as-data* mechanism: the core
never receives an external `RecoveryFailed`; it produces one internally, the
orchestrator intercepts it, re-dispatches it before returning, and the whole
decision is visible in the effect trace. No external watchdog is needed (INV7).

---

### `Locked`

The terminal state. All events fall through to `Outcome::Super`; the top-level
handler returns `Outcome::Super` which `statig` silently discards. No further
transitions are possible.

**Entry action**: emit `LatchLockdown` — a single instruction to the board to
hold all components in reset permanently.

**Events handled**: none (every event reaches the implicit top level and is
dropped).

---

## Superstate — `Operational`

Four states (`Ready`, `Updating`, `Recovering`, `AwaitingReady`) share the
`Operational` superstate. Their `superstate()` method returns
`Some(Superstate::Operational(…))`; `PowerOnReset`, `VerifyingPlatform`, and
`Locked` return `None`.

When a state's handler returns `Outcome::Super`, `statig` calls
`Superstate::call_handler`. Two events are answered here rather than duplicated
in each of the four states:

| Event | Effects emitted | Next state |
|---|---|---|
| `AttestationChallenge` | `SignAttestation` | `Outcome::Handled` (no transition, INV6) |
| `CorruptionDetected(id)` | — | `Recovering` (set `rot.failed = Some(id)`, INV5) |
| anything else | — | `Outcome::Super` (discarded at the top level) |

---

## Exit actions

There are none. `call_exit_action` uses the default empty implementation.
All clean-up is done in **entry actions** of the target state or inside the
handler that triggers the transition, keeping the logic in one place.

---

## `statig` integration

The machine uses `statig` 0.4.1 with hand-written trait impls — no proc-macros.

| Trait | Implemented by | Role |
|---|---|---|
| `IntoStateMachine` | `Rot<N>` | Declares the associated types (`Event`, `Context = Sink`, `State`, `Superstate`) and the static `initial() -> State` function. |
| `StatigState<Rot<N>>` | `State` | `call_handler` — dispatch an event to the correct state arm; `call_entry_action` — run the state's entry logic; `superstate` — return the containing superstate if any. |
| `StatigSuperstate<Rot<N>>` | `Superstate<'_>` | `call_handler` — handle events that fell through from a leaf state. |

`initial()` is a `fn() -> State` (no `self`), so it is a compile-time constant.
This means the machine always starts in `PowerOnReset` — there is no way to
branch on a runtime value at construction time. The shell-supplied
`PowerGood(PowerOnResult)` event is the first real branching point (see
`verification-model.md` §Board boundary for the rationale).
