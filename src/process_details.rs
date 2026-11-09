use crate::services::ProcessTarget;
use enzui_core::blocking_task::{
    BlockingTask, BlockingTaskCancellation, BlockingTaskDispatcher, BlockingTaskError,
};
use enzui_core::cancellation::{CancelCause, CancelToken};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

mod procfs;
#[cfg(test)]
mod tests;

const FLIGHT_BUDGET: Duration = Duration::from_secs(2);
const BUSY_BACKOFF: Duration = Duration::from_millis(250);
const MAX_BUSY_RETRIES: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Limit {
    Entries,
    Name,
    Path,
    Input,
    Aggregate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    PermissionDenied,
    Refused,
    HiddenKernelThreads,
    Gone,
    EntryVanished,
    IdentityChanged,
    IdentityUnknown,
    Limit(Limit),
    Cancelled,
    TimedOut,
    Busy,
    Worker,
    GenerationExhausted,
    Io,
}

impl Failure {
    fn label(self) -> &'static str {
        match self {
            Self::PermissionDenied => "Refused — permission denied",
            Self::Refused => "Refused — unsafe proc entry",
            Self::HiddenKernelThreads => "Unknown — hidden kernel-thread data",
            Self::Gone => "Unknown — process vanished",
            Self::EntryVanished => "Unknown — entry vanished during census",
            Self::IdentityChanged => "Unknown — process identity changed",
            Self::IdentityUnknown => "Unknown — process identity unverified",
            Self::Limit(Limit::Entries) => "Refused — entry count limit exceeded",
            Self::Limit(Limit::Name) => "Refused — name byte limit exceeded",
            Self::Limit(Limit::Path) => "Refused — path byte limit exceeded",
            Self::Limit(Limit::Input) => "Refused — proc input byte limit exceeded",
            Self::Limit(Limit::Aggregate) => "Refused — aggregate byte limit exceeded",
            Self::Cancelled => "Unknown — census cancelled",
            Self::TimedOut => "Unknown — census timed out",
            Self::Busy => "Unknown — detail worker busy; refresh to retry",
            Self::Worker => "Unknown — detail worker failed",
            Self::GenerationExhausted => "Refused — detail generation exhausted",
            Self::Io => "Unknown — proc read failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    id: u32,
    value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reading {
    threads: Result<Vec<Entry>, Failure>,
    files: Result<Vec<Entry>, Failure>,
    cwd: Result<String, Failure>,
}

impl Reading {
    fn failed(failure: Failure) -> Self {
        Self {
            threads: Err(failure),
            files: Err(failure),
            cwd: Err(failure),
        }
    }

    fn labels(&self) -> [String; 3] {
        [
            census_label(&self.threads),
            census_label(&self.files),
            match &self.cwd {
                Ok(path) => path.clone(),
                Err(failure) => failure.label().into(),
            },
        ]
    }
}

fn census_label(result: &Result<Vec<Entry>, Failure>) -> String {
    match result {
        Ok(entries) => entries.len().to_string(),
        Err(failure) => failure.label().into(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Intent {
    target: ProcessTarget,
    generation: u64,
}

struct Flight {
    intent: Intent,
    token: CancelToken,
    cancellation: BlockingTaskCancellation<Reading>,
    task: Option<BlockingTask<Reading>>,
    started: Instant,
}

impl Flight {
    fn cancel(&self) {
        self.token.cancel(CancelCause::Cancelled);
        self.cancellation.cancel();
    }
}

type Collector = dyn Fn(&ProcessTarget, &CancelToken) -> Reading + Send + Sync;

pub(crate) struct Details {
    sequence: u64,
    exhausted: bool,
    current: Option<Intent>,
    reading: Option<Reading>,
    flight: Option<Flight>,
    pending: Option<Intent>,
    retry_at: Option<Instant>,
    busy_retries: u8,
    collect: Arc<Collector>,
    flight_wakes: Vec<crate::collection::FlightWake>,
}

impl std::fmt::Debug for Details {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("Details")
            .field("current", &self.current)
            .field("flight", &self.flight.is_some())
            .field("pending", &self.pending)
            .finish()
    }
}

impl Default for Details {
    fn default() -> Self {
        Self {
            sequence: 0,
            exhausted: false,
            current: None,
            reading: None,
            flight: None,
            pending: None,
            retry_at: None,
            busy_retries: 0,
            collect: Arc::new(procfs::collect),
            flight_wakes: Vec::new(),
        }
    }
}

impl Details {
    pub(crate) fn select(&mut self, target: Option<ProcessTarget>) {
        let target = target.filter(procfs::valid_target);
        if self.current.as_ref().map(|intent| &intent.target) == target.as_ref() {
            return;
        }
        self.invalidate();
        if let Some(target) = target {
            self.request(target);
        }
    }

    pub(crate) fn refresh(&mut self) {
        if let Some(intent) = self.current.clone() {
            self.invalidate();
            self.request(intent.target);
        }
    }

    fn request(&mut self, target: ProcessTarget) {
        let Some(generation) = self.sequence.checked_add(1).filter(|_| !self.exhausted) else {
            self.exhausted = true;
            return;
        };
        self.sequence = generation;
        let intent = Intent { target, generation };
        self.current = Some(intent.clone());
        self.pending = Some(intent);
    }

    pub(crate) fn invalidate(&mut self) {
        self.current = None;
        self.pending = None;
        self.reading = None;
        self.retry_at = None;
        self.busy_retries = 0;
        if let Some(flight) = &self.flight {
            flight.cancel();
        }
    }

    fn publish(&mut self, intent: &Intent, reading: Reading) {
        if !self.exhausted && self.current.as_ref() == Some(intent) {
            self.reading = Some(reading);
        }
    }

    pub(crate) fn labels(&self, target: &ProcessTarget) -> [String; 3] {
        if !procfs::valid_target(target) {
            return std::array::from_fn(|_| Failure::IdentityUnknown.label().into());
        }
        if self.exhausted {
            return std::array::from_fn(|_| Failure::GenerationExhausted.label().into());
        }
        if self
            .current
            .as_ref()
            .is_some_and(|intent| &intent.target == target)
            && let Some(reading) = &self.reading
        {
            return reading.labels();
        }
        std::array::from_fn(|_| "Unknown — details pending".into())
    }

    pub(crate) fn needs_wake(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn take_flight_wakes(&mut self) -> Vec<crate::collection::FlightWake> {
        std::mem::take(&mut self.flight_wakes)
    }

    pub(crate) fn poll(&mut self, dispatcher: &BlockingTaskDispatcher, now: Instant, admit: bool) {
        let mut terminal = None;
        if let Some(flight) = &mut self.flight {
            let timed_out = now.saturating_duration_since(flight.started) >= FLIGHT_BUDGET;
            if timed_out {
                flight.cancel();
            }
            if let Some(task) = &mut flight.task
                && let Poll::Ready(answer) =
                    Pin::new(task).poll(&mut Context::from_waker(Waker::noop()))
            {
                let result = if answer.generation() != flight.intent.generation {
                    Ok(Reading::failed(Failure::Worker))
                } else if timed_out {
                    Ok(Reading::failed(Failure::TimedOut))
                } else if flight.token.is_terminal() {
                    Ok(Reading::failed(Failure::Cancelled))
                } else {
                    answer.into_result()
                };
                terminal = Some((flight.intent.clone(), result));
                flight.task = None;
            }
            // Cancellation can settle the future before the worker releases its slot.
            if flight.task.is_none() && dispatcher.snapshot().live() == 0 {
                self.flight = None;
            }
        }
        if let Some((intent, result)) = terminal {
            match result {
                Err(BlockingTaskError::Busy) if self.current.as_ref() == Some(&intent) => {
                    if self.busy_retries < MAX_BUSY_RETRIES {
                        self.busy_retries += 1;
                        self.pending = Some(intent);
                        self.retry_at = Some(now + BUSY_BACKOFF);
                    } else {
                        self.publish(&intent, Reading::failed(Failure::Busy));
                    }
                }
                Ok(reading) => self.publish(&intent, reading),
                Err(_) => self.publish(&intent, Reading::failed(Failure::Worker)),
            }
        }
        if admit
            && self.flight.is_none()
            && dispatcher.snapshot().live() == 0
            && self.retry_at.is_none_or(|at| now >= at)
            && let Some(intent) = self.pending.take()
        {
            let token = CancelToken::new();
            let worker_token = token.share();
            let collect = Arc::clone(&self.collect);
            let target = intent.target.clone();
            let task = dispatcher.submit(
                intent.generation,
                crate::i18n::in_worker(
                    move |context: enzui_core::blocking_task::BlockingTaskContext| {
                        if context.is_cancelled() || worker_token.is_terminal() {
                            Reading::failed(Failure::Cancelled)
                        } else {
                            collect(&target, &worker_token)
                        }
                    },
                ),
            );
            self.flight_wakes
                .push(crate::collection::flight_wake(&task, Some(FLIGHT_BUDGET)));
            self.flight = Some(Flight {
                intent,
                token,
                cancellation: task.cancellation(),
                task: Some(task),
                started: now,
            });
        }
    }
}

impl Drop for Details {
    fn drop(&mut self) {
        self.invalidate();
    }
}
