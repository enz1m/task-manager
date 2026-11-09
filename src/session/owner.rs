use super::Session;
use super::authority::{Authority, Decoded, Encoded, Failure};
use enzui_core::blocking_task::{
    BlockingShutdown, BlockingTask, BlockingTaskError, BlockingTaskOwner,
};
#[cfg(test)]
use enzui_core::persist::LoadOutcome;
use std::future::Future;
use std::num::NonZeroUsize;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MAX_ATTEMPTS: u8 = 3;
pub(crate) const WAKE_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Snapshot {
    generation: u64,
    record: Session,
    bytes: Encoded,
}

#[derive(Clone, Copy)]
enum Work {
    Load,
    Save(Snapshot),
}

enum Answer {
    Load(Result<Option<Decoded>, Failure>),
    Save(Result<(), Failure>),
}

struct Flight {
    work: Work,
    task: Pin<Box<BlockingTask<Answer>>>,
}

#[derive(Clone, Copy)]
struct Close {
    window: iced::window::Id,
    generation: u64,
}

enum Join {
    Idle,
    Running(JoinHandle<BlockingShutdown>),
    Joined,
    Failed,
}

pub(crate) struct Owner {
    authority: Arc<Mutex<Authority>>,
    io: Option<BlockingTaskOwner>,
    flight: Option<Flight>,
    sequence: u64,
    baseline: Option<Session>,
    migration_pending: bool,
    dirty: Option<Snapshot>,
    loading: bool,
    ignore_load: bool,
    failure: Option<(u64, Failure)>,
    fatal: Option<Failure>,
    attempts: u8,
    retry_at: Option<Instant>,
    close_sequence: u64,
    close: Option<Close>,
    join: Join,
    flight_wakes: Vec<crate::collection::FlightWake>,
    #[cfg(test)]
    before_save: Option<Box<dyn FnOnce() + Send>>,
}

impl Owner {
    pub(super) fn new(path: Option<&Path>) -> Self {
        Self {
            authority: Arc::new(Mutex::new(Authority::new(path.map(Path::to_path_buf)))),
            io: None,
            flight: None,
            sequence: 0,
            baseline: None,
            migration_pending: false,
            dirty: None,
            loading: false,
            ignore_load: false,
            failure: None,
            fatal: None,
            attempts: 0,
            retry_at: None,
            close_sequence: 0,
            close: None,
            join: Join::Idle,
            flight_wakes: Vec::new(),
            #[cfg(test)]
            before_save: None,
        }
    }

    pub(crate) fn begin_load(&mut self, ignore: bool) {
        self.ignore_load = ignore;
        self.loading = true;
        self.dispatch(Work::Load);
    }

    pub(crate) fn remember(&mut self, record: Session) {
        if self.is_closing() || self.fatal.is_some() {
            return;
        }
        self.ignore_load = true;
        if self.dirty.is_some_and(|snapshot| snapshot.record == record)
            || (self.dirty.is_none() && !self.migration_pending && self.baseline == Some(record))
        {
            return;
        }
        let Some(generation) = self.sequence.checked_add(1) else {
            self.fatal = Some(Failure::Exhausted);
            return;
        };
        let bytes = match Encoded::new(record) {
            Ok(bytes) => bytes,
            Err(failure) => {
                self.fatal = Some(failure);
                return;
            }
        };
        self.sequence = generation;
        self.dirty = Some(Snapshot {
            generation,
            record,
            bytes,
        });
        self.failure = None;
        self.attempts = 0;
        self.retry_at = None;
        if !self.loading
            && self.flight.is_none()
            && let Some(snapshot) = self.dirty
        {
            self.dispatch(Work::Save(snapshot));
        }
    }

    fn dispatch(&mut self, work: Work) {
        if self.flight.is_some() || !matches!(self.join, Join::Idle) || self.fatal.is_some() {
            return;
        }
        if self.io.is_none() {
            let one = NonZeroUsize::new(1).expect("positive session worker bound");
            match BlockingTaskOwner::new("tm-session-io", one, one) {
                Ok(owner) => self.io = Some(owner),
                Err(_) => {
                    self.failed(Failure::Busy, Instant::now());
                    return;
                }
            }
        }
        let generation = match work {
            Work::Load => 0,
            Work::Save(snapshot) => snapshot.generation,
        };
        let authority = Arc::clone(&self.authority);
        #[cfg(test)]
        let hold = if matches!(work, Work::Save(_)) {
            self.before_save.take()
        } else {
            None
        };
        let Some(io) = self.io.as_ref() else {
            return;
        };
        let task = io.submit(generation, move |_| {
            #[cfg(test)]
            if let Some(hold) = hold {
                hold();
            }
            let mut authority = authority
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match work {
                Work::Load => Answer::Load(authority.load()),
                Work::Save(snapshot) => {
                    Answer::Save(authority.save(snapshot.generation, snapshot.bytes))
                }
            }
        });
        self.flight_wakes
            .push(crate::collection::flight_wake(&task, None));
        self.flight = Some(Flight {
            work,
            task: Box::pin(task),
        });
    }

    fn failed(&mut self, failure: Failure, now: Instant) {
        if matches!(
            failure,
            Failure::Lost | Failure::Worker | Failure::Exhausted | Failure::Join
        ) {
            self.fatal = Some(failure);
            return;
        }
        self.failure = Some((self.sequence, failure));
        self.attempts = self.attempts.saturating_add(1).min(MAX_ATTEMPTS);
        self.retry_at = Some(now + Duration::from_millis(250 << self.attempts));
    }

    fn settle_save(&mut self, snapshot: Snapshot, result: Result<(), Failure>, now: Instant) {
        if let Err(failure @ (Failure::Lost | Failure::Exhausted)) = result {
            self.fatal = Some(failure);
            return;
        }
        if self.dirty != Some(snapshot) {
            return;
        }
        match result {
            Ok(()) => {
                self.baseline = Some(snapshot.record);
                self.migration_pending = false;
                self.dirty = None;
                self.failure = None;
                self.attempts = 0;
                self.retry_at = None;
            }
            Err(failure) => self.failed(failure, now),
        }
    }

    pub(crate) fn poll(&mut self, now: Instant) -> Option<crate::model::Page> {
        let answer = self.flight.as_mut().and_then(|flight| {
            let mut context = Context::from_waker(Waker::noop());
            match flight.task.as_mut().poll(&mut context) {
                Poll::Ready(answer) => Some(answer),
                Poll::Pending => None,
            }
        });
        let mut page = None;
        if let Some(answer) = answer {
            let flight = self.flight.take()?;
            let generation = match flight.work {
                Work::Load => 0,
                Work::Save(snapshot) => snapshot.generation,
            };
            if answer.generation() != generation {
                self.fatal = Some(Failure::Worker);
            } else {
                match (flight.work, answer.into_result()) {
                    (Work::Load, Ok(Answer::Load(Ok(decoded)))) => {
                        self.loading = false;
                        self.attempts = 0;
                        self.retry_at = None;
                        self.failure = None;
                        self.migration_pending =
                            decoded.is_some_and(|value| value.migration_pending);
                        if !self.ignore_load && !self.is_closing() {
                            let record = decoded.map(|value| value.record);
                            self.baseline = record;
                            page = record.and_then(|record| record.page);
                        }
                    }
                    (Work::Load, Ok(Answer::Load(Err(failure)))) => {
                        self.loading = failure != Failure::Refused;
                        self.failed(failure, now);
                    }
                    (Work::Save(snapshot), Ok(Answer::Save(result))) => {
                        self.settle_save(snapshot, result, now);
                    }
                    (Work::Save(snapshot), Err(BlockingTaskError::Busy)) => {
                        self.settle_save(snapshot, Err(Failure::Busy), now);
                    }
                    (Work::Load, Err(BlockingTaskError::Busy)) => self.failed(Failure::Busy, now),
                    _ => self.fatal = Some(Failure::Worker),
                }
            }
        }
        if self.flight.is_none() && self.retry_due(now) {
            if self.loading {
                self.dispatch(Work::Load);
            } else if let Some(snapshot) = self.dirty {
                self.dispatch(Work::Save(snapshot));
            }
        }
        if self.is_closing()
            && self.flight.is_none()
            && !self.loading
            && self.dirty.is_none()
            && self.fatal.is_none()
            && self.failure.is_none()
        {
            self.advance_join();
        }
        page
    }

    fn retry_due(&self, now: Instant) -> bool {
        self.fatal.is_none()
            && self.attempts < MAX_ATTEMPTS
            && !self
                .failure
                .is_some_and(|(_, failure)| failure == Failure::Refused)
            && self.retry_at.is_none_or(|at| now >= at)
    }

    pub(crate) fn take_flight_wakes(&mut self) -> Vec<crate::collection::FlightWake> {
        std::mem::take(&mut self.flight_wakes)
    }

    pub(crate) fn startup_settled(&self) -> bool {
        !self.loading && self.flight.is_none()
    }

    // Running loads and saves wake on settlement; only retries and joins need a timer.
    pub(crate) fn needs_wake(&self) -> bool {
        (self.fatal.is_none()
            && !self
                .failure
                .is_some_and(|(_, failure)| failure == Failure::Refused)
            && self.retry_at.is_some_and(|at| Instant::now() < at))
            || matches!(self.join, Join::Running(_))
            || ((self.loading || self.dirty.is_some())
                && self.flight.is_none()
                && self.fatal.is_none()
                && self.attempts < MAX_ATTEMPTS
                && !self
                    .failure
                    .is_some_and(|(_, failure)| failure == Failure::Refused))
            || (self.is_closing()
                && matches!(self.join, Join::Idle)
                && self.failure.is_none()
                && self.fatal.is_none())
    }

    pub(crate) fn retry(&mut self, generation: u64, now: Instant) {
        if generation != self.sequence
            || self.flight.is_some()
            || self.fatal.is_some()
            || self.retry_at.is_some_and(|at| now < at)
            || self
                .failure
                .is_some_and(|(_, failure)| failure == Failure::Refused)
        {
            return;
        }
        self.attempts = 0;
        self.retry_at = None;
    }

    pub(crate) fn start_close(&mut self, window: iced::window::Id) -> bool {
        if self.close.is_some() {
            return false;
        }
        let Some(generation) = self.close_sequence.checked_add(1) else {
            self.fatal = Some(Failure::Exhausted);
            return false;
        };
        self.close_sequence = generation;
        self.close = Some(Close { window, generation });
        self.ignore_load = true;
        true
    }

    pub(crate) fn is_closing(&self) -> bool {
        self.close.is_some()
    }
    pub(crate) fn closing_window(&self) -> Option<iced::window::Id> {
        self.close.map(|close| close.window)
    }
    #[cfg(not(test))]
    pub(crate) fn refuse_missing_root(&mut self) {
        self.fatal = Some(Failure::Refused);
    }
    pub(crate) fn ready_window(&self) -> Option<iced::window::Id> {
        self.close
            .filter(|_| {
                matches!(self.join, Join::Joined)
                    && self.fatal.is_none()
                    && self.dirty.is_none()
                    && self.flight.is_none()
            })
            .map(|close| close.window)
    }

    pub(crate) fn keep_open(&mut self, generation: u64) {
        if self
            .close
            .is_some_and(|close| close.generation == generation)
            && matches!(self.join, Join::Idle)
        {
            self.close = None;
        }
    }

    pub(crate) fn resume_after_action_veto(&mut self) {
        if matches!(self.join, Join::Joined) {
            self.close = None;
            self.join = Join::Idle;
        }
    }

    fn advance_join(&mut self) {
        if matches!(self.join, Join::Idle) {
            if self
                .io
                .as_ref()
                .is_some_and(|owner| owner.dispatcher().snapshot().live() != 0)
            {
                return;
            }
            let Some(owner) = self.io.take() else {
                self.join = Join::Joined;
                return;
            };
            let retained = Arc::new(Mutex::new(Some(owner)));
            let worker = Arc::clone(&retained);
            match std::thread::Builder::new()
                .name("tm-session-join".into())
                .spawn(move || {
                    let owner = worker
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                        .expect("retained session owner");
                    owner.shutdown_and_join()
                }) {
                Ok(handle) => self.join = Join::Running(handle),
                Err(_) => {
                    self.io = retained
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    self.fatal = Some(Failure::Join);
                }
            }
        }
        if matches!(&self.join, Join::Running(handle) if handle.is_finished()) {
            let Join::Running(handle) = std::mem::replace(&mut self.join, Join::Failed) else {
                return;
            };
            if matches!(
                handle.join(),
                Ok(BlockingShutdown::Joined {
                    workers: 1,
                    panicked: 0
                })
            ) {
                self.join = Join::Joined;
            } else {
                self.fatal = Some(Failure::Join);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn load(&self) -> LoadOutcome<Session> {
        self.authority.lock().unwrap().load_outcome()
    }
    #[cfg(test)]
    pub(crate) fn is_write_blocked(&self) -> bool {
        self.authority.lock().unwrap().refused()
    }
}

/// Page-save failures and close progress as app-owned notices for the
/// bottom-right stack, each paired with its dismissal key (see
/// [`crate::actions::notices`]). Retry appears once a retry is allowed.
pub(crate) fn notices(
    owner: &Owner,
) -> Vec<(
    String,
    enzui_widgets::notice_stack::EnzNotice<crate::message::Message>,
)> {
    use crate::message::Message;
    use enzui_widgets::notice_stack::EnzNotice;
    use enzui_widgets::toast::ToastTone;
    let mut out = Vec::new();
    if let Some(failure) = owner
        .fatal
        .or_else(|| owner.failure.map(|(_, failure)| failure))
    {
        let mut buttons = Vec::new();
        if owner.fatal.is_none()
            && failure != Failure::Refused
            && owner.flight.is_none()
            && owner.retry_at.is_none_or(|at| Instant::now() >= at)
        {
            buttons.push(("Retry page save", Message::SessionRetry(owner.sequence)));
        }
        out.push((
            format!("session-failure:{}:{failure:?}", owner.fatal.is_some()),
            crate::actions::with_actions(
                EnzNotice::new(
                    "tm-session-failure",
                    crate::i18n::display(failure.summary()),
                    ToastTone::Error,
                ),
                buttons,
            ),
        ));
    }
    if let Some(close) = owner.close {
        let title = if owner.ready_window().is_some() {
            "Session owner joined; waiting for remaining owners"
        } else {
            "Closing: retaining this window until the newest page is durable and its owner joins"
        };
        let mut buttons = Vec::new();
        if matches!(owner.join, Join::Idle) {
            buttons.push(("Keep Open", Message::SessionKeepOpen(close.generation)));
        }
        out.push((
            format!("session-close:{}:{title}", close.generation),
            crate::actions::with_actions(
                EnzNotice::new(
                    "tm-session-close",
                    crate::i18n::display(title),
                    ToastTone::Info,
                ),
                buttons,
            ),
        ));
    }
    out
}

#[cfg(test)]
mod tests;

#[cfg(test)]
impl Owner {
    pub(crate) fn is_busy(&self) -> bool {
        self.flight.is_some() || self.needs_wake()
    }
}
