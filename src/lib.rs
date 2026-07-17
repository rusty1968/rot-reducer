//! `rot_reducer` — a hardware Root-of-Trust state machine, driven as a pure reducer.
//!
//! It models the security lifecycle of an OCP-style (Open Compute Project) Root
//! of Trust (RoT) — the hardware trust anchor that, at power-on, measures each
//! platform component's firmware, releases a component from reset only once its
//! measurement matches a known-good reference, and latches the platform into
//! lockdown if trust cannot be established. It also covers runtime attestation,
//! firmware update, and corruption recovery.
//!
//! The machine starts at *platform* verification, not self-verification: the
//! RoT's own firmware integrity and its attestation (DICE alias) identity are
//! established one boot layer down — by the immutable ROM and the measuring
//! bootloader (e.g. mcuboot) — *before* this machine runs. So "the attestation
//! key is available" is a power-on precondition carried in [`PowerOnResult`], not
//! a step the machine sequences; the core neither measures itself nor derives a
//! key.
//!
//! The machine is a *pure function* of `(state, event, shared storage)`: it
//! never touches hardware and never reads the world — it only chooses state
//! transitions and *describes* side effects as [`Effect`] values that a board /
//! shell layer carries out (see `examples/board.rs`). It is generic over an
//! opaque [`ComponentId`], so no concrete hardware appears in the core. That
//! firewall makes every run deterministic and every test an assertion over an
//! ordered `Vec<Effect>`.
//!
//! The core does no input or output of its own, so replaying the same events
//! always produces the same run. Two small conveniences bend that rule without
//! breaking it, and one keeps it strict. Three design choices in all:
//!
//!   1. **Effects go into a buffer passed in with the event.** A handler reports
//!      an effect by calling `ctx.emit(..)`, not `rot.emit(..)`. That buffer (a
//!      [`Sink`]) belongs to the orchestrator and is fresh for every event, so
//!      the machine still can't do I/O, and there is no list of effects sitting
//!      in the machine's own data that would need clearing between events.
//!   2. **A follow-up event travels as an effect.** A handler can ask for one by
//!      emitting the internal [`Effect::Emit`]; the orchestrator then dispatches
//!      that event next. Because it rides along as an ordinary effect, it shows
//!      up in the effect trace instead of being a hidden change. We use it to
//!      enforce the recovery-retry limit inside the core (INV8) rather than
//!      waiting on an outside `RecoveryFailed`.
//!   3. **The core reads nothing; answers arrive in events.** Where a decision
//!      needs outside information — such as whether the device is provisioned at
//!      power-on — the shell reads it and puts the answer in the event itself
//!      ([`Event::PowerGood`] carries [`PowerOnResult`]).
//!
//! Engine: statig 0.4.1, with the `State`/`Superstate` traits written by hand
//! (the derive macros are turned off).

#![no_std]
#![forbid(unsafe_code)]

use core::marker::PhantomData;

use statig::blocking::{
    IntoStateMachine, IntoStateMachineExt as _, State as StatigState, StateMachine,
    Superstate as StatigSuperstate,
};
use statig::Outcome;

// The core hard-codes no settings that vary by deployment. The two the board
// chooses are passed in, not fixed here:
//   * how many components the chain can hold — the `N` on [`Rot`]/[`Orchestrator`],
//     taken from the size of the chain the board hands in; and
//   * how many recovery attempts are allowed (INV8) — the `max_retry` argument.
// The board picks both (see `examples/board.rs`). The two limits just below,
// [`EFFECT_CAP`] and [`PENDING_CAP`], are different: they follow from how the
// machine itself works, not from the deployment, so they stay here in the core.

/// How many effects one event can produce. The busiest handler emits 3
/// (`ReleaseReset` + `ReadFirmware` + `VerifyFirmware`), so 8 leaves
/// plenty of room. Going over is a bug in our logic; we drop the extra rather
/// than panic, so the core never panics and stays `no_std`.
const EFFECT_CAP: usize = 8;

/// How many events can be waiting while we finish handling one outside event
/// (the original plus any [`Effect::Emit`] follow-ups). The most a handler adds
/// is one, so 8 is plenty.
const PENDING_CAP: usize = 8;

/// An identifier for one platform component. The core never looks inside it; the
/// board layer decides which real piece of hardware each id stands for, inside
/// its [`Platform`] impl.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ComponentId(u8);

impl ComponentId {
    /// Make a component id. The board layer calls this, never the core.
    pub const fn new(id: u8) -> Self {
        Self(id)
    }

    /// The raw number inside, which the board layer uses to reach hardware.
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// How components in the trust chain are classified. The board supplies one
/// [`ComponentKind`] per [`ComponentId`] when building the chain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComponentKind {
    /// The component has an integrated iRoT (e.g. Caliptra). Two independent
    /// authentication checks apply:
    ///   1. The eRoT reads the component's firmware from the flash it controls,
    ///      verifies signature and SVN via [`Effect::ReadFirmware`] /
    ///      [`Effect::VerifyFirmware`], and only then emits `ReleaseReset`.
    ///   2. After reset, the iRoT performs its own local verification. The
    ///      machine waits in [`State::AwaitingReady`] until the board signals
    ///      [`Event::ComponentReady`] before advancing the chain walk.
    Active,
    /// No local iRoT. The eRoT's authentication from flash is the only trust
    /// gate. The chain walk advances immediately after `ReleaseReset`.
    Passive,
}

/// The result of the board's power-on checks, delivered inside
/// [`Event::PowerGood`]. The shell reads OTP/UFM and performs the ROM-based
/// self-integrity check before emitting this; the core never reads anything
/// directly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PowerOnResult {
    /// Self-verified and provisioned. The lower boot layers (ROM + bootloader)
    /// have established the device's identity and attestation key.
    Provisioned,
    /// Self-verified but not provisioned — the device cannot act as a RoT.
    Unprovisioned,
    /// Self-verification failed. The machine latches to [`State::Locked`]
    /// immediately. Any A/B recovery attempt should happen in the board layer
    /// *before* emitting this event.
    SelfVerificationFailed,
}

/// Everything the outside world can tell the state machine.
///
/// `PowerGood` carries the [`PowerOnResult`]; the component-specific events
/// carry the [`ComponentId`] they are about; the rest carry nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// Power-on, carrying the shell's self-verification and provisioning read.
    PowerGood(PowerOnResult),
    VerificationPassed(ComponentId),
    VerificationFailed(ComponentId),
    /// An [`ComponentKind::Active`] component has finished its local iRoT
    /// verification and is ready (e.g. MCTP channel established). The board
    /// delivers this after observing the component become operational.
    ComponentReady(ComponentId),
    AttestationChallenge,
    UpdateRequest,
    UpdateVerified,
    UpdateRejected,
    CorruptionDetected(ComponentId),
    Restored(ComponentId),
    RecoveryFailed,
}

/// Everything the state machine can ask the outside world to do. A [`Platform`]
/// carries out all of them except [`Effect::Emit`]; the ones about a specific
/// component carry the [`ComponentId`] they act on.
///
/// [`Effect::Emit`] is the one that stays inside: it never reaches the
/// `Platform`. The orchestrator catches it and turns the carried event into the
/// next event to handle. Sending a follow-up event this way means it appears in
/// the effect trace like everything else, instead of being a hidden change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    /// Ask the board to read the named component's firmware image from the
    /// flash the eRoT controls (SPI interposition or equivalent).
    ReadFirmware(ComponentId),
    /// Ask the board to verify the measured image: check signature and SVN
    /// against policy. The board responds with [`Event::VerificationPassed`] on
    /// acceptance or [`Event::VerificationFailed`] on rejection.
    VerifyFirmware(ComponentId),
    ReleaseReset(ComponentId),
    SignAttestation,
    AuthenticateUpdate,
    StageUpdate,
    ActivateUpdate,
    DiscardStaged,
    RestoreGoldenImage(ComponentId),
    LatchLockdown,
    /// Stays inside: tells the orchestrator to handle this `Event` next. The
    /// orchestrator consumes it; it is never handed to a [`Platform`].
    Emit(Event),
}

/// The states the machine can be in. None of them carry any data.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    PowerOnReset,
    VerifyingPlatform,
    /// The eRoT has released an [`ComponentKind::Active`] component from reset
    /// and is waiting for that component's iRoT to finish its independent local
    /// verification and signal [`Event::ComponentReady`].
    AwaitingReady,
    Ready,
    Updating,
    Recovering,
    Locked,
}

/// A group state that several states share, so event handling common to all of
/// them lives in one place. The lifetime parameter is here only to match the
/// shape statig expects (a group state may borrow from its state); we borrow
/// nothing.
#[derive(Debug)]
pub enum Superstate<'sub> {
    Operational(PhantomData<&'sub ()>),
}

/// The little effect buffer handed to every handler (statig calls it the
/// `Context`).
///
/// This is the whole trick: the only thing a handler can do to the outside is
/// call `emit` to add an [`Effect`] — it cannot read anything or do I/O. The
/// orchestrator gives each event a fresh `Sink` and reads the effects out of it
/// afterward, so no effects are ever stored in the machine itself.
pub struct Sink {
    effects: heapless::Vec<Effect, EFFECT_CAP>,
}

impl Sink {
    fn new() -> Self {
        Self {
            effects: heapless::Vec::new(),
        }
    }

    /// Add one effect. The buffer holds more than any single event needs, so
    /// running out means a bug in our logic; we drop the effect rather than
    /// panic (see [`EFFECT_CAP`]).
    pub fn emit(&mut self, effect: Effect) {
        let _ = self.effects.push(effect);
    }

    /// The effects added while handling this event, in order.
    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }
}

/// The data that lives between events. It holds everything the machine has to
/// remember from one event to the next — most importantly the `cursor` into the
/// trust chain. A normal state-to-itself transition would reset that, so the
/// chain is walked by returning `Outcome::Handled` instead of transitioning.
///
/// Notice there is no `effects` field: the effects live in the [`Sink`] instead,
/// which is why nothing here has to be cleared before each event.
///
/// `N` is how many components the chain can hold — a board choice, not core
/// logic — so the core sets no default and just takes whatever size chain the
/// board hands in.
pub struct Rot<const N: usize> {
    /// The trust chain, in order; the board decides the order and kind at startup.
    chain: heapless::Vec<(ComponentId, ComponentKind), N>,
    /// Where we are in `chain`. Kept here so it survives across events.
    cursor: u8,
    /// The component that failed and set off recovery, if any.
    failed: Option<ComponentId>,
    /// How many recovery attempts have been made (INV8).
    retry_count: u8,
    /// How many recovery attempts are allowed before the machine locks down
    /// (INV8). The board sets this when it builds the machine.
    max_retry: u8,
    /// The active component we are waiting on in [`State::AwaitingReady`], if
    /// any. `None` in every other state (INV10).
    awaiting: Option<ComponentId>,
}

impl<const N: usize> Rot<N> {
    /// Build the machine's data from the board's trust `chain` (each entry is a
    /// `(ComponentId, ComponentKind)` pair) and its limit on recovery attempts
    /// (INV8). Both are board choices; the core keeps them but picks no default.
    pub fn new(chain: heapless::Vec<(ComponentId, ComponentKind), N>, max_retry: u8) -> Self {
        Self {
            chain,
            cursor: 0,
            failed: None,
            retry_count: 0,
            max_retry,
            awaiting: None,
        }
    }
}

impl<const N: usize> IntoStateMachine for Rot<N> {
    type Event<'evt> = Event;
    type Context<'ctx> = Sink;
    type State = State;
    type Superstate<'sub> = Superstate<'sub>;

    fn initial() -> State {
        State::PowerOnReset
    }

    // No `before_dispatch` step is needed: because effects live in the `Sink`
    // (a fresh one per event, owned by the orchestrator), there is nothing to
    // clear here between events.
}


impl<const N: usize> StatigState<Rot<N>> for State {
    fn call_handler(&mut self, rot: &mut Rot<N>, event: &Event, ctx: &mut Sink) -> Outcome<State> {
        match self {
            State::PowerOnReset => match event {
                // The shell already read provisioning and put the answer in this
                // event; the core never reads anything itself. Self-integrity and
                // identity were established below this machine (ROM + bootloader),
                // so a provisioned power-on goes straight to verifying the
                // platform components.
                Event::PowerGood(PowerOnResult::Provisioned) => {
                    Outcome::Transition(State::VerifyingPlatform)
                }
                // Not provisioned — cannot act as a root of trust.
                Event::PowerGood(PowerOnResult::Unprovisioned) => {
                    Outcome::Transition(State::Locked)
                }
                // Self-verification failed (INV12) — latch immediately.
                Event::PowerGood(PowerOnResult::SelfVerificationFailed) => {
                    Outcome::Transition(State::Locked)
                }
                _ => Outcome::Super,
            },

            // Walk the trust chain using `Handled` and the `cursor`, never a
            // state-to-itself transition (that would reset the cursor).
            State::VerifyingPlatform => match event {
                Event::VerificationPassed(id) => {
                    ctx.emit(Effect::ReleaseReset(*id));
                    // Branch on the *current* component's kind (the one that
                    // just passed), not the next: an Active component must have
                    // its own iRoT confirm readiness before we advance.
                    let current_kind = rot.chain[rot.cursor as usize].1;
                    let next_idx = (rot.cursor as usize) + 1;
                    if next_idx < rot.chain.len() {
                        let (next_id, _) = rot.chain[next_idx];
                        rot.cursor += 1;
                        // Speculatively start reading the next component while
                        // the current one's iRoT (if Active) is still booting.
                        ctx.emit(Effect::ReadFirmware(next_id));
                        ctx.emit(Effect::VerifyFirmware(next_id));
                        match current_kind {
                            // Active: wait for this component's iRoT before
                            // acting on the next verification result.
                            ComponentKind::Active => {
                                rot.awaiting = Some(*id);
                                Outcome::Transition(State::AwaitingReady)
                            }
                            // Passive: eRoT auth is the only gate — advance.
                            ComponentKind::Passive => Outcome::Handled,
                        }
                    } else {
                        Outcome::Transition(State::Ready)
                    }
                }
                Event::VerificationFailed(id) => {
                    rot.failed = Some(*id);
                    Outcome::Transition(State::Recovering)
                }
                _ => Outcome::Super,
            },

            // Waiting for an Active component's iRoT to confirm readiness.
            // The cursor already points to the next component and its
            // ReadFirmware + VerifyFirmware have already been emitted
            // speculatively; we just need to wait here until both
            // ComponentReady (iRoT done) and VerificationPassed (eRoT done)
            // have arrived before moving on.
            State::AwaitingReady => match event {
                // The board signals that the awaited component's iRoT finished
                // its local verification (INV11). Clear the latch and stay here
                // to receive the pending VerificationPassed for the next
                // component (already speculatively read).
                Event::ComponentReady(id) => {
                    if rot.awaiting != Some(*id) {
                        // Stale or spurious — ignore (INV10).
                        return Outcome::Handled;
                    }
                    rot.awaiting = None;
                    Outcome::Handled
                }
                // eRoT authentication of the speculatively-read next-in-chain
                // component completed. Release it and advance.
                Event::VerificationPassed(id) => {
                    ctx.emit(Effect::ReleaseReset(*id));
                    let next_idx = (rot.cursor as usize) + 1;
                    if next_idx < rot.chain.len() {
                        let (next_id, _) = rot.chain[next_idx];
                        rot.cursor += 1;
                        ctx.emit(Effect::ReadFirmware(next_id));
                        ctx.emit(Effect::VerifyFirmware(next_id));
                        Outcome::Handled
                    } else {
                        Outcome::Transition(State::Ready)
                    }
                }
                Event::VerificationFailed(id) => {
                    rot.failed = Some(*id);
                    rot.awaiting = None;
                    Outcome::Transition(State::Recovering)
                }
                _ => Outcome::Super,
            },

            State::Ready => match event {
                Event::UpdateRequest => Outcome::Transition(State::Updating),
                // Attestation and the corruption watch are handled by the
                // shared Operational group state.
                _ => Outcome::Super,
            },

            State::Updating => match event {
                Event::UpdateVerified => {
                    ctx.emit(Effect::ActivateUpdate);
                    Outcome::Transition(State::Ready)
                }
                // A rejected update is just undone, not treated as corruption (INV4).
                Event::UpdateRejected => {
                    ctx.emit(Effect::DiscardStaged);
                    Outcome::Transition(State::Ready)
                }
                _ => Outcome::Super,
            },

            State::Recovering => match event {
                Event::Restored(_) => {
                    rot.retry_count = rot.retry_count.saturating_add(1);
                    if rot.retry_count >= rot.max_retry {
                        // Out of attempts: hand ourselves a `RecoveryFailed`
                        // event, so the limit is enforced here in the core rather
                        // than by an outside timer. The orchestrator handles it
                        // next, sending us to Locked, and the whole thing shows
                        // up in the effect trace.
                        ctx.emit(Effect::Emit(Event::RecoveryFailed));
                        Outcome::Handled
                    } else {
                        // Start the chain over from the top to re-check trust.
                        Outcome::Transition(State::VerifyingPlatform)
                    }
                }
                Event::RecoveryFailed => Outcome::Transition(State::Locked),
                _ => Outcome::Super,
            },

            // The end state: it swallows every event and emits nothing.
            State::Locked => Outcome::Super,
        }
    }

    fn call_entry_action(&mut self, rot: &mut Rot<N>, ctx: &mut Sink) {
        match self {
            // Start the chain walk: reset cursor and awaiting, then kick off
            // the eRoT's authentication of the first component.
            State::VerifyingPlatform => {
                rot.cursor = 0;
                rot.awaiting = None;
                if let Some(&(first_id, _)) = rot.chain.first() {
                    ctx.emit(Effect::ReadFirmware(first_id));
                    ctx.emit(Effect::VerifyFirmware(first_id));
                }
            }
            // Check the update is genuine, then stage it in the spare firmware bank.
            State::Updating => {
                ctx.emit(Effect::AuthenticateUpdate);
                ctx.emit(Effect::StageUpdate);
            }
            // Restore the known-good firmware for the component that failed.
            State::Recovering => {
                if let Some(failed) = rot.failed {
                    ctx.emit(Effect::RestoreGoldenImage(failed));
                }
            }
            // Lock the platform down for good; components stay held in reset.
            State::Locked => {
                ctx.emit(Effect::LatchLockdown);
            }
            // Platform is fully verified and healthy: clear the per-episode
            // recovery tally so a future corruption starts counting from zero
            // (INV8 tracks consecutive failures, not lifetime total).
            State::Ready => {
                rot.retry_count = 0;
            }
            _ => {}
        }
    }

    // No exit actions; the default (do nothing) is fine.

    fn superstate(&mut self) -> Option<Superstate<'_>> {
        match self {
            State::Ready | State::Updating | State::Recovering | State::AwaitingReady => {
                Some(Superstate::Operational(PhantomData))
            }
            _ => None,
        }
    }
}

impl<const N: usize> StatigSuperstate<Rot<N>> for Superstate<'_> {
    fn call_handler(&mut self, rot: &mut Rot<N>, event: &Event, ctx: &mut Sink) -> Outcome<State> {
        match self {
            // These are handled the same way in every Operational state: an
            // attestation challenge, and the watch for runtime corruption.
            Superstate::Operational(_) => match event {
                // Answer an attestation challenge by signing it (INV6).
                Event::AttestationChallenge => {
                    ctx.emit(Effect::SignAttestation);
                    Outcome::Handled
                }
                // Corruption spotted while running — recover that component (INV5).
                Event::CorruptionDetected(id) => {
                    rot.failed = Some(*id);
                    Outcome::Transition(State::Recovering)
                }
                _ => Outcome::Super,
            },
        }
    }
}

/// How the core reaches the outside world: carry out one effect. This is the
/// only outward connection — the world talks back to the core only through
/// [`Event`]s (see [`PowerOnResult`]), so there is no read method here, and no
/// way to fetch events either (a loop outside the core delivers those, see
/// [`Orchestrator`]). Never called with [`Effect::Emit`].
pub trait Platform {
    /// Carry out one effect.
    fn execute(&mut self, effect: Effect);
}

/// How events get in — but only for the built-in [`run`] loop. If you run your
/// own loop and deliver events yourself, you don't implement this.
pub trait EventSource {
    /// Wait for and return the next event.
    fn next_event(&mut self) -> Event;
}

/// A handle to a running machine that a caller's **own loop** steps once per
/// event. It wraps the `statig` machine, so a caller only depends on
/// `rot_reducer` and never has to name a `statig` type.
///
/// The loop lives outside this crate; it delivers events and routes effects. The
/// board layer supplies the trust chain and names the components (see
/// `examples/board.rs`); the core works only with the opaque ids:
///
/// ```
/// use rot_reducer::{ComponentId, ComponentKind, Orchestrator, Event, PowerOnResult, State};
///
/// // The board layer's job: pick the capacity, name components, annotate kinds,
/// // order the chain, and choose the recovery-retry cap.
/// const CAPACITY: usize = 8;
/// const MAX_RETRY: u8 = 3;
/// const BMC: ComponentId = ComponentId::new(0);
/// const HOST: ComponentId = ComponentId::new(1);
/// let mut chain = heapless::Vec::<(ComponentId, ComponentKind), CAPACITY>::new();
/// let _ = chain.push((BMC, ComponentKind::Passive));
/// let _ = chain.push((HOST, ComponentKind::Passive));
///
/// let mut orch = Orchestrator::new(chain, MAX_RETRY);
/// let mut effects = Vec::new();
///
/// for ev in [
///     Event::PowerGood(PowerOnResult::Provisioned),
///     Event::VerificationPassed(BMC),
///     Event::VerificationPassed(HOST),
/// ] {
///     // one step of the caller's loop
///     orch.dispatch_with(ev, |e| effects.push(e));
///     if orch.state() == State::Locked { break; }
/// }
///
/// assert_eq!(orch.state(), State::Ready);
/// ```
pub struct Orchestrator<const N: usize> {
    machine: StateMachine<Rot<N>>,
}

impl<const N: usize> Orchestrator<N> {
    /// Build an orchestrator from the board's trust `chain` (each entry is a
    /// `(ComponentId, ComponentKind)` pair) and its limit on recovery attempts
    /// (INV8) — both board choices. The capacity `N` comes from the chain.
    /// Nothing runs yet: the first `dispatch*` runs the starting state's entry
    /// actions.
    pub fn new(chain: heapless::Vec<(ComponentId, ComponentKind), N>, max_retry: u8) -> Self {
        Self {
            machine: Rot::new(chain, max_retry).state_machine(),
        }
    }

    /// The state the machine is in right now (cheap to copy).
    pub fn state(&self) -> State {
        *self.machine.state()
    }

    /// Handle one event **all the way through**, calling `on_effect` once for
    /// each outside effect, in the order they were emitted. Any internal
    /// [`Effect::Emit`] follow-up events are handled here too (in order) before
    /// this returns, so one call fully settles the machine — including a lockdown
    /// it triggers on itself. No [`Platform`] needed: just pass a closure.
    pub fn dispatch_with(&mut self, event: Event, mut on_effect: impl FnMut(Effect)) {
        let mut pending: heapless::Vec<Event, PENDING_CAP> = heapless::Vec::new();
        let _ = pending.push(event);

        let mut i = 0;
        while i < pending.len() {
            let ev = pending[i];
            i += 1;

            let mut buf = Sink::new();
            self.machine.handle_with_context(&ev, &mut buf);

            for &effect in buf.effects() {
                match effect {
                    // Stays inside: queue it to handle next, don't send it out.
                    Effect::Emit(internal) => {
                        let _ = pending.push(internal);
                    }
                    // Goes outside: give it to the caller now, in order.
                    external => on_effect(external),
                }
            }
        }
    }

    /// Same as [`dispatch_with`], but sends each outside effect to a [`Platform`]
    /// for you.
    pub fn dispatch(&mut self, platform: &mut impl Platform, event: Event) {
        self.dispatch_with(event, |effect| platform.execute(effect));
    }
}

/// A ready-made loop for callers who want this crate to run the loop: get an
/// event, handle it all the way through, forever. If you already have your own
/// loop, hold an [`Orchestrator`] and step it yourself instead.
pub fn run<const N: usize>(
    io: &mut (impl Platform + EventSource),
    chain: heapless::Vec<(ComponentId, ComponentKind), N>,
    max_retry: u8,
) -> ! {
    let mut orch = Orchestrator::new(chain, max_retry);
    loop {
        let event = io.next_event();
        orch.dispatch(io, event);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    // Two generic components — the core never learns what they are (INV9).
    const C0: ComponentId = ComponentId::new(0);
    const C1: ComponentId = ComponentId::new(1);

    // Provisioned power-on — the common case for every trust-walk test.
    const BOOT: Event = Event::PowerGood(PowerOnResult::Provisioned);

    // Deployment policy the board owns — the core defines neither. Tests stand in
    // for the board and pick their own values.
    const CAPACITY: usize = 8;
    const MAX_RETRY: u8 = 3;

    fn chain(ids: &[(ComponentId, ComponentKind)]) -> heapless::Vec<(ComponentId, ComponentKind), CAPACITY> {
        let mut c = heapless::Vec::new();
        for &entry in ids {
            c.push(entry).expect("chain within CAPACITY");
        }
        c
    }

    fn passive(ids: &[ComponentId]) -> heapless::Vec<(ComponentId, ComponentKind), CAPACITY> {
        chain(&ids.iter().map(|&id| (id, ComponentKind::Passive)).collect::<std::vec::Vec<_>>())
    }

    /// A `Platform` that records every external `execute()` in order — exercises
    /// the `Orchestrator::dispatch(&mut impl Platform, _)` path. (The doctest on
    /// `Orchestrator` covers the closure `dispatch_with` path.)
    struct Recorder {
        recorded: Vec<Effect>,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                recorded: Vec::new(),
            }
        }
    }

    impl Platform for Recorder {
        fn execute(&mut self, effect: Effect) {
            self.recorded.push(effect);
        }
    }

    /// Drive a fresh orchestrator through `script` from an external loop (exactly
    /// how a caller would), one event per step. Returns the ordered *external*
    /// effects and the final state.
    fn drive(
        chain: heapless::Vec<(ComponentId, ComponentKind), CAPACITY>,
        script: &[Event],
    ) -> (Vec<Effect>, State) {
        let mut orch = Orchestrator::new(chain, MAX_RETRY);
        let mut platform = Recorder::new();
        for &event in script {
            orch.dispatch(&mut platform, event);
        }
        (platform.recorded, orch.state())
    }

    /// INV1/INV2/INV3: a provisioned power-on enters VerifyingPlatform; no
    /// component is released before its eRoT-side verification passes; components
    /// are verified and released in chain (trust) order.
    #[test]
    fn cold_boot_walks_chain_in_order() {
        let (effects, state) = drive(
            passive(&[C0, C1]),
            &[
                BOOT,
                Event::VerificationPassed(C0),
                Event::VerificationPassed(C1),
            ],
        );

        assert_eq!(
            effects,
            std::vec![
                Effect::ReadFirmware(C0),
                Effect::VerifyFirmware(C0),
                Effect::ReleaseReset(C0),
                Effect::ReadFirmware(C1),
                Effect::VerifyFirmware(C1),
                Effect::ReleaseReset(C1),
            ],
        );
        assert_eq!(state, State::Ready);
    }

    /// Reads-as-events: an unprovisioned power-on read (delivered *in* the event)
    /// sends the core straight to lockdown — with no reader lane anywhere.
    #[test]
    fn unprovisioned_boot_locks_down() {
        let (effects, state) = drive(
            passive(&[C0]),
            &[Event::PowerGood(PowerOnResult::Unprovisioned)],
        );

        assert_eq!(effects, std::vec![Effect::LatchLockdown]);
        assert_eq!(state, State::Locked);
    }

    /// INV6: an attestation challenge is answerable in every Operational state —
    /// proven by the `Super` bubble from both `Ready` and `Updating`.
    #[test]
    fn attestation_shared_across_operational_states() {
        // From Ready.
        let (effects, state) = drive(
            passive(&[C0]),
            &[BOOT, Event::VerificationPassed(C0), Event::AttestationChallenge],
        );
        assert_eq!(effects.last(), Some(&Effect::SignAttestation));
        assert_eq!(state, State::Ready);

        // From Updating.
        let (effects, state) = drive(
            passive(&[C0]),
            &[
                BOOT,
                Event::VerificationPassed(C0),
                Event::UpdateRequest,
                Event::AttestationChallenge,
            ],
        );
        assert_eq!(effects.last(), Some(&Effect::SignAttestation));
        assert_eq!(state, State::Updating);
    }

    /// INV4: a rejected update returns to Ready via DiscardStaged (rollback) and
    /// never enters Recovering/Locked.
    #[test]
    fn update_rollback_is_not_recovery() {
        let (effects, state) = drive(
            passive(&[C0]),
            &[
                BOOT,
                Event::VerificationPassed(C0),
                Event::UpdateRequest,
                Event::UpdateRejected,
            ],
        );

        let tail = &effects[effects.len() - 3..];
        assert_eq!(
            tail,
            &[Effect::AuthenticateUpdate, Effect::StageUpdate, Effect::DiscardStaged],
        );
        assert_eq!(state, State::Ready);
        assert!(!effects.contains(&Effect::RestoreGoldenImage(C0)));
        assert!(!effects.contains(&Effect::LatchLockdown));
    }

    /// INV5: runtime corruption targets the named component and re-walks the
    /// chain from the top after restore.
    #[test]
    fn runtime_corruption_targets_component_and_rewalks() {
        let (effects, state) = drive(
            passive(&[C0, C1]),
            &[
                BOOT,
                Event::VerificationPassed(C0),
                Event::VerificationPassed(C1),
                Event::CorruptionDetected(C1),
            ],
        );
        assert_eq!(effects.last(), Some(&Effect::RestoreGoldenImage(C1)));
        assert_eq!(state, State::Recovering);

        // After Restored, re-enter VerifyingPlatform and re-walk from C0.
        let (effects, state) = drive(
            passive(&[C0, C1]),
            &[
                BOOT,
                Event::VerificationPassed(C0),
                Event::VerificationPassed(C1),
                Event::CorruptionDetected(C1),
                Event::Restored(C1),
            ],
        );
        let tail = &effects[effects.len() - 2..];
        assert_eq!(
            tail,
            &[Effect::ReadFirmware(C0), Effect::VerifyFirmware(C0)],
        );
        assert_eq!(state, State::VerifyingPlatform);
    }

    /// Feedback-as-data (INV8 enforced in-core): after `MAX_RETRY` restore
    /// attempts the core self-emits `RecoveryFailed` and latches to `Locked`
    /// WITHOUT any external `RecoveryFailed` in the script.
    #[test]
    fn retry_cap_self_latches_via_emit() {
        // 1-component chain; cycle Recovering <-> VerifyingPlatform via mismatch.
        let mut script = std::vec![BOOT, Event::VerificationPassed(C0)];
        // Now Ready. Kick off recovery, then fail to restore MAX_RETRY times.
        script.push(Event::CorruptionDetected(C0)); // -> Recovering (retry 0)
        for _ in 0..(MAX_RETRY - 1) {
            script.push(Event::Restored(C0)); // -> re-walk VerifyingPlatform
            script.push(Event::VerificationFailed(C0)); // -> Recovering again
        }
        script.push(Event::Restored(C0)); // MAX_RETRY-th -> self-emit RecoveryFailed

        let (effects, state) = drive(passive(&[C0]), &script);

        // The script never contains an external RecoveryFailed...
        assert!(!script.contains(&Event::RecoveryFailed));
        // ...yet the core drove itself to Locked and latched lockdown.
        assert_eq!(state, State::Locked);
        assert_eq!(effects.last(), Some(&Effect::LatchLockdown));
    }

    /// A board picks the chain capacity `N` (the core names no default): here a
    /// 3-wide chain infers `Orchestrator<3>` and walks all three to `Ready`.
    #[test]
    fn custom_capacity_walks_full_chain() {
        const C2: ComponentId = ComponentId::new(2);

        let mut c = heapless::Vec::<(ComponentId, ComponentKind), 3>::new();
        for &id in &[C0, C1, C2] {
            c.push((id, ComponentKind::Passive)).expect("3 fits in N=3");
        }

        let mut orch = Orchestrator::new(c, MAX_RETRY);
        let mut effects = Vec::new();
        for ev in [
            BOOT,
            Event::VerificationPassed(C0),
            Event::VerificationPassed(C1),
            Event::VerificationPassed(C2),
        ] {
            orch.dispatch_with(ev, |e| effects.push(e));
        }

        assert_eq!(orch.state(), State::Ready);
        assert_eq!(effects.last(), Some(&Effect::ReleaseReset(C2)));
    }

    /// INV8 counts consecutive failures, not a lifetime total: after a full
    /// recovery cycle reaches Ready the tally resets, so a later unrelated
    /// corruption starts from zero and cannot prematurely latch the machine.
    #[test]
    fn retry_count_resets_after_successful_recovery() {
        // max_retry = 2 so one retry succeeds, but two in a row would latch.
        let mut c = heapless::Vec::<(ComponentId, ComponentKind), CAPACITY>::new();
        c.push((C0, ComponentKind::Passive)).expect("fits");
        let mut orch = Orchestrator::new(c, 2);
        let mut effects = Vec::new();

        // First episode: corrupt → restore → re-walk → Ready (retry_count resets).
        for ev in [
            BOOT,
            Event::VerificationPassed(C0),  // → Ready
            Event::CorruptionDetected(C0),  // → Recovering
            Event::Restored(C0),            // retry_count = 1 < 2 → VerifyingPlatform
            Event::VerificationPassed(C0),  // → Ready  (retry_count reset to 0)
        ] {
            orch.dispatch_with(ev, |e| effects.push(e));
        }
        assert_eq!(orch.state(), State::Ready);

        // Second episode: corrupt again — must NOT latch despite the earlier retry.
        let start = effects.len();
        for ev in [
            Event::CorruptionDetected(C0),  // → Recovering (retry_count was 0)
            Event::Restored(C0),            // retry_count = 1 < 2 → VerifyingPlatform
            Event::VerificationPassed(C0),  // → Ready
        ] {
            orch.dispatch_with(ev, |e| effects.push(e));
        }
        assert_eq!(orch.state(), State::Ready);
        assert!(!effects[start..].contains(&Effect::LatchLockdown));
    }

    /// A board supplies the recovery-retry cap (the core names no default). With
    /// `max_retry = 1` the FIRST failed restore self-latches to `Locked` — where
    /// a cap of 3 would instead re-walk the chain.
    #[test]
    fn custom_retry_cap_latches_sooner() {
        let mut c = heapless::Vec::<(ComponentId, ComponentKind), CAPACITY>::new();
        c.push((C0, ComponentKind::Passive)).expect("1 fits");

        let mut orch = Orchestrator::new(c, 1);
        let mut effects = Vec::new();
        for ev in [
            BOOT,
            Event::VerificationPassed(C0),
            Event::CorruptionDetected(C0), // -> Recovering (retry 0)
            Event::Restored(C0),           // retry 1 >= 1 -> self-latch to Locked
        ] {
            orch.dispatch_with(ev, |e| effects.push(e));
        }

        assert_eq!(orch.state(), State::Locked);
        assert_eq!(effects.last(), Some(&Effect::LatchLockdown));
    }

    // ----- New invariant tests (INV10–INV13) -----

    /// INV12: PowerGood(SelfVerificationFailed) latches to Locked immediately
    /// without ever entering VerifyingPlatform.
    #[test]
    fn self_verification_failure_latches_immediately() {
        let (effects, state) = drive(
            passive(&[C0]),
            &[Event::PowerGood(PowerOnResult::SelfVerificationFailed)],
        );
        assert_eq!(effects, std::vec![Effect::LatchLockdown]);
        assert_eq!(state, State::Locked);
    }

    /// INV11: an Active component gates the chain walk — the cursor does not
    /// advance and the next component's measurement does not begin until
    /// ComponentReady arrives.
    #[test]
    fn active_component_gates_on_component_ready() {
        // Two-component chain: C0 Active, C1 Passive.
        let (effects, state) = drive(
            chain(&[(C0, ComponentKind::Active), (C1, ComponentKind::Passive)]),
            &[
                BOOT,
                Event::VerificationPassed(C0), // eRoT auth passes → ReleaseReset(C0), enter AwaitingReady
                // No ComponentReady yet — machine stays in AwaitingReady.
            ],
        );
        assert_eq!(state, State::AwaitingReady);
        // Effect trace: read+verify C0 (entry), then release C0 and
        // start reading+verifying C1 (handler emits these speculatively).
        assert!(effects.contains(&Effect::ReleaseReset(C0)));
        assert!(effects.contains(&Effect::ReadFirmware(C1)));

        // Now deliver ComponentReady(C0) — chain walk should advance.
        let (effects2, state2) = drive(
            chain(&[(C0, ComponentKind::Active), (C1, ComponentKind::Passive)]),
            &[
                BOOT,
                Event::VerificationPassed(C0),
                Event::ComponentReady(C0),
                // C1 is Passive — machine should be in VerifyingPlatform waiting for VerificationPassed(C1).
                Event::VerificationPassed(C1),
            ],
        );
        assert_eq!(state2, State::Ready);
        assert!(effects2.contains(&Effect::ReleaseReset(C1)));
    }

    /// INV10: a ComponentReady that does not match the awaited component is
    /// silently ignored; the walk does not advance.
    #[test]
    fn spurious_component_ready_is_ignored() {
        let (effects, state) = drive(
            chain(&[(C0, ComponentKind::Active), (C1, ComponentKind::Passive)]),
            &[
                BOOT,
                Event::VerificationPassed(C0),       // → AwaitingReady(C0)
                Event::ComponentReady(C1),          // wrong id — should be ignored
            ],
        );
        // Still waiting for C0.
        assert_eq!(state, State::AwaitingReady);
        // C1 was not released.
        assert!(!effects.contains(&Effect::ReleaseReset(C1)));
    }

    /// INV13: AttestationChallenge is handled in AwaitingReady exactly as in
    /// Ready/Updating/Recovering — no state change.
    #[test]
    fn attestation_in_awaiting_ready() {
        let (effects, state) = drive(
            chain(&[(C0, ComponentKind::Active), (C1, ComponentKind::Passive)]),
            &[
                BOOT,
                Event::VerificationPassed(C0),   // → AwaitingReady
                Event::AttestationChallenge,
            ],
        );
        assert_eq!(state, State::AwaitingReady);
        assert_eq!(effects.last(), Some(&Effect::SignAttestation));
    }
}
