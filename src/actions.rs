use crate::model::PendingAction;
use crate::service_actions::{self, Failure, Sample, Verb};
use crate::services::{ActionOutcome, ProcessSignalBackend, SignalFailure, Snapshot};
use enzui_core::blocking_task::{BlockingShutdown, BlockingTaskError, BlockingTaskOwner};
use enzui_core::cancellation::{CancelCause, CancelToken};
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

const PROCESS_BUDGET: Duration = Duration::from_secs(3);
const SERVICE_BUDGET: Duration = Duration::from_secs(15);
const SAMPLE_BUDGET: Duration = Duration::from_secs(2);
const FRESHNESS: Duration = Duration::from_secs(3);
const SAMPLE_PAUSE: Duration = Duration::from_millis(50);
const SAMPLE_ATTEMPTS: usize = 20;
const OBSERVATION_BUDGET: Duration = Duration::from_secs(2);
const PRE_WIRE: u8 = 0;
const CANCELLED_BEFORE_WIRE: u8 = 1;
const WIRED: u8 = 2;

#[path = "actions/execution.rs"]
mod execution;

use execution::{execute_process, execute_service};
pub(crate) use execution::{notices, with_actions};
pub(crate) use intents::{Acceptance, Debt, Intent, State, Target};
use intents::{Attempt, Close, ClosePhase, Flight, Report, Work};
pub(crate) use retirement::{JoinFault, RetirementStatus};
use retirement::{RetirementAdmission, RetirementLease, RetirementPool};
#[path = "actions/intents.rs"]
mod intents;
#[path = "actions/retirement.rs"]
mod retirement;

pub(crate) struct Actions {
    sequence: u64,
    pub(crate) confirmation: Option<Intent>,
    pending: Option<Intent>,
    pending_retry_ready: bool,
    current: Option<Attempt>,
    owner: Option<RetirementLease>,
    backend: Arc<dyn ProcessSignalBackend>,
    services: Arc<dyn service_actions::Backend>,
    closing: Option<Close>,
    close_sequence: u64,
    notice: Option<String>,
    shutdown_unverified: bool,
    joined_workers: Option<usize>,
}
impl std::fmt::Debug for Actions {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("Actions")
            .field("sequence", &self.sequence)
            .finish()
    }
}
impl Default for Actions {
    fn default() -> Self {
        Self::with_backend(Arc::new(crate::services::LinuxProcessSignalBackend))
    }
}
impl Actions {
    pub(crate) fn with_backend(backend: Arc<dyn ProcessSignalBackend>) -> Self {
        Self::with_backends(backend, Arc::new(service_actions::System))
    }
    pub(crate) fn with_backends(
        backend: Arc<dyn ProcessSignalBackend>,
        services: Arc<dyn service_actions::Backend>,
    ) -> Self {
        Self {
            sequence: 0,
            confirmation: None,
            pending: None,
            pending_retry_ready: false,
            current: None,
            owner: None,
            backend,
            services,
            closing: None,
            close_sequence: 0,
            notice: None,
            shutdown_unverified: false,
            joined_workers: None,
        }
    }
    pub(crate) fn capture(&mut self, action: &PendingAction, snapshot: &Snapshot) -> bool {
        if self.is_closing()
            || self.shutdown_unverified
            || self.pending.is_some()
            || self
                .current
                .as_ref()
                .is_some_and(|a| a.state == State::Busy)
        {
            return false;
        }
        let Some(generation) = self.sequence.checked_add(1) else {
            self.notice = Some(State::Exhausted.label());
            return false;
        };
        let target = match action {
            PendingAction::EndProcess {
                pid,
                start_time_ticks,
                kernel_uid,
                ..
            }
            | PendingAction::KillProcess {
                pid,
                start_time_ticks,
                kernel_uid,
                ..
            } => {
                let (Ok(number), Some(signal), Some(row)) = (
                    pid.parse::<u32>(),
                    crate::process_action_of(action).map(crate::services::ProcessAction::signal),
                    snapshot.processes.iter().find(|row| {
                        &row.pid == pid
                            && row.start_time_ticks == *start_time_ticks
                            && row.kernel_uid == *kernel_uid
                    }),
                ) else {
                    return false;
                };
                if *start_time_ticks == 0 {
                    return false;
                }
                Target::Process {
                    pid: number,
                    start_time_ticks: *start_time_ticks,
                    target_uid: row.kernel_uid,
                    signal,
                }
            }
            PendingAction::StopService { unit } | PendingAction::Service { unit, .. } => {
                let Some(row) = snapshot.services.iter().find(|row| &row.name == unit) else {
                    return false;
                };
                let before = match Sample::from_row(row) {
                    Ok(before) => before,
                    Err(failure) => {
                        self.notice = Some(failure.label().into());
                        return false;
                    }
                };
                let verb = match action {
                    PendingAction::Service { verb, .. } => *verb,
                    _ => Verb::Stop,
                };
                Target::Service { before, verb }
            }
        };
        let intent = Intent {
            generation,
            action: action.clone(),
            effective_uid: rustix::process::geteuid().as_raw(),
            target,
        };
        if !intent.matches_snapshot(snapshot) {
            return false;
        }
        self.sequence = generation;
        self.notice = None;
        self.confirmation = Some(intent);
        true
    }
    pub(crate) fn confirmation_matches(&self, action: &PendingAction, snapshot: &Snapshot) -> bool {
        !self.is_closing()
            && self
                .confirmation
                .as_ref()
                .is_some_and(|intent| &intent.action == action && intent.matches_snapshot(snapshot))
    }
    pub(crate) fn confirm(&mut self, action: &PendingAction) {
        if self.is_closing()
            || !self
                .confirmation
                .as_ref()
                .is_some_and(|intent| &intent.action == action)
        {
            return;
        }
        let Some(intent) = self.confirmation.take() else {
            return;
        };
        if self.blocks_close() {
            self.pending = Some(intent);
            self.pending_retry_ready = false;
        } else {
            self.begin(intent);
        }
    }
    fn begin(&mut self, intent: Intent) {
        self.current = Some(Attempt {
            intent,
            attempt: 1,
            state: State::Busy,
            cancellation: CancelToken::new(),
            work: Arc::new(Work::default()),
            flight: None,
            retry_ready: false,
            debt: Debt::None,
            proof_at: None,
        });
        self.submit();
    }
    fn submit(&mut self) {
        let Some(current) = self.current.as_mut() else {
            return;
        };
        if self.owner.is_none()
            || (self.closing.is_none()
                && self.owner.as_ref().is_some_and(RetirementLease::clean_join))
        {
            let pool = RetirementPool::global();
            let reservation = pool
                .ok_or(RetirementAdmission::Unavailable)
                .and_then(|pool| pool.reserve());
            let lease = match reservation {
                Ok(lease) => lease,
                Err(RetirementAdmission::Full) => {
                    current.state = State::Busy;
                    current.retry_ready = false;
                    self.notice = Some(
                        "Action retirement capacity is full; exact action retained. Retry later."
                            .into(),
                    );
                    return;
                }
                Err(RetirementAdmission::Unavailable) => {
                    current.state = State::Unavailable;
                    self.notice = Some(
                        "Action retirement worker unavailable; no action owner created".into(),
                    );
                    return;
                }
            };
            let one = NonZeroUsize::new(1).expect("nonzero action capacity");
            let Ok(owner) = BlockingTaskOwner::new("tm-action", one, one) else {
                current.state = State::Unavailable;
                return;
            };
            lease.install(owner);
            self.owner = Some(lease);
            self.joined_workers = None;
        }
        let Some(owner) = self.owner.as_ref() else {
            return;
        };
        self.notice = None;
        let intent = current.intent.clone();
        let attempt = current.attempt;
        let cancellation = current.cancellation.clone();
        let work = Arc::clone(&current.work);
        *work
            .observed_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let reconciliation_only = current.debt.blocks() || current.work.wired();
        let backend = Arc::clone(&self.backend);
        let services = Arc::clone(&self.services);
        let started = Instant::now();
        let task = owner.submit(
            intent.generation,
            crate::i18n::in_worker(move |_| {
                let state = match &intent.target {
                    Target::Process { .. } => execute_process(
                        &intent,
                        started,
                        &cancellation,
                        &work,
                        backend.as_ref(),
                        reconciliation_only,
                    ),
                    Target::Service { before, verb } => execute_service(
                        &intent,
                        before,
                        *verb,
                        started,
                        &cancellation,
                        &work,
                        services.as_ref(),
                        reconciliation_only,
                    ),
                };
                let sampled_at = work
                    .observed_at
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .unwrap_or_else(Instant::now);
                Report {
                    generation: intent.generation,
                    attempt,
                    sampled_at,
                    state,
                }
            }),
        );
        current.state = State::Running;
        current.proof_at = None;
        current.retry_ready = false;
        let Some(task) = task else {
            current.state = State::Unavailable;
            return;
        };
        current.flight = Some(Flight {
            task: Some(task),
            settled: None,
            started,
        });
    }
    pub(crate) fn cancel(&mut self, generation: u64, attempt: u64) {
        if attempt == 0
            && self
                .pending
                .as_ref()
                .is_some_and(|intent| intent.generation == generation)
        {
            self.pending = None;
            return;
        }
        let Some(current) = self
            .current
            .as_mut()
            .filter(|a| a.intent.generation == generation && a.attempt == attempt)
        else {
            return;
        };
        if self.closing.is_some() {
            current.work.cancel_pre_wire(&current.cancellation);
            return;
        }
        current.work.cancel_pre_wire(&current.cancellation);
        current.cancellation.cancel(CancelCause::Cancelled);
        current.work.note_interruption(&current.cancellation);
        if current.flight.is_some() {
            current.state = State::CancelledPending;
        } else if current.state == State::Busy && !current.debt.blocks() {
            current.state = State::Cancelled;
        }
    }
    pub(crate) fn retry(&mut self, generation: u64, attempt: u64) {
        if self.is_closing() && !self.current.as_ref().is_some_and(|a| a.debt.blocks()) {
            return;
        }
        if attempt == 0
            && self.pending_retry_ready
            && !self.blocks_close()
            && self
                .pending
                .as_ref()
                .is_some_and(|intent| intent.generation == generation)
        {
            if let Some(intent) = self.pending.take() {
                self.begin(intent);
            }
            return;
        }
        let Some(current) = self
            .current
            .as_mut()
            .filter(|a| a.intent.generation == generation && a.attempt == attempt)
        else {
            return;
        };
        if !current.retry_ready || current.flight.is_some() || !current.retryable() {
            return;
        }
        let Some(attempt) = current.attempt.checked_add(1) else {
            current.state = State::Exhausted;
            return;
        };
        current.attempt = attempt;
        current.cancellation = CancelToken::new();
        if let Some(close) = self.closing.as_mut() {
            close.phase = ClosePhase::Draining;
        }
        self.submit();
    }
    pub(crate) fn acknowledge(&mut self, generation: u64, attempt: u64) {
        if self.live() != 0 {
            return;
        }
        let Some(current) = self.current.as_mut().filter(|current| {
            current.intent.generation == generation
                && current.attempt == attempt
                && current.flight.is_none()
        }) else {
            return;
        };
        let State::ConfirmedAbsent(absence) = &current.state else {
            return;
        };
        if current.debt != Debt::ConfirmedAbsent {
            return;
        }
        if !matches!(
            current.work.acceptance(),
            Acceptance::Accepted | Acceptance::SettledFailure
        ) {
            current.state = State::Uncertain;
            current.debt = Debt::Uncertain;
            current.proof_at = None;
            self.notice =
                Some("Command settlement is unverified; failure cannot be acknowledged".into());
            return;
        }
        if current.proof_at.is_none_or(|at| at.elapsed() > FRESHNESS) {
            current.state = State::Uncertain;
            current.debt = Debt::Uncertain;
            current.proof_at = None;
            self.notice = Some(
                "Failure observation expired; Retry reconciliation before acknowledging".into(),
            );
            return;
        }
        current.state = State::AcknowledgedFailure(absence.clone());
        current.debt = Debt::None;
        current.proof_at = None;
        self.closing = None;
        self.notice = None;
        self.pending_retry_ready = false;
    }
    pub(crate) fn live(&self) -> usize {
        self.owner.as_ref().map_or(0, RetirementLease::live)
    }
    pub(crate) fn blocks_close(&self) -> bool {
        self.live() > 0
            || self
                .current
                .as_ref()
                .is_some_and(|a| a.flight.is_some() || a.debt.blocks())
    }
    pub(crate) fn needs_wake(&self) -> bool {
        self.closing
            .is_some_and(|close| matches!(close.phase, ClosePhase::Draining | ClosePhase::Joining))
            || (self
                .closing
                .is_some_and(|close| close.phase == ClosePhase::Vetoed)
                && self
                    .owner
                    .as_ref()
                    .is_some_and(|owner| owner.receipt.status() != RetirementStatus::Reserved))
            || (self.pending.is_some() && !self.pending_retry_ready && !self.blocks_close())
            || self
                .current
                .as_ref()
                .is_some_and(|a| a.flight.is_some() || (!a.retry_ready && a.retryable()))
    }
    pub(crate) fn poll(&mut self) -> Option<ActionOutcome> {
        self.pending_retry_ready =
            self.pending.is_some() && !self.blocks_close() && !self.is_closing();
        let live = self.live();
        let current = self.current.as_mut()?;
        let Some(flight) = current.flight.as_mut() else {
            current.retry_ready = live == 0;
            return None;
        };
        if !current.work.wired() && flight.started.elapsed() >= current.intent.budget() {
            current.cancellation.cancel(CancelCause::Deadline);
        }
        if let Some(task) = flight.task.as_mut()
            && let Poll::Ready(result) =
                Pin::new(task).poll(&mut Context::from_waker(Waker::noop()))
        {
            flight.settled = Some(result.into_result());
            flight.task = None;
        }
        if current.work.wired() {
            current.work.note_interruption(&current.cancellation);
            current.debt = Debt::Uncertain;
            current.state = if current.cancellation.is_terminal() {
                State::CancelledPending
            } else {
                State::Reconciling
            };
        }
        // Future settlement is not worker exit; retain the slot and result until both agree.
        if live != 0 {
            return None;
        }
        let result = flight.settled.take()?;
        let started = flight.started;
        current.flight = None;
        current.proof_at = match &result {
            Ok(report)
                if report.generation == current.intent.generation
                    && report.attempt == current.attempt
                    && report.sampled_at >= started
                    && report.sampled_at.elapsed() <= FRESHNESS
                    && matches!(report.state, State::ConfirmedAbsent(_)) =>
            {
                Some(report.sampled_at)
            }
            _ => None,
        };
        current.state = match result {
            Ok(report)
                if report.generation != current.intent.generation
                    || report.attempt != current.attempt
                    || report.sampled_at < started =>
            {
                State::Uncertain
            }
            Ok(report)
                if report.sampled_at.elapsed() > FRESHNESS
                    && ((current.work.wired()
                        && current.work.acceptance() != Acceptance::Refused)
                        || report.state.debt().blocks()) =>
            {
                State::Uncertain
            }
            Ok(report) if report.state == State::Succeeded => {
                current.work.observed_label(&current.cancellation)
            }
            Ok(report) => report.state,
            Err(BlockingTaskError::Busy)
                if current.work.wire.load(Ordering::Acquire) == CANCELLED_BEFORE_WIRE =>
            {
                State::Cancelled
            }
            Err(BlockingTaskError::Busy) => State::Busy,
            Err(BlockingTaskError::Shutdown) if !current.work.wired() => State::Unavailable,
            Err(_) if matches!(current.intent.target, Target::Service { .. }) => {
                current.work.record_command(Err(Failure::CleanupPending));
                State::ServiceUncertain(Failure::CleanupPending)
            }
            Err(_) => State::Uncertain,
        };
        current.debt = current.state.debt();
        let text = format!("{} — {}", current.label(), current.intent.description());
        Some(if current.state == State::Succeeded {
            ActionOutcome::info(crate::i18n::message(
                "tm-completed-de6a1c",
                &[(
                    "arg0",
                    crate::i18n::argument(&(current.intent.description())),
                )],
            ))
        } else {
            ActionOutcome::warning(text)
        })
    }
    pub(crate) fn is_closing(&self) -> bool {
        self.closing.is_some()
    }
    pub(crate) fn start_close(&mut self, window: iced::window::Id) -> bool {
        if self.closing.is_some() {
            return false;
        }
        let Some(generation) = self.close_sequence.checked_add(1) else {
            self.notice = Some(State::Exhausted.label());
            return false;
        };
        self.close_sequence = generation;
        self.closing = Some(Close {
            window,
            generation,
            phase: ClosePhase::Draining,
        });
        self.confirmation = None;
        self.pending = None;
        if let Some(current) = self.current.as_mut() {
            current.work.cancel_pre_wire(&current.cancellation);
            if current.state == State::Busy
                && current.flight.is_none()
                && !current.work.wired()
                && !current.debt.blocks()
            {
                current.state = State::Cancelled;
            }
        }
        true
    }
    pub(crate) fn keep_open(&mut self, generation: u64) {
        if self.closing.is_some_and(|close| {
            close.generation == generation && close.phase == ClosePhase::Vetoed
        }) {
            self.closing = None;
        }
    }
    pub(crate) fn retry_close(&mut self, generation: u64) {
        if !self.closing.is_some_and(|close| {
            close.generation == generation && close.phase == ClosePhase::Vetoed
        }) {
            return;
        }
        if let Some(current) = &self.current
            && current.debt.blocks()
        {
            if current.flight.is_none() && current.retry_ready && current.retryable() {
                self.retry(current.intent.generation, current.attempt);
            } else {
                self.notice = Some(
                    if current.flight.is_some() {
                        "Reconciliation is still running; Close remains pending"
                    } else if current.state == State::Exhausted {
                        "Reconciliation retry sequence exhausted; Keep Open"
                    } else {
                        "Reconciliation Retry becomes available on the next wake"
                    }
                    .into(),
                );
            }
            return;
        }
        if self.shutdown_unverified {
            self.notice = Some("A clean exact-join receipt is unavailable; Close cannot be acknowledged. Keep Open.".into());
            return;
        }
        if let Some(close) = self.closing.as_mut() {
            close.phase = ClosePhase::Draining;
        }
    }
    pub(crate) fn advance_close(&mut self) -> Option<iced::window::Id> {
        let mut close = self.closing?;
        if close.phase == ClosePhase::Committed {
            return None;
        }
        if let Some(result) = self.owner.as_ref().and_then(RetirementLease::poll_join) {
            close.phase = match result {
                Ok(BlockingShutdown::Joined {
                    workers,
                    panicked: 0,
                }) if !self.shutdown_unverified
                    && self.owner.as_ref().is_some_and(RetirementLease::clean_join) =>
                {
                    self.joined_workers = Some(workers);
                    ClosePhase::Ready
                }
                Ok(BlockingShutdown::Joined { panicked, .. }) => {
                    self.shutdown_unverified = true;
                    self.notice = Some(crate::i18n::message(
                        "tm-action-workers-joined-with-panicked-worker-panics-or-bc839c",
                        &[("arg0", crate::i18n::argument(&(panicked)))],
                    ));
                    ClosePhase::Vetoed
                }
                Err(JoinFault::HelperFailedAfterTake) => {
                    self.shutdown_unverified = true;
                    self.notice = Some("Action join helper failed after taking its owner; custody remains unknown and its slot cannot be reused. Keep Open.".into());
                    ClosePhase::Vetoed
                }
                Err(JoinFault::ReaperFailed) => {
                    self.shutdown_unverified = true;
                    self.notice = Some("Action retirement reaper stopped; reserved owners and helpers remain frozen. Close is vetoed. Keep Open.".into());
                    ClosePhase::Vetoed
                }
                _ => {
                    self.shutdown_unverified = true;
                    self.notice = Some("Action owner join fault; no clean exact-join receipt authorizes Close. Keep Open.".into());
                    ClosePhase::Vetoed
                }
            };
            self.closing = Some(close);
        }
        if close.phase == ClosePhase::Joining {
            return None;
        }
        if self.shutdown_unverified {
            close.phase = ClosePhase::Vetoed;
            self.closing = Some(close);
            return None;
        }
        if close.phase == ClosePhase::Ready {
            return Some(close.window);
        }
        if self.blocks_close() {
            close.phase = if self
                .current
                .as_ref()
                .is_some_and(|a| a.flight.is_none() && a.debt.blocks())
            {
                ClosePhase::Vetoed
            } else {
                ClosePhase::Draining
            };
            self.closing = Some(close);
            return None;
        }
        if close.phase == ClosePhase::Vetoed {
            return None;
        }
        let Some(owner) = self.owner.as_ref() else {
            self.joined_workers = Some(0);
            close.phase = ClosePhase::Ready;
            self.closing = Some(close);
            return Some(close.window);
        };
        if owner.start_join() {
            close.phase = ClosePhase::Joining;
        } else {
            self.notice = Some(
                "Action owner join worker unavailable; Retry owner shutdown or Keep Open".into(),
            );
            close.phase = ClosePhase::Vetoed;
        }
        self.closing = Some(close);
        None
    }
    pub(crate) fn commit_close(&mut self, window: iced::window::Id) -> bool {
        let Some(close) = self.closing.as_mut() else {
            return false;
        };
        if close.window != window
            || close.phase != ClosePhase::Ready
            || self.joined_workers.is_none()
            || self.owner.as_ref().is_some_and(|owner| !owner.clean_join())
        {
            return false;
        }
        close.phase = ClosePhase::Committed;
        true
    }
    pub(crate) fn shutdown_bounded(&mut self) {
        self.confirmation = None;
        self.pending = None;
        if let Some(current) = &self.current {
            current.work.cancel_pre_wire(&current.cancellation);
            current.cancellation.cancel(CancelCause::Shutdown);
        }
        self.owner.take();
    }
    pub(crate) fn running_identity(&self) -> Option<(u64, u64)> {
        let current = self.current.as_ref()?;
        current.flight.as_ref()?;
        Some((current.intent.generation, current.attempt))
    }

    pub(crate) fn status(&self) -> Option<(u64, u64, String, bool, bool)> {
        let current = self.current.as_ref()?;
        if current.state == State::Succeeded {
            return None;
        }
        Some((
            current.intent.generation,
            current.attempt,
            format!("{} — {}", current.label(), current.intent.description()),
            current.retry_ready && current.retryable(),
            !self.is_closing() && (current.flight.is_some() || current.state == State::Busy),
        ))
    }
    #[cfg(test)]
    pub(crate) fn state(&self) -> Option<&State> {
        self.current.as_ref().map(|a| &a.state)
    }
}

impl Drop for Actions {
    fn drop(&mut self) {
        self.shutdown_bounded();
    }
}

struct ObservationBudget {
    started: Instant,
}
impl ObservationBudget {
    fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
    fn remaining(&self) -> Duration {
        OBSERVATION_BUDGET.saturating_sub(self.started.elapsed())
    }
    fn expired(&self) -> bool {
        self.remaining().is_zero()
    }
    fn pause(&self) {
        std::thread::sleep(SAMPLE_PAUSE.min(self.remaining()));
    }
}
fn pre_wire_interruption(token: &CancelToken) -> State {
    if token.cause() == Some(CancelCause::Deadline) {
        State::TimedOut
    } else {
        State::Cancelled
    }
}

fn definite_signal_refusal(failure: &SignalFailure) -> bool {
    matches!(
        failure,
        SignalFailure::NotPermitted
            | SignalFailure::Gone
            | SignalFailure::IdentityChanged
            | SignalFailure::Unsupported
    )
}
