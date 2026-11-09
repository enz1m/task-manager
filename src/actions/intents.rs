//! Action intents: targets, acceptance, state, and close.

use super::{
    CANCELLED_BEFORE_WIRE, PRE_WIRE, PROCESS_BUDGET, SERVICE_BUDGET, WIRED, definite_signal_refusal,
};
use crate::model::PendingAction;
use crate::service_actions::{Failure, Sample, Verb};
use crate::services::{PinnedProcess, Signal, SignalFailure, Snapshot};
use enzui_core::blocking_task::{BlockingTask, BlockingTaskError};
use enzui_core::cancellation::{CancelCause, CancelToken};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Process {
        pid: u32,
        start_time_ticks: u64,
        target_uid: Option<u32>,
        signal: Signal,
    },
    Service {
        before: Sample,
        verb: Verb,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Intent {
    pub(crate) generation: u64,
    pub(crate) action: PendingAction,
    pub(crate) effective_uid: u32,
    pub(crate) target: Target,
}
impl Intent {
    pub(crate) fn description(&self) -> String {
        match &self.action {
            PendingAction::EndProcess { pid, name, .. }
            | PendingAction::KillProcess { pid, name, .. } => format!(
                "{} {}",
                crate::i18n::display(self.action.confirm_label()),
                crate::services::process_subject(pid, name)
            ),
            PendingAction::StopService { unit } | PendingAction::Service { unit, .. } => format!(
                "{} {}",
                crate::i18n::display(self.action.confirm_label()),
                crate::services::bounded_subject(unit)
            ),
        }
    }
    pub(super) fn budget(&self) -> Duration {
        match self.target {
            Target::Process { .. } => PROCESS_BUDGET,
            Target::Service { .. } => SERVICE_BUDGET,
        }
    }
    pub(super) fn matches_snapshot(&self, snapshot: &Snapshot) -> bool {
        match &self.target {
            Target::Process {
                pid,
                start_time_ticks,
                target_uid,
                ..
            } => {
                !snapshot.processes_fabricated
                    && snapshot.processes.iter().any(|row| {
                        row.pid == pid.to_string()
                            && row.start_time_ticks == *start_time_ticks
                            && row.kernel_uid == *target_uid
                            && row.action_allowed
                    })
            }
            Target::Service { before, .. } => {
                !snapshot.services_fabricated
                    && snapshot.services.iter().any(|row| {
                        row.name == before.unit
                            && row.action_allowed
                            && Sample::from_row(row).as_ref() == Ok(before)
                    })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Acceptance {
    PreCall,
    InCall,
    Accepted,
    Refused,
    SettledFailure,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Debt {
    None,
    ConfirmedAbsent,
    Uncertain,
}
impl Debt {
    pub(super) fn blocks(self) -> bool {
        self != Self::None
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Absence {
    ProcessStillPresent,
    ServiceState { active: String, sub: String },
}
impl Absence {
    pub(super) fn label(&self) -> String {
        let label: String = match self {
            Self::ProcessStillPresent => "the retained process is still present".into(),
            Self::ServiceState { active, sub } => crate::i18n::message(
                "tm-the-exact-unit-remains-active-sub-7f6943",
                &[
                    ("arg0", crate::i18n::argument(&(active))),
                    ("arg1", crate::i18n::argument(&(sub))),
                ],
            ),
        };
        crate::i18n::display(label)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Busy,
    Running,
    Reconciling,
    CancelledPending,
    Succeeded,
    Cancelled,
    CancelledReconciled,
    DeadlineReconciled,
    ProcessReconciled(SignalFailure),
    ServiceReconciled(Failure),
    TimedOut,
    ConfirmedAbsent(Absence),
    AcknowledgedFailure(Absence),
    Uncertain,
    ObservedWithoutResult,
    Failed(SignalFailure),
    CredentialsChanged,
    ServiceFailed(Failure),
    ServiceUncertain(Failure),
    Unavailable,
    Exhausted,
}
impl State {
    pub(super) fn debt(&self) -> Debt {
        match self {
            Self::ConfirmedAbsent(_) => Debt::ConfirmedAbsent,
            Self::Uncertain | Self::ServiceUncertain(_) => Debt::Uncertain,
            _ => Debt::None,
        }
    }
    pub(super) fn label(&self) -> String {
        let label: String = match self {
            Self::Busy => "Busy: exact action retained; Retry when capacity is available".into(),
            Self::Running => "Action owned: verifying captured identity and prior state".into(),
            Self::Reconciling => {
                "Mutation wired: awaiting command exit and authoritative state".into()
            }
            Self::CancelledPending => {
                "Interruption requested: not undone; occupancy and reconciliation retained".into()
            }
            Self::Succeeded => {
                "Succeeded: command returned successfully and target consequence verified".into()
            }
            Self::Cancelled => "Cancelled before mutation was wired".into(),
            Self::CancelledReconciled => {
                "Cancelled, not undone: target consequence verified".into()
            }
            Self::DeadlineReconciled => "Deadline elapsed; target consequence verified".into(),
            Self::ProcessReconciled(failure) => crate::i18n::message(
                "tm-target-exit-verified-no-success-claimed-0e807c",
                &[(
                    "arg0",
                    crate::i18n::argument(&(Self::Failed(failure.clone()).label())),
                )],
            ),
            Self::ServiceReconciled(failure) => crate::i18n::message(
                "tm-service-command-target-consequence-verified-no-succe-8a6ebc",
                &[("arg0", crate::i18n::argument(&(failure.label())))],
            ),
            Self::TimedOut => "Deadline exceeded; no success claimed".into(),
            Self::ConfirmedAbsent(absence) => crate::i18n::message(
                "tm-confirmed-failure-retry-observation-or-acknowledge-f-e06510",
                &[("arg0", crate::i18n::argument(&(absence.label())))],
            ),
            Self::AcknowledgedFailure(absence) => crate::i18n::message(
                "tm-failure-acknowledged-nothing-was-retried-6c765b",
                &[("arg0", crate::i18n::argument(&(absence.label())))],
            ),
            Self::Uncertain => {
                "Uncertain: authoritative outcome unavailable; Retry reconciliation, not mutation"
                    .into()
            }
            Self::ObservedWithoutResult => {
                "Worker result unavailable; target consequence observed, no success claimed".into()
            }
            Self::CredentialsChanged => "Captured target UID changed; nothing signalled".into(),
            Self::ServiceFailed(failure) => crate::i18n::message(
                "tm-service-action-3d87f9",
                &[("arg0", crate::i18n::argument(&(failure.label())))],
            ),
            Self::ServiceUncertain(failure) => crate::i18n::message(
                "tm-uncertain-service-action-reconciliation-retained-10efec",
                &[("arg0", crate::i18n::argument(&(failure.label())))],
            ),
            Self::Unavailable => "Action worker unavailable".into(),
            Self::Exhausted => {
                "Action generation or Retry sequence exhausted; no further attempt admitted".into()
            }
            Self::Failed(SignalFailure::NotPermitted) => {
                "Permission denied: nothing signalled".into()
            }
            Self::Failed(SignalFailure::Gone) => "Target vanished before signal".into(),
            Self::Failed(SignalFailure::IdentityChanged) => {
                "Target reused: nothing signalled".into()
            }
            Self::Failed(SignalFailure::IdentityUnverifiable) => {
                "Target identity unavailable: nothing signalled".into()
            }
            Self::Failed(SignalFailure::Unsupported) => "Process signalling unavailable".into(),
            Self::Failed(SignalFailure::Other(_)) => "Signal failed: nothing signalled".into(),
        };
        crate::i18n::display(label)
    }
}
#[derive(Default)]
pub(super) struct Work {
    pub(super) wire: AtomicU8,
    pub(super) acceptance: AtomicU8,
    pub(super) interruption: CancelToken,
    pub(super) process_exit: Mutex<Option<Result<(), SignalFailure>>>,
    pub(super) observed_at: Mutex<Option<Instant>>,
    pub(super) pin: Mutex<Option<Box<dyn PinnedProcess>>>,
    pub(super) service_exit: Mutex<Option<Result<(), Failure>>>,
}
impl Work {
    pub(super) fn begin_wire(&self) -> bool {
        let entered = self
            .wire
            .compare_exchange(PRE_WIRE, WIRED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if entered {
            self.acceptance
                .store(Acceptance::InCall as u8, Ordering::Release);
        }
        entered
    }
    pub(super) fn acceptance(&self) -> Acceptance {
        match self.acceptance.load(Ordering::Acquire) {
            0 => Acceptance::PreCall,
            1 => Acceptance::InCall,
            2 => Acceptance::Accepted,
            3 => Acceptance::Refused,
            4 => Acceptance::SettledFailure,
            _ => Acceptance::Unknown,
        }
    }
    pub(super) fn record_signal(&self, result: Result<(), SignalFailure>) {
        let acceptance = match &result {
            Ok(()) => Acceptance::Accepted,
            Err(failure) if definite_signal_refusal(failure) => Acceptance::Refused,
            Err(_) => Acceptance::Unknown,
        };
        *self
            .process_exit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
        self.acceptance.store(acceptance as u8, Ordering::Release);
    }
    pub(super) fn record_command(&self, result: Result<(), Failure>) {
        let acceptance = match result {
            Ok(()) => Acceptance::Accepted,
            Err(
                Failure::Denied | Failure::NotFound | Failure::Unavailable | Failure::InvalidUnit,
            ) => Acceptance::Refused,
            Err(Failure::Failed | Failure::Changed) => Acceptance::SettledFailure,
            Err(_) => Acceptance::Unknown,
        };
        *self
            .service_exit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
        self.acceptance.store(acceptance as u8, Ordering::Release);
    }
    pub(super) fn cancel_pre_wire(&self, token: &CancelToken) {
        if self
            .wire
            .compare_exchange(
                PRE_WIRE,
                CANCELLED_BEFORE_WIRE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            token.cancel(CancelCause::Cancelled);
        }
    }
    pub(super) fn note_interruption(&self, token: &CancelToken) {
        if self.wired()
            && let Some(cause) = token.cause()
        {
            self.interruption.cancel(cause);
        }
    }
    pub(super) fn observed_label(&self, token: &CancelToken) -> State {
        self.note_interruption(token);
        match self.interruption.cause() {
            Some(CancelCause::Cancelled | CancelCause::Shutdown | CancelCause::Disconnected) => {
                State::CancelledReconciled
            }
            Some(CancelCause::Deadline) => State::DeadlineReconciled,
            None => State::Succeeded,
        }
    }
    pub(super) fn wired(&self) -> bool {
        self.wire.load(Ordering::Acquire) == WIRED
    }
}
pub(super) struct Report {
    pub(super) generation: u64,
    pub(super) attempt: u64,
    pub(super) sampled_at: Instant,
    pub(super) state: State,
}
pub(super) struct Flight {
    pub(super) task: Option<BlockingTask<Report>>,
    pub(super) settled: Option<Result<Report, BlockingTaskError>>,
    pub(super) started: Instant,
}
pub(super) struct Attempt {
    pub(super) intent: Intent,
    pub(super) attempt: u64,
    pub(super) state: State,
    pub(super) cancellation: CancelToken,
    pub(super) work: Arc<Work>,
    pub(super) flight: Option<Flight>,
    pub(super) retry_ready: bool,
    pub(super) debt: Debt,
    pub(super) proof_at: Option<Instant>,
}
impl Attempt {
    pub(super) fn retryable(&self) -> bool {
        self.state != State::Exhausted && (self.state == State::Busy || self.debt.blocks())
    }
    pub(super) fn label(&self) -> String {
        let cause = if self.debt.blocks() || self.flight.is_some() {
            match self
                .work
                .interruption
                .cause()
                .or_else(|| self.cancellation.cause())
            {
                Some(CancelCause::Cancelled) => "; cancellation is not an undo",
                Some(CancelCause::Deadline) => "; deadline expired",
                _ => "",
            }
        } else {
            ""
        };
        crate::i18n::message(
            "tm-cause-128d07",
            &[
                (
                    "arg0",
                    crate::i18n::argument(
                        &(match self.work.acceptance() {
                            Acceptance::PreCall => "Pre-call",
                            Acceptance::InCall => "Call entered",
                            Acceptance::Accepted => "Accepted",
                            Acceptance::Refused => "Refused",
                            Acceptance::SettledFailure => "Command failed",
                            Acceptance::Unknown => "Acceptance uncertain",
                        }),
                    ),
                ),
                ("arg1", crate::i18n::argument(&(self.state.label()))),
                ("arg2", crate::i18n::argument(&(cause))),
                (
                    "arg3",
                    crate::i18n::argument(
                        &(if self.debt.blocks() {
                            "; reconciliation debt retained; Retry only resamples"
                        } else {
                            ""
                        }),
                    ),
                ),
            ],
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClosePhase {
    Draining,
    Vetoed,
    Joining,
    Ready,
    Committed,
}
#[derive(Clone, Copy)]
pub(super) struct Close {
    pub(super) window: iced::window::Id,
    pub(super) generation: u64,
    pub(super) phase: ClosePhase,
}
