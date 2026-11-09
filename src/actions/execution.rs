//! Execution: process/service runners and the actions view.

use super::intents::{Absence, Acceptance, ClosePhase, Intent, State, Target, Work};
use super::{
    Actions, SAMPLE_ATTEMPTS, SAMPLE_BUDGET, definite_signal_refusal, pre_wire_interruption,
};
use super::{FRESHNESS, ObservationBudget};
use crate::service_actions::{self, Failure, Sample, Verb};
use crate::services::{ProcessSignalBackend, SignalFailure};
use enzui_core::cancellation::{CancelCause, CancelToken};
use std::time::Instant;

pub(super) fn execute_process(
    intent: &Intent,
    started: Instant,
    cancellation: &CancelToken,
    work: &Work,
    backend: &dyn ProcessSignalBackend,
    reconciliation_only: bool,
) -> State {
    let Target::Process {
        pid,
        start_time_ticks,
        target_uid,
        signal,
    } = &intent.target
    else {
        return State::Uncertain;
    };
    let mut handle = work
        .pin
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !work.wired() && !reconciliation_only {
        if *pid <= 1 || *pid == std::process::id() {
            return State::Failed(SignalFailure::NotPermitted);
        }
        if cancellation.is_terminal() {
            return pre_wire_interruption(cancellation);
        }
        if started.elapsed() >= intent.budget() {
            return State::TimedOut;
        }
        let Some(target_uid) = target_uid else {
            return State::Failed(SignalFailure::IdentityUnverifiable);
        };
        let pinned = match backend.pin(*pid) {
            Ok(pin) => pin,
            Err(failure) => return State::Failed(failure),
        };
        match pinned.start_time_ticks() {
            Ok(start) if start == *start_time_ticks => {}
            Ok(_) => return State::Failed(SignalFailure::IdentityChanged),
            Err(failure) => return State::Failed(failure),
        }
        match pinned.uid() {
            Ok(uid) if uid != *target_uid => return State::CredentialsChanged,
            Ok(uid)
                if uid == intent.effective_uid
                    && intent.effective_uid == rustix::process::geteuid().as_raw() => {}
            Ok(_) => return State::Failed(SignalFailure::NotPermitted),
            Err(failure) => return State::Failed(failure),
        }
        if started.elapsed() >= intent.budget() {
            return State::TimedOut;
        }
        *handle = Some(pinned);
        if cancellation.is_terminal() || !work.begin_wire() {
            return pre_wire_interruption(cancellation);
        }
        if let Some(pinned) = handle.as_ref() {
            let result = pinned.send_signal(*signal);
            work.record_signal(result);
        }
    }
    work.note_interruption(cancellation);
    let Some(pinned) = handle.as_ref() else {
        return State::Uncertain;
    };
    let signal_exit = work
        .process_exit
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let observation = ObservationBudget::new();
    let mut absent_samples = 0;
    for index in 0..SAMPLE_ATTEMPTS {
        if observation.expired() {
            return State::Uncertain;
        }
        let captured_at = Instant::now();
        let sample = pinned.has_exited();
        *work
            .observed_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            sample.as_ref().ok().map(|_| captured_at);
        if let Some(Err(failure)) = &signal_exit
            && definite_signal_refusal(failure)
            && sample.is_ok()
        {
            return State::Failed(failure.clone());
        }
        if observation.expired() {
            return State::Uncertain;
        }
        match sample {
            Ok(true) => {
                return match signal_exit {
                    Some(Ok(())) => work.observed_label(cancellation),
                    Some(Err(failure)) => State::ProcessReconciled(failure),
                    None => State::ObservedWithoutResult,
                };
            }
            Ok(false) => absent_samples += 1,
            Err(_) => absent_samples = 0,
        }
        if index + 1 < SAMPLE_ATTEMPTS {
            observation.pause();
        }
    }
    if matches!(signal_exit, Some(Ok(()))) && absent_samples >= 2 {
        State::ConfirmedAbsent(Absence::ProcessStillPresent)
    } else {
        State::Uncertain
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "worker execution needs captured identity, cancellation and observation context"
)]
pub(super) fn execute_service(
    intent: &Intent,
    before: &Sample,
    verb: Verb,
    started: Instant,
    cancellation: &CancelToken,
    work: &Work,
    backend: &dyn service_actions::Backend,
    reconciliation_only: bool,
) -> State {
    let remaining = || intent.budget().saturating_sub(started.elapsed());
    if !work.wired() && !reconciliation_only {
        if cancellation.is_terminal() {
            return pre_wire_interruption(cancellation);
        }
        if remaining().is_zero() {
            return State::TimedOut;
        }
        let current =
            match backend.sample(&before.unit, remaining().min(SAMPLE_BUDGET), cancellation) {
                Ok(sample) => sample,
                Err(Failure::CleanupPending) => {
                    work.record_command(Err(Failure::CleanupPending));
                    return State::ServiceUncertain(Failure::CleanupPending);
                }
                Err(Failure::Cancelled) => return pre_wire_interruption(cancellation),
                Err(failure) => return State::ServiceFailed(failure),
            };
        if &current != before {
            return State::ServiceFailed(Failure::Changed);
        }
        if intent.effective_uid != rustix::process::geteuid().as_raw() {
            return State::ServiceFailed(Failure::Denied);
        }
        let command_budget = remaining().saturating_sub(SAMPLE_BUDGET);
        if command_budget.is_zero() {
            return State::ServiceFailed(Failure::Timeout);
        }
        if cancellation.is_terminal() || !work.begin_wire() {
            return pre_wire_interruption(cancellation);
        }
        let result = backend.command(&before.unit, verb, command_budget, cancellation);
        match result {
            Err(Failure::Cancelled) => {
                work.interruption.cancel(CancelCause::Cancelled);
            }
            Err(Failure::Timeout) => {
                work.interruption.cancel(CancelCause::Deadline);
            }
            _ => {}
        }
        work.record_command(result);
    }
    work.note_interruption(cancellation);
    // Caller cancellation and dispatch deadlines do not cancel this observation phase.
    let observation_token = CancelToken::new();
    let observation = ObservationBudget::new();
    let mut last_failure = None;
    let mut absent: Option<Sample> = None;
    let mut stable_samples = 0;
    for index in 0..SAMPLE_ATTEMPTS {
        if observation.expired() {
            return State::ServiceUncertain(Failure::Timeout);
        }
        let captured_at = Instant::now();
        let observed = backend.sample(
            &before.unit,
            observation.remaining().min(SAMPLE_BUDGET),
            &observation_token,
        );
        *work
            .observed_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = observed
            .as_ref()
            .ok()
            .filter(|sample| before.same_identity(sample))
            .map(|_| captured_at);
        if observed == Err(Failure::CleanupPending) {
            work.record_command(Err(Failure::CleanupPending));
        }
        if observation.expired() {
            return State::ServiceUncertain(Failure::Timeout);
        }
        match observed {
            Ok(sample) if !before.same_identity(&sample) => {
                last_failure = Some(Failure::Changed);
                absent = None;
                stable_samples = 0;
            }
            Ok(sample) if sample.active == verb.target() => {
                if matches!(work.acceptance(), Acceptance::Unknown | Acceptance::InCall) {
                    return State::ServiceUncertain(
                        work.service_exit
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .and_then(Result::err)
                            .unwrap_or(Failure::Unverifiable),
                    );
                }
                let value = *work
                    .service_exit
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                return match value {
                    Some(Err(Failure::CleanupPending)) => {
                        State::ServiceUncertain(Failure::CleanupPending)
                    }
                    Some(Err(failure)) => State::ServiceReconciled(failure),
                    Some(Ok(())) => work.observed_label(cancellation),
                    None => State::ObservedWithoutResult,
                };
            }
            Ok(sample) => {
                if work.acceptance() == Acceptance::Refused
                    && let Some(Err(failure)) = *work
                        .service_exit
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                {
                    return State::ServiceFailed(failure);
                }
                if matches!(sample.active.as_str(), "active" | "inactive" | "failed") {
                    stable_samples = if absent.as_ref() == Some(&sample) {
                        stable_samples + 1
                    } else {
                        1
                    };
                    absent = Some(sample);
                } else {
                    absent = None;
                    stable_samples = 0;
                }
                last_failure = None;
            }
            Err(failure) => {
                last_failure = Some(failure);
                absent = None;
                stable_samples = 0;
            }
        }
        if index + 1 < SAMPLE_ATTEMPTS {
            observation.pause();
        }
    }
    let settlement = *work
        .service_exit
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if stable_samples >= 2
        && matches!(
            work.acceptance(),
            Acceptance::Accepted | Acceptance::SettledFailure
        )
        && let Some(sample) = absent
    {
        return State::ConfirmedAbsent(Absence::ServiceState {
            active: sample.active,
            sub: sample.sub,
        });
    }
    State::ServiceUncertain(
        settlement
            .and_then(Result::err)
            .or(last_failure)
            .unwrap_or(Failure::Unverifiable),
    )
}

/// The action owner's status as app-owned notices for the bottom-right stack.
///
/// Each entry pairs a dismissal key with its notice. The key names the state
/// it reports, so a dismissed notice stays hidden until that state changes.
/// An action that is not ready yet is left off until it is.
pub(crate) fn notices(
    actions: &Actions,
) -> Vec<(
    String,
    enzui_widgets::notice_stack::EnzNotice<crate::message::Message>,
)> {
    use crate::i18n::display;
    use crate::message::Message;
    use enzui_widgets::notice_stack::EnzNotice;
    use enzui_widgets::toast::ToastTone;
    let mut out = Vec::new();
    if let Some(notice) = &actions.notice {
        out.push((
            format!("action-notice:{notice}"),
            EnzNotice::new("tm-action-notice", notice.clone(), ToastTone::Warning),
        ));
    }
    if let Some(close) = actions.closing {
        let (title, tone) = match close.phase {
            ClosePhase::Vetoed => (
                "Close vetoed: action reconciliation or owner shutdown is unresolved",
                ToastTone::Warning,
            ),
            ClosePhase::Joining => ("Closing: joining action owner", ToastTone::Info),
            _ => (
                "Closing: retained first window; waiting for owned work",
                ToastTone::Info,
            ),
        };
        let mut buttons = Vec::new();
        if close.phase == ClosePhase::Vetoed {
            if actions
                .current
                .as_ref()
                .is_some_and(|current| current.debt.blocks())
            {
                if actions.current.as_ref().is_some_and(|current| {
                    current.retry_ready && current.retryable() && current.flight.is_none()
                }) {
                    buttons.push((
                        "Retry reconciliation",
                        Message::RetryActionsClose(close.generation),
                    ));
                }
            } else if !actions.blocks_close() {
                buttons.push((
                    "Retry owner shutdown",
                    Message::RetryActionsClose(close.generation),
                ));
            }
            buttons.push(("Keep Open", Message::KeepActionsOpen(close.generation)));
        }
        out.push((
            format!("action-close:{}:{title}", close.generation),
            with_actions(
                EnzNotice::new("tm-action-close", display(title), tone),
                buttons,
            ),
        ));
    }
    if let Some((generation, attempt, text, retry, cancel)) = actions.status() {
        let mut buttons = Vec::new();
        if retry {
            buttons.push(("Retry", Message::RetryOwnedAction(generation, attempt)));
        }
        if cancel {
            buttons.push((
                "Cancel action",
                Message::CancelOwnedAction(generation, attempt),
            ));
        }
        let mut notice = EnzNotice::new(
            "tm-action-status",
            text.clone(),
            if cancel {
                ToastTone::Info
            } else {
                ToastTone::Warning
            },
        );
        let mut key = format!("action-status:{generation}:{attempt}:{text}");
        if let Some(current) = &actions.current
            && matches!(current.state, State::ConfirmedAbsent(_))
            && current.flight.is_none()
        {
            if current.proof_at.is_some_and(|at| at.elapsed() <= FRESHNESS) {
                buttons.push((
                    "Acknowledge failure / Keep Open",
                    Message::AcknowledgeActionFailure(current.intent.generation, current.attempt),
                ));
            } else {
                notice = notice.description(display(
                    "Failure observation expired; Retry before acknowledging",
                ));
                key.push_str(":expired");
            }
        }
        out.push((key, with_actions(notice, buttons)));
    }
    if let Some(intent) = &actions.pending {
        let mut buttons = Vec::new();
        if actions.pending_retry_ready {
            buttons.push((
                "Retry pending action",
                Message::RetryOwnedAction(intent.generation, 0),
            ));
        }
        buttons.push((
            "Cancel pending action",
            Message::CancelOwnedAction(intent.generation, 0),
        ));
        out.push((
            format!("action-pending:{}", intent.generation),
            with_actions(
                EnzNotice::new(
                    "tm-action-pending",
                    crate::i18n::message(
                        "tm-busy-exact-pending-action-retained-f814e6",
                        &[("arg0", crate::i18n::argument(&(intent.description())))],
                    ),
                    ToastTone::Info,
                ),
                buttons,
            ),
        ));
    }
    out
}

/// Attach up to two actions: the first as the primary, the second as the secondary.
pub(crate) fn with_actions<Message>(
    mut notice: enzui_widgets::notice_stack::EnzNotice<Message>,
    buttons: Vec<(&str, Message)>,
) -> enzui_widgets::notice_stack::EnzNotice<Message> {
    let mut buttons = buttons.into_iter();
    if let Some((label, message)) = buttons.next() {
        notice = notice.action(crate::i18n::display(label), message);
    }
    if let Some((label, message)) = buttons.next() {
        notice = notice.secondary_action(crate::i18n::display(label), message);
    }
    debug_assert!(
        buttons.next().is_none(),
        "a notice carries at most two actions"
    );
    notice
}
