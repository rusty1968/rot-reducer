# Verification Model

This document describes how platform firmware verification is modelled in
`rot_reducer`: the problem it solves, the types that carry the domain, the states
and actions that sequence the work, and the boundaries between the pure core and
the board layer that executes it.

---

## 1. The Problem

The eRoT (external Root of Trust — the discrete RoT device, e.g. on a DC-SCM)
must verify every platform component's firmware before releasing it from reset.
Two independent mechanisms do this:

1. **eRoT-side**: the eRoT reads the component's firmware image from the SPI
   flash it controls, verifies the signature and SVN against a Reference
   Integrity Manifest (RIM/PFM), and only then releases the component from reset.

2. **iRoT-side**: components with an integrated Root of Trust (e.g. a BMC SoC
   or CPU with Caliptra) perform their own independent local self-verification
   after reset. The eRoT must wait for this local check to complete before
   treating the component as trusted and advancing to the next one in the chain.

Components that have no integrated iRoT (e.g. a BMC without Caliptra) rely
solely on the eRoT-side check. The eRoT can advance immediately after releasing
them.

This two-tier model — eRoT gate + optional iRoT gate — is the core problem the
verification states solve.

---

## 2. Domain Types

### `ComponentKind`

Classifies a component at chain-build time. Supplied by the board; the core
never derives it.

```
Active  — has an integrated iRoT (e.g. Caliptra); both eRoT and iRoT checks apply
Passive — no integrated iRoT; only the eRoT check applies
```

### `ComponentId`

An opaque `u8` the core carries and equality-compares but never interprets. The
board decides which id maps to which physical device. The core never looks
inside.

### Events that cross the verification boundary

| Event | Direction | Meaning |
|---|---|---|
| `VerificationPassed(ComponentId)` | board → core | The eRoT-side check passed: signature and SVN valid. |
| `VerificationFailed(ComponentId)` | board → core | The eRoT-side check failed: image rejected. |
| `ComponentReady(ComponentId)` | board → core | An `Active` component's integrated iRoT has finished its local verification and the component is operational (e.g. MCTP channel established). |

### Effects the core emits for verification work

| Effect | Meaning |
|---|---|
| `ReadFirmware(ComponentId)` | Ask the board to read the component's firmware image from eRoT-controlled flash. |
| `VerifyFirmware(ComponentId)` | Ask the board to verify the image against the RIM/PFM. The board responds with `VerificationPassed` or `VerificationFailed`. |
| `ReleaseReset(ComponentId)` | Release the named component from reset. Emitted only after `VerificationPassed`. |

These are descriptions, not actions. The board's `Platform::execute` carries
them out; the core never touches hardware.

---

## 3. States

### `VerifyingPlatform`

The core is walking the trust chain: sequentially verifying each component with
the eRoT before releasing it.

**Entry action** (runs every time the state is entered, including after recovery
re-walks):
- Reset `cursor = 0` and `awaiting = None`.
- Emit `ReadFirmware(chain[0])` + `VerifyFirmware(chain[0])` to kick off
  verification of the first component.

**Handlers**:

- `VerificationPassed(id)`:
  1. Emit `ReleaseReset(id)`.
  2. Check the **current** component's `ComponentKind` (the one that just
     passed — `chain[cursor]`).
  3. If there are more components (`cursor + 1 < chain.len()`):
     - Advance `cursor`.
     - Speculatively emit `ReadFirmware(next)` + `VerifyFirmware(next)` —
       the next eRoT check starts concurrently while the current component boots.
     - If current kind is `Active`: set `awaiting = Some(id)`, transition to
       `AwaitingReady`. The chain walk pauses until the iRoT confirms readiness.
     - If current kind is `Passive`: return `Handled` (stay in
       `VerifyingPlatform`). The walk advances immediately.
  4. If chain is done: transition to `Ready`.

- `VerificationFailed(id)`:
  - Set `rot.failed = Some(id)`, transition to `Recovering`.

**Why `Handled` and not a self-transition**: a self-transition in statig runs the
entry action again, which resets `cursor = 0` and re-emits `ReadFirmware` for
the first component. The chain walk uses `Handled` deliberately to advance
without resetting the cursor. The cursor lives in `Rot` shared storage so it
persists across events.

### `AwaitingReady`

The eRoT has released an `Active` component from reset. The chain walk is
paused, waiting for that component's integrated iRoT to finish its local
verification. Meanwhile, the eRoT check for the *next* component has already
been kicked off speculatively (the `ReadFirmware` + `VerifyFirmware` for
`chain[cursor]` were emitted before transitioning here).

**Entry action**: none (the transition from `VerifyingPlatform` sets `awaiting`
before entering).

**Handlers**:

- `ComponentReady(id)`:
  - If `id != rot.awaiting`: spurious or stale signal — ignore (`Handled`,
    INV9). The walk does not advance.
  - Otherwise: clear `rot.awaiting = None`, return `Handled`. The machine stays
    in `AwaitingReady`, waiting for the speculative eRoT `VerifyFirmware` result.

- `VerificationPassed(id)`:
  - The speculatively-read next component has passed its eRoT check.
  - Emit `ReleaseReset(id)`.
  - If more components remain: advance `cursor`, emit `ReadFirmware` +
    `VerifyFirmware` for the next, return `Handled`.
  - If chain done: transition to `Ready`.

- `VerificationFailed(id)`:
  - Clear `rot.awaiting`, set `rot.failed = Some(id)`, transition to
    `Recovering`.

**Invariant**: `awaiting` is `Some` only while in `AwaitingReady` (INV9).
It is always cleared before leaving this state.

---

## 4. The Speculative Read Pattern

When an `Active` component passes eRoT verification the core does three things
in the same handler, before transitioning to `AwaitingReady`:

```
emit ReleaseReset(current)
emit ReadFirmware(next)        ← speculative: next eRoT check starts immediately
emit VerifyFirmware(next)      ← while current's iRoT is still booting
cursor += 1
awaiting = Some(current)
→ Transition(AwaitingReady)
```

This overlaps the integrated iRoT boot time of the current component with the
eRoT firmware read of the next. The two checks are independent (different
hardware paths), so the overlap is safe. The machine only acts on the
`VerificationPassed` for the next component once it arrives in `AwaitingReady`.

---

## 5. Sequencing by `ComponentKind`

```
chain: [(C0, Active), (C1, Passive)]

VerifyingPlatform (entry):
  emit ReadFirmware(C0)
  emit VerifyFirmware(C0)

VerificationPassed(C0):           ← eRoT check done
  emit ReleaseReset(C0)
  emit ReadFirmware(C1)           ← speculative eRoT check of next
  emit VerifyFirmware(C1)         ← speculative eRoT check of next
  cursor = 1
  awaiting = Some(C0)
  → AwaitingReady

ComponentReady(C0):               ← C0's integrated iRoT done
  awaiting = None
  Handled (stay in AwaitingReady, wait for VerificationPassed(C1))

VerificationPassed(C1):           ← speculative eRoT check resolved
  emit ReleaseReset(C1)
  chain done → Ready
```

For a `Passive` component (no integrated iRoT) the `ComponentReady` gate is absent:

```
VerificationPassed(C0):           ← Passive
  emit ReleaseReset(C0)
  emit ReadFirmware(C1)
  emit VerifyFirmware(C1)
  cursor = 1
  Handled (stay in VerifyingPlatform)

VerificationPassed(C1):
  emit ReleaseReset(C1)
  chain done → Ready
```

---

## 6. The Board Boundary

The core never reads flash, never checks signatures, never observes reset lines.
It only emits descriptions. The complete split:

| Responsibility | Core (`src/lib.rs`) | Board (`Platform` impl) |
|---|---|---|
| Chain order and `ComponentKind` | reads from `Rot.chain`, set by board at startup | decides and provides |
| Read firmware image | emits `ReadFirmware(id)` | executes: eRoT reads via SPI interposition, I3C, or other transport |
| Verify signature / SVN | emits `VerifyFirmware(id)` | executes: eRoT checks against RIM/PFM; responds with `VerificationPassed` or `VerificationFailed` |
| Release from reset | emits `ReleaseReset(id)` | executes: eRoT drives reset GPIO or equivalent |
| Detect iRoT readiness | waits for `ComponentReady(id)` event | observes: integrated iRoT signals readiness (MCTP channel-up, GPIO, etc.); calls `dispatch` |
| Decide what to do on failure | transitions to `Recovering` | none — recovery policy lives in the core |

---

## 7. What This Model Does Not Cover

- **Self-verification of the eRoT firmware itself**: this happens one boot layer
  down (eRoT ROM + measuring bootloader) before this machine runs. The result is
  delivered as `PowerOnResult` in `Event::PowerGood`, not sequenced by these
  states.
- **Attestation** (`AttestationChallenge` / `SignAttestation`): a separate
  concern handled in the `Operational` superstate. Not part of the boot-time
  verification chain; not yet covered by the CSA extension.
- **Firmware update verification** (`AuthenticateUpdate`): handled in the
  `Updating` state. Distinct from boot-time chain verification.
