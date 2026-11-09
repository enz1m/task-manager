use crate::device_facts::{DeviceFacts, DeviceFactsOp};
use crate::services::{SampleScope, Snapshot};
use enzui_core::blocking_task::{
    BlockingShutdown, BlockingTask, BlockingTaskCancellation, BlockingTaskDispatcher,
    BlockingTaskError, BlockingTaskOwner,
};
use enzui_core::cancellation::{CancelCause, CancelToken};
use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub(crate) const WAKE_INTERVAL: Duration = Duration::from_millis(50);
pub(crate) const CLOSE_BUDGET: Duration = Duration::from_secs(2);
const COLLECTION_BUDGET: Duration = Duration::from_secs(15);
pub(crate) const LIVE_CAPTURE_MAX_AGE: Duration = Duration::from_secs(3);
const WORKERS_PER_OWNER: usize = 1;
const QUEUE_PER_OWNER: usize = 1;

mod health;
use health::bounded;
pub(crate) use health::{Health, Phase, SourceKind, Stamp, Terminal, domains, page_source};

pub(crate) struct Collected {
    pub(crate) snapshot: Snapshot,
    pub(crate) stamps: Vec<Stamp>,
}
impl Collected {
    #[cfg(test)]
    pub(crate) fn fixture(snapshot: Snapshot, scope: SampleScope, generation: u64) -> Self {
        let stamps = domains(scope)
            .into_iter()
            .map(|source| Stamp {
                source,
                generation,
                captured_at: Instant::now(),
                terminal: match source {
                    SourceKind::Processes if snapshot.processes.is_empty() => Terminal::Empty,
                    SourceKind::Services if snapshot.services.is_empty() => Terminal::Empty,
                    _ => Terminal::Current,
                },
            })
            .collect();
        Self { snapshot, stamps }
    }
}
type Collector = dyn Fn(SampleScope, u64, &CancelToken) -> Result<Collected, String> + Send + Sync;
#[derive(Clone, Copy, Debug)]
pub(crate) struct Intent {
    pub(crate) kind: SourceKind,
    pub(crate) generation: u64,
    pub(crate) scope: SampleScope,
}
struct FactsIntent {
    generation: u64,
    operation: Arc<Mutex<Option<DeviceFactsOp>>>,
}
struct Flight<T> {
    task: Option<BlockingTask<T>>,
    cancellation: BlockingTaskCancellation<T>,
    token: CancelToken,
    intent: Intent,
    started: Instant,
}
impl<T> Flight<T> {
    fn cancel(&self) {
        self.token.cancel(CancelCause::Cancelled);
        self.cancellation.cancel();
    }
    fn poll(&mut self) -> Option<Result<T, BlockingTaskError>> {
        let task = self.task.as_mut()?;
        match Pin::new(task).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Pending => None,
            Poll::Ready(settlement) => {
                self.task = None;
                Some(settlement.into_result())
            }
        }
    }
}
#[allow(
    clippy::large_enum_variant,
    reason = "one retained event per tick avoids a separate payload allocation"
)]
pub(crate) enum Event {
    Snapshot(Intent, Stamp, Result<Collected, String>),
    Facts(u64, Stamp, Result<DeviceFacts, String>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClosePhase {
    Draining,
    Vetoed,
    Committed,
}

#[derive(Clone, Copy)]
struct CloseDrain {
    window: iced::window::Id,
    started: Instant,
    phase: ClosePhase,
}

struct CollectionWorkers {
    snapshot: BlockingTaskOwner,
    facts: BlockingTaskOwner,
    prefetch: Option<crate::services::PrefetchOwner>,
}
impl CollectionWorkers {
    fn join(self) -> bool {
        let snapshot = self.snapshot.shutdown_and_join();
        let facts = self.facts.shutdown_and_join();
        let prefetch = self
            .prefetch
            .is_none_or(crate::services::PrefetchOwner::join);
        matches!(snapshot, BlockingShutdown::Joined { panicked: 0, .. })
            && matches!(facts, BlockingShutdown::Joined { panicked: 0, .. })
            && prefetch
    }
}

enum WorkerJoin {
    Idle,
    Running(JoinHandle<bool>),
    Joined,
    Failed,
}

// App ownership outlives subscription recipes, which carry wakes rather than
// accepted work or its occupancy.
pub(crate) struct Collections {
    snapshot_owner: Option<BlockingTaskOwner>,
    facts_owner: Option<BlockingTaskOwner>,
    snapshot_dispatcher: BlockingTaskDispatcher,
    facts_dispatcher: BlockingTaskDispatcher,
    worker_join: WorkerJoin,

    prefetch: Option<crate::services::PrefetchOwner>,
    collect: Arc<Collector>,
    pub(crate) painted: bool,
    first_frame_seen: bool,
    sequence: u64,
    exhausted: bool,
    latest_snapshot: Option<u64>,
    latest_facts: Option<u64>,
    snapshot_flight: Option<Flight<Result<Collected, String>>>,
    facts_flight: Option<Flight<Result<(DeviceFacts, Instant), String>>>,
    pending_snapshot: Option<Intent>,
    pending_facts: Option<FactsIntent>,
    closing: Option<CloseDrain>,
    flight_wakes: Vec<FlightWake>,
    /// The generation the one-second tick requested. Its flight settles
    /// without a wake of its own: the next tick polls it, so a steady tick
    /// is one message instead of two.
    paced_generation: Option<u64>,
}
/// Resolves when a flight settles or its budget runs out, whichever is first.
pub(crate) type FlightWake = Pin<Box<dyn Future<Output = ()> + Send>>;
pub(crate) fn flight_wake<T: Send + 'static>(
    task: &BlockingTask<T>,
    budget: Option<Duration>,
) -> FlightWake {
    let settled = task.settled();
    Box::pin(async move {
        match budget {
            Some(budget) => {
                let budget = std::pin::pin!(tokio::time::sleep(budget));
                let _ = iced::futures::future::select(settled, budget).await;
            }
            None => settled.await,
        }
    })
}
impl std::fmt::Debug for Collections {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("Collections")
            .field("live", &self.live())
            .field("sequence", &self.sequence)
            .finish()
    }
}
fn owner(name: &str) -> BlockingTaskOwner {
    let workers = NonZeroUsize::new(WORKERS_PER_OWNER).expect("positive worker cap");
    let queue = NonZeroUsize::new(QUEUE_PER_OWNER).expect("positive queue cap");
    BlockingTaskOwner::new(name, workers, queue)
        .unwrap_or_else(|_| BlockingTaskOwner::unavailable(queue))
}
impl Collections {
    pub(crate) fn system() -> Self {
        let prefetch = crate::services::PrefetchOwner::start();
        let pool = prefetch.pool();
        let mut collections = Self::with_collector(Arc::new(move |scope, generation, token| {
            pool.collect_owned(scope, token).map(|mut collected| {
                for stamp in &mut collected.stamps {
                    stamp.generation = generation;
                }
                collected
            })
        }));
        collections.prefetch = Some(prefetch);
        collections
    }
    pub(crate) fn with_collector(collect: Arc<Collector>) -> Self {
        let snapshot_owner = owner("tm-collection");
        let facts_owner = owner("tm-device-facts");
        Self {
            snapshot_dispatcher: snapshot_owner.dispatcher(),
            facts_dispatcher: facts_owner.dispatcher(),
            snapshot_owner: Some(snapshot_owner),
            facts_owner: Some(facts_owner),
            worker_join: WorkerJoin::Idle,

            prefetch: None,
            collect,
            painted: false,
            first_frame_seen: false,
            sequence: 0,
            exhausted: false,
            latest_snapshot: None,
            latest_facts: None,
            snapshot_flight: None,
            facts_flight: None,
            pending_snapshot: None,
            pending_facts: None,
            closing: None,
            flight_wakes: Vec::new(),
            paced_generation: None,
        }
    }
    fn next(&mut self) -> Option<u64> {
        let Some(next) = self.sequence.checked_add(1) else {
            self.exhausted = true;
            return None;
        };
        self.sequence = next;
        Some(next)
    }
    pub(crate) fn refusal(&self) -> Option<&'static str> {
        self.exhausted
            .then_some("Collection generation exhausted; restart Task Manager to collect again")
    }
    pub(crate) fn request(&mut self, kind: SourceKind, scope: SampleScope) -> Option<Intent> {
        if self.closing.is_some() {
            return None;
        }
        if kind == SourceKind::Cadence
            && (self.pending_snapshot.is_some()
                || self.snapshot_flight.as_ref().is_some_and(|flight| {
                    flight.intent.scope == scope
                        && !flight.token.is_terminal()
                        && Some(flight.intent.generation) == self.latest_snapshot
                }))
        {
            return None;
        }
        let generation = self.next()?;
        let intent = Intent {
            kind,
            generation,
            scope,
        };
        self.latest_snapshot = Some(generation);
        self.pending_snapshot = Some(intent);
        Some(intent)
    }
    /// [`Self::request`] for the one-second tick: the result is picked up by
    /// the next tick's poll rather than a wake of its own.
    pub(crate) fn request_paced(&mut self, scope: SampleScope) -> Option<Intent> {
        let intent = self.request(SourceKind::Cadence, scope)?;
        self.paced_generation = Some(intent.generation);
        Some(intent)
    }
    pub(crate) fn request_facts(&mut self, operation: DeviceFactsOp) -> Option<u64> {
        if self.closing.is_some() {
            return None;
        }
        let generation = self.next()?;
        self.latest_facts = Some(generation);
        self.pending_facts = Some(FactsIntent {
            generation,
            operation: Arc::new(Mutex::new(Some(operation))),
        });
        Some(generation)
    }
    pub(crate) fn invalidate(&mut self) {
        // Revoking publication does not release an occupied worker, even when
        // cancellation has already settled its task.
        self.latest_snapshot = None;
        self.latest_facts = None;
        self.pending_snapshot = None;
        self.pending_facts = None;
        if let Some(flight) = &self.snapshot_flight {
            flight.cancel();
        }
        if let Some(flight) = &self.facts_flight {
            flight.cancel();
        }
    }
    pub(crate) fn frame_seen(&mut self) {
        // A redraw notification may precede paint; admit work only on the
        // following frame so the pending view has had a frame of its own.
        if self.first_frame_seen {
            self.painted = true;
        } else {
            self.first_frame_seen = true;
        }
    }

    pub(crate) fn accepts_snapshot(&self, intent: Intent) -> bool {
        self.closing.is_none() && Some(intent.generation) == self.latest_snapshot
    }
    pub(crate) fn accepts_facts(&self, generation: u64) -> bool {
        self.closing.is_none() && Some(generation) == self.latest_facts
    }
    pub(crate) fn detail_dispatcher(&self) -> BlockingTaskDispatcher {
        self.facts_dispatcher.clone()
    }
    pub(crate) fn live(&self) -> usize {
        self.snapshot_dispatcher.snapshot().live()
            + self.facts_dispatcher.snapshot().live()
            + self
                .prefetch
                .as_ref()
                .map_or(0, crate::services::PrefetchOwner::live)
    }
    /// The wakes of flights started since the last call, to be awaited by
    /// the runtime instead of polling for their settlement.
    pub(crate) fn take_flight_wakes(&mut self) -> Vec<FlightWake> {
        std::mem::take(&mut self.flight_wakes)
    }
    pub(crate) fn is_idle(&self) -> bool {
        self.pending_snapshot.is_none()
            && self.pending_facts.is_none()
            && self.snapshot_flight.is_none()
            && self.facts_flight.is_none()
    }

    /// Whether the owner needs polling on a timer: queued work waiting for a
    /// free slot, or a close draining its workers. Flights wake through
    /// [`Self::take_flight_wakes`].
    pub(crate) fn needs_wake(&self) -> bool {
        if self.is_close_committed() {
            return false;
        }
        self.pending_snapshot.is_some()
            || self.pending_facts.is_some()
            || self.closing.is_some_and(|drain| {
                drain.phase == ClosePhase::Draining
                    || self.live() > 0
                    || matches!(self.worker_join, WorkerJoin::Running(_))
            })
    }
    pub(crate) fn is_closing(&self) -> bool {
        self.closing.is_some()
    }
    pub(crate) fn is_close_committed(&self) -> bool {
        self.closing
            .is_some_and(|drain| drain.phase == ClosePhase::Committed)
    }
    pub(crate) fn closing_window(&self) -> Option<iced::window::Id> {
        self.closing.map(|drain| drain.window)
    }

    #[cfg(test)]
    pub(crate) fn latest_generation(&self) -> u64 {
        self.latest_snapshot.unwrap_or(0)
    }
    pub(crate) fn close(&mut self, window: iced::window::Id, now: Instant) {
        if self
            .closing
            .is_some_and(|drain| drain.window != window || drain.phase != ClosePhase::Vetoed)
        {
            return;
        }
        self.invalidate();
        self.closing = Some(CloseDrain {
            window,
            started: now,
            phase: ClosePhase::Draining,
        });
        if let Some(owner) = self.snapshot_owner.as_mut() {
            owner.shutdown();
        }
        if let Some(owner) = self.facts_owner.as_mut() {
            owner.shutdown();
        }
        if let Some(prefetch) = &self.prefetch {
            prefetch.stop();
        }
    }
    pub(crate) fn owned_close_state(
        &mut self,
        now: Instant,
        additional_live: usize,
    ) -> (Option<iced::window::Id>, Option<String>) {
        let Some(mut drain) = self.closing else {
            return (None, None);
        };
        if drain.phase == ClosePhase::Committed {
            return (None, None);
        }
        let live = self.live() + additional_live;
        if live == 0 && self.join_workers() {
            drain.phase = ClosePhase::Committed;
            self.closing = Some(drain);
            return (Some(drain.window), None);
        }
        if now.saturating_duration_since(drain.started) >= CLOSE_BUDGET
            || matches!(self.worker_join, WorkerJoin::Failed)
        {
            drain.phase = ClosePhase::Vetoed;
        }
        self.closing = Some(drain);
        let notice = if matches!(self.worker_join, WorkerJoin::Failed) {
            "Close vetoed: collection worker join failed; clean shutdown is unverified".to_owned()
        } else {
            crate::i18n::message(
                "tm-retaining-first-window-until-live-owned-i-o-jobs-and-2bb9e0",
                &[
                    (
                        "arg0",
                        crate::i18n::argument(
                            &(if drain.phase == ClosePhase::Vetoed {
                                "Close vetoed"
                            } else {
                                "Closing"
                            }),
                        ),
                    ),
                    ("arg1", crate::i18n::argument(&(live))),
                ],
            )
        };
        (None, Some(notice))
    }

    fn join_workers(&mut self) -> bool {
        if matches!(self.worker_join, WorkerJoin::Idle) {
            let (snapshot, facts) = match (self.snapshot_owner.take(), self.facts_owner.take()) {
                (Some(snapshot), Some(facts)) => (snapshot, facts),
                (snapshot, facts) => {
                    self.snapshot_owner = snapshot;
                    self.facts_owner = facts;
                    self.worker_join = WorkerJoin::Failed;
                    return false;
                }
            };
            let retained = Arc::new(Mutex::new(Some(CollectionWorkers {
                snapshot,
                facts,
                prefetch: self.prefetch.take(),
            })));
            let workers = Arc::clone(&retained);
            match std::thread::Builder::new()
                .name("tm-collection-join".to_owned())
                .spawn(move || {
                    let workers = workers
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    workers.is_some_and(CollectionWorkers::join)
                }) {
                Ok(handle) => self.worker_join = WorkerJoin::Running(handle),
                Err(_) => {
                    let value = retained
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    if let Some(workers) = value {
                        self.snapshot_owner = Some(workers.snapshot);
                        self.facts_owner = Some(workers.facts);
                        self.prefetch = workers.prefetch;
                    }
                    self.worker_join = WorkerJoin::Failed;
                }
            }
        }
        if matches!(&self.worker_join, WorkerJoin::Running(handle) if handle.is_finished())
            && let WorkerJoin::Running(handle) =
                std::mem::replace(&mut self.worker_join, WorkerJoin::Failed)
        {
            self.worker_join = if handle.join().unwrap_or(false) {
                WorkerJoin::Joined
            } else {
                WorkerJoin::Failed
            };
        }
        matches!(self.worker_join, WorkerJoin::Joined)
    }

    pub(crate) fn poll(&mut self, now: Instant) -> Vec<Event> {
        if !self.painted || self.is_close_committed() {
            return Vec::new();
        }
        let mut events = Vec::new();
        let mut snapshot_busy = false;
        let mut facts_busy = false;
        if let Some(flight) = &mut self.snapshot_flight {
            if flight.task.is_some()
                && now.saturating_duration_since(flight.started) > COLLECTION_BUDGET
            {
                flight.cancel();
            }
            if let Some(result) = flight.poll() {
                if matches!(result, Err(BlockingTaskError::Busy)) {
                    snapshot_busy = true;
                    if Some(flight.intent.generation) == self.latest_snapshot {
                        self.pending_snapshot = Some(flight.intent);
                    }
                } else {
                    let result = result
                        .map_err(|error| error.to_string())
                        .and_then(|value| value)
                        .and_then(|value| {
                            if flight.token.is_terminal() {
                                Err("Collection cancelled or overdue".to_owned())
                            } else {
                                Ok(value)
                            }
                        })
                        .map_err(|error| bounded(&error));
                    let captured_at = result
                        .as_ref()
                        .ok()
                        .and_then(|collected| {
                            collected.stamps.iter().map(|stamp| stamp.captured_at).max()
                        })
                        .unwrap_or(now);
                    let stamp = Stamp {
                        source: flight.intent.kind,
                        generation: flight.intent.generation,
                        captured_at,
                        terminal: if result.is_ok() {
                            Terminal::Current
                        } else if flight.token.is_terminal() {
                            Terminal::Cancelled
                        } else {
                            Terminal::Failed
                        },
                    };
                    events.push(Event::Snapshot(flight.intent, stamp, result));
                }
            }
            if flight.task.is_none() && self.snapshot_dispatcher.snapshot().live() == 0 {
                self.snapshot_flight = None;
            }
        }
        if let Some(flight) = &mut self.facts_flight {
            if flight.task.is_some()
                && now.saturating_duration_since(flight.started) > COLLECTION_BUDGET
            {
                flight.cancel();
            }
            if let Some(result) = flight.poll() {
                if matches!(result, Err(BlockingTaskError::Busy)) {
                    facts_busy = true;
                } else {
                    if self
                        .pending_facts
                        .as_ref()
                        .is_some_and(|pending| pending.generation == flight.intent.generation)
                    {
                        self.pending_facts = None;
                    }
                    let result = result
                        .map_err(|error| error.to_string())
                        .and_then(|value| value)
                        .and_then(|value| {
                            if flight.token.is_terminal() {
                                Err("Collection cancelled or overdue".to_owned())
                            } else {
                                Ok(value)
                            }
                        })
                        .map_err(|error| bounded(&error));
                    let captured_at = result.as_ref().map_or(now, |(_, at)| *at);
                    let result = result.map(|(facts, _)| facts);
                    let terminal = match &result {
                        Ok(facts) if !facts.has_any() => Terminal::Empty,
                        Ok(_) => Terminal::Current,
                        Err(_) if flight.token.is_terminal() => Terminal::Cancelled,
                        Err(_) => Terminal::Failed,
                    };
                    events.push(Event::Facts(
                        flight.intent.generation,
                        Stamp {
                            source: SourceKind::DeviceFacts,
                            generation: flight.intent.generation,
                            captured_at,
                            terminal,
                        },
                        result,
                    ));
                }
            }
            if flight.task.is_none() && self.facts_dispatcher.snapshot().live() == 0 {
                self.facts_flight = None;
            }
        }
        if self.closing.is_some() {
            return events;
        }
        if self.snapshot_flight.is_none()
            && !snapshot_busy
            && let Some(intent) = self.pending_snapshot.take()
        {
            let collect = Arc::clone(&self.collect);
            let token = CancelToken::new();
            let worker_token = token.share();
            let task = self.snapshot_dispatcher.submit(
                intent.generation,
                crate::i18n::in_worker(
                    move |context: enzui_core::blocking_task::BlockingTaskContext| {
                        if context.is_cancelled() || worker_token.is_terminal() {
                            return Err("Collection cancelled".to_owned());
                        }
                        collect(intent.scope, intent.generation, &worker_token)
                    },
                ),
            );
            if self.paced_generation != Some(intent.generation) {
                self.flight_wakes
                    .push(flight_wake(&task, Some(COLLECTION_BUDGET)));
            }
            self.snapshot_flight = Some(Flight {
                cancellation: task.cancellation(),
                task: Some(task),
                token,
                intent,
                started: now,
            });
        }
        if self.facts_flight.is_none()
            && !facts_busy
            && let Some(pending) = &self.pending_facts
        {
            let intent = Intent {
                kind: SourceKind::DeviceFacts,
                generation: pending.generation,
                scope: SampleScope::ALL,
            };
            let operation = Arc::clone(&pending.operation);
            let token = CancelToken::new();
            let worker_token = token.share();
            let task = self.facts_dispatcher.submit(
                intent.generation,
                crate::i18n::in_worker(
                    move |context: enzui_core::blocking_task::BlockingTaskContext| {
                        if context.is_cancelled() || worker_token.is_terminal() {
                            return Err("Device facts cancelled".to_owned());
                        }
                        let operation = operation
                            .lock()
                            .map_err(|_| "Device facts lock failed")?
                            .take()
                            .ok_or("Device facts already consumed")?;
                        let facts = operation.execute();
                        if worker_token.is_terminal() {
                            Err("Device facts cancelled".to_owned())
                        } else {
                            Ok((facts, Instant::now()))
                        }
                    },
                ),
            );
            self.flight_wakes
                .push(flight_wake(&task, Some(COLLECTION_BUDGET)));
            self.facts_flight = Some(Flight {
                cancellation: task.cancellation(),
                task: Some(task),
                token,
                intent,
                started: now,
            });
        }
        events
    }
}
impl Drop for Collections {
    fn drop(&mut self) {
        self.invalidate();
        if let Some(prefetch) = &self.prefetch {
            prefetch.stop();
        }
        if let WorkerJoin::Running(handle) =
            std::mem::replace(&mut self.worker_join, WorkerJoin::Failed)
        {
            let _ = handle.join();
        }
        if let Some(owner) = self.snapshot_owner.take() {
            let _ = owner.shutdown_and_join();
        }
        if let Some(owner) = self.facts_owner.take() {
            let _ = owner.shutdown_and_join();
        }
        if let Some(prefetch) = self.prefetch.take() {
            let _ = prefetch.join();
        }
    }
}
