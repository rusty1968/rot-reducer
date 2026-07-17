# CSA Extension — Implementation Plan

> **Design doc:** [csa-extension.md](csa-extension.md)
> **Status:** Not started

Steps are ordered by dependency. Each step is independently compilable before
the next begins (except step 4, which requires step 3's new `State` arm).

---

| # | Task | File(s) | Notes |
|---|---|---|---|
| 0 | `CompareToRim` → `VerifyFirmware`, `MeasurePlatformFirmware` → `ReadFirmware` | `src/lib.rs`, `examples/` | Pure renames. No logic change. Apply across all files before any other changes. |
| 1 | `PowerOnResult` replaces `Provisioning` | `src/lib.rs` | New type with `Provisioned { active_slot: u8 }`, `Unprovisioned`, `SelfVerificationFailed`. Replace `Provisioning` everywhere. Update `PowerGood` arm in `call_handler`. |
| 2 | `ComponentKind` + annotated chain | `src/lib.rs` | Add `pub enum ComponentKind { Active, Passive }`. Change `Rot.chain` from `heapless::Vec<ComponentId, N>` to `heapless::Vec<(ComponentId, ComponentKind), N>`. Update all `chain[cursor]` indexing sites. |
| 3 | `ComponentReady` event variant | `src/lib.rs` | Add `ComponentReady(ComponentId)` arm to `Event`. No handler yet — `_ => Outcome::Super` in all existing states catches it safely. |
| 4 | `AwaitingReady` state variant | `src/lib.rs` | Add `AwaitingReady` arm to `State`. Add empty `call_entry_action` arm (no-op). `superstate()` returns `None` for now (fixed in step 8). |
| 5 | `awaiting` field on `Rot` | `src/lib.rs` | Add `awaiting: Option<ComponentId>` to `Rot`. Initialise to `None` in `Rot::new`. Clear in `VerifyingPlatform` entry action alongside cursor reset. |
| 6 | Update `VerifyingPlatform` handler | `src/lib.rs` | In `VerificationPassed` arm: after `ReleaseReset`, check `chain[cursor+1].1`. `Passive` → `Outcome::Handled` (unchanged). `Active` → advance cursor, set `rot.awaiting`, emit `ReadFirmware` + `VerifyFirmware` for next, `Outcome::Transition(State::AwaitingReady)`. Chain-done path → `Outcome::Transition(State::Ready)` (unchanged). |
| 7 | Implement `AwaitingReady` handler | `src/lib.rs` | Handle three events: `ComponentReady(id)` — if `id == rot.awaiting`, clear `rot.awaiting`; if chain done `→ Ready`, else `Outcome::Handled`; mismatch id → ignore (`Outcome::Handled`). `VerificationPassed(id)` — emit `ReleaseReset`, advance, set `awaiting`, emit next measure pair, `Outcome::Handled`. `VerificationFailed(id)` — `rot.failed = Some(id)`, `→ Recovering`. |
| 8 | `AwaitingReady` joins `Operational` | `src/lib.rs` | Add `State::AwaitingReady` to the `superstate()` match arm that returns `Some(Superstate::Operational(…))`. |
| 9 | Update `Orchestrator::new` signature | `src/lib.rs` | Chain parameter type changes to `heapless::Vec<(ComponentId, ComponentKind), N>`. Update `run<N>` helper the same way. Update the doctest inside the `Orchestrator` doc comment. |
| 10 | Update tests + add INV10–INV13 | `src/lib.rs` | Wrap all bare `ComponentId` chain entries as `(id, ComponentKind::Passive)` in existing tests (behaviour unchanged). Add four new tests: `spurious_component_ready_is_ignored` (INV10), `active_component_gates_on_component_ready` (INV11), `self_verification_failure_latches_immediately` (INV12), `attestation_in_awaiting_ready` (INV13). |
| 11 | Update `examples/board.rs` | `examples/board.rs` | Annotate components as `(BMC, ComponentKind::Active)`, `(HOST, ComponentKind::Active)`. Add `ComponentReady` signals at the right points in the scripted event sequence. |
| 12 | `cargo test` — all green | — | `cargo test && cargo run --example board` |

---

## Invariants added

| ID | Statement |
|---|---|
| **INV10** | A `ComponentReady(id)` that does not match `rot.awaiting` is silently ignored. |
| **INV11** | An `Active` component is never advanced past without a `ComponentReady` for it. |
| **INV12** | `PowerGood(SelfVerificationFailed)` always transitions to `Locked` without entering `VerifyingPlatform`. |
| **INV13** | `AttestationChallenge` is handled in `AwaitingReady` the same as in every other `Operational` state. |

## Backward compatibility

Existing boards that use an all-`Passive` chain and never send `SelfVerificationFailed`
need only the mechanical wrapping in steps 1, 2, and 9:

- `(id, ComponentKind::Passive)` in chain construction
- `PowerOnResult::Provisioned { active_slot: 0 }` in `PowerGood`

The effect trace for an all-`Passive` chain is identical to the current behaviour.
