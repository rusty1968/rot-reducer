# Invariants

Invariants are the behaviours the machine is required to guarantee, stated
precisely enough to be tested. Each one is cross-referenced by ID in the source
(`INVn`) and in the test comments, so a failing test identifies the exact
requirement it broke.

Invariants are grouped by lifecycle phase.

---

## Boot sequence

### INV1 — Provisioned power-on always enters `VerifyingPlatform`

A `PowerGood(Provisioned)` event always causes a transition to
`VerifyingPlatform`. The machine never jumps directly to `Ready` or `Locked` on
a successful provisioned boot.

**Verified by**: `cold_boot_walks_chain_in_order`

---

### INV2 — No `ReleaseReset` before `VerificationPassed`

`ReleaseReset(id)` is never emitted for a component before the corresponding
`VerificationPassed(id)` arrives. Components are only freed from reset once the
eRoT has authenticated their firmware.

**Verified by**: `cold_boot_walks_chain_in_order` (effect order assertion)

---

### INV3 — Chain order is respected

Components are verified and released in the order the platform supplied in the
chain. No component is skipped, verified out of order, or released before its
predecessor.

**Verified by**: `cold_boot_walks_chain_in_order` (exact effect sequence assertion)

---

## Update lifecycle

### INV4 — Rejected update never triggers recovery

A `UpdateRejected` event causes `DiscardStaged` and returns to `Ready`. It never
causes a transition to `Recovering` or `Locked`. A rejected update is a policy
decision, not a corruption event.

**Verified by**: `update_rollback_is_not_recovery`

---

## Runtime resiliency

### INV5 — Corruption recovery targets the named component and re-walks

`CorruptionDetected(id)` sets the failed component to `id` and transitions to
`Recovering`, which emits `RestoreGoldenImage(id)` — exactly the named
component, not the whole chain. After `Restored`, the machine re-enters
`VerifyingPlatform` and re-walks the full chain from component 0.

**Verified by**: `runtime_corruption_targets_component_and_rewalks`

---

### INV6 — Attestation is answerable from any `Operational` state

`AttestationChallenge` produces `SignAttestation` from `Ready`, `Updating`,
`Recovering`, and `AwaitingReady` without any state change. It is handled once
in the `Operational` superstate rather than duplicated across all four.

**Verified by**: `attestation_shared_across_operational_states`,
`attestation_in_awaiting_ready`

---

### INV7 — Recovery retry cap is enforced in-core; consecutive failures only

After `max_retry` consecutive failed restore attempts, the core self-emits
`Effect::Emit(RecoveryFailed)` and drives the machine to `Locked` entirely
within one `dispatch_with` call. No external watchdog or `RecoveryFailed` event
from outside is required.

The count tracks **consecutive** failures: it resets to zero when the machine
successfully reaches `Ready` after a recovery. A later, unrelated corruption
episode starts from zero and cannot prematurely latch the machine due to a
previous episode.

**Verified by**: `retry_cap_self_latches_via_emit`,
`retry_count_resets_after_successful_recovery`, `custom_retry_cap_latches_sooner`

---

## Component identity

### INV8 — The core never inspects a `ComponentId`

`ComponentId` is an opaque `u8`. The core only carries and equality-compares it.
All hardware mapping — which id corresponds to which physical device — belongs
entirely to the platform layer. The core cannot distinguish components by anything
other than identity equality.

**Verified by**: test setup (both tests use only generic `C0`/`C1` with no
hardware meaning attached)

---

## Active component gating (CSA dual-layer verification)

### INV9 — Spurious `ComponentReady` is silently ignored

A `ComponentReady(id)` event where `id` does not match the currently awaited
component (`rot.awaiting`) is dropped without advancing the chain walk. The
machine stays in `AwaitingReady`. This prevents a stale or misdirected signal
from incorrectly advancing trust.

**Verified by**: `spurious_component_ready_is_ignored`

---

### INV10 — `Active` component gates the chain walk on iRoT readiness

After an `Active` component passes eRoT verification and is released from reset,
the machine transitions to `AwaitingReady` and does not advance the chain walk
until `ComponentReady(id)` arrives for that component. The eRoT verification of
the next component may proceed speculatively in parallel, but the chain position
does not advance until the integrated iRoT confirms readiness.

**Verified by**: `active_component_gates_on_component_ready`

---

### INV11 — Self-verification failure latches immediately

`PowerGood(SelfVerificationFailed)` transitions directly to `Locked` and emits
`LatchLockdown`. The machine never enters `VerifyingPlatform`. The eRoT itself
is untrusted; no platform component is verified or released.

**Verified by**: `self_verification_failure_latches_immediately`

---

### INV12 — `AttestationChallenge` is answered in `AwaitingReady`

`AttestationChallenge` produces `SignAttestation` in `AwaitingReady` exactly as
in `Ready`, `Updating`, and `Recovering`. This follows from `AwaitingReady`
being a member of the `Operational` superstate. No state change occurs.

**Verified by**: `attestation_in_awaiting_ready`

---

## Summary table

| ID | Phase | Statement | Test |
|---|---|---|---|
| **INV1** | Boot | Provisioned power-on always enters `VerifyingPlatform` | `cold_boot_walks_chain_in_order` |
| **INV2** | Boot | No `ReleaseReset(id)` before `VerificationPassed(id)` | `cold_boot_walks_chain_in_order` |
| **INV3** | Boot | Components verified and released in chain order | `cold_boot_walks_chain_in_order` |
| **INV4** | Update | Rejected update rolls back; never triggers recovery | `update_rollback_is_not_recovery` |
| **INV5** | Recovery | Corruption targets named component; re-walks full chain | `runtime_corruption_targets_component_and_rewalks` |
| **INV6** | Attestation | `AttestationChallenge` answered from all `Operational` states | `attestation_shared_across_operational_states`, `attestation_in_awaiting_ready` |
| **INV7** | Recovery | Retry cap enforced in-core; consecutive failures only | `retry_cap_self_latches_via_emit`, `retry_count_resets_after_successful_recovery` |
| **INV8** | Identity | Core never inspects `ComponentId` contents | test setup |
| **INV9** | Active gating | Spurious `ComponentReady` is ignored | `spurious_component_ready_is_ignored` |
| **INV10** | Active gating | `Active` component gates chain walk on iRoT readiness | `active_component_gates_on_component_ready` |
| **INV11** | Boot | Self-verification failure latches immediately | `self_verification_failure_latches_immediately` |
| **INV12** | Attestation | `AttestationChallenge` answered in `AwaitingReady` | `attestation_in_awaiting_ready` |
