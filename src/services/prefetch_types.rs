//! Prefetch job types.

use super::*;

pub(crate) const SYSTEMCTL_EXE: &str = "/usr/bin/systemctl";
pub(crate) const JOURNALCTL_EXE: &str = "/usr/bin/journalctl";

pub(crate) const HELPER_STDERR_MAX_BYTES: usize = 64 * 1024;

pub(crate) const SERVICES_STDOUT_MAX_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const FRAGMENT_STDOUT_MAX_BYTES: usize = 4 * 1024 * 1024;

pub(crate) const SNAPSHOT_HELPER_DEADLINE: Duration = Duration::from_secs(10);
pub(crate) const SERVICE_LOG_DEADLINE: Duration = Duration::from_secs(15);

// Skip follow-up helpers when the remaining shared allowance falls below this floor.
pub(crate) const ROUTE_MIN_CHILD_BUDGET: Duration = Duration::from_millis(200);

#[derive(Clone, Copy)]
pub(crate) struct RouteBudget {
    start: Instant,
    total: Duration,
}

impl RouteBudget {
    pub(crate) fn new(total: Duration) -> Self {
        Self {
            start: Instant::now(),
            total,
        }
    }

    pub(crate) fn remaining(&self) -> Duration {
        self.total.saturating_sub(self.start.elapsed())
    }

    pub(crate) fn next_child_deadline(&self) -> Option<Duration> {
        let remaining = self.remaining();
        (remaining >= ROUTE_MIN_CHILD_BUDGET).then_some(remaining)
    }
}

pub(crate) fn run_request(
    request: ProcessRequest,
    cancellation: &CancelToken,
) -> Result<ProcessOutput, ProcessError> {
    process_transport::run(request, cancellation)
}

pub(crate) type PrefetchCollector<T> =
    dyn Fn(&CancelToken) -> (Vec<T>, bool) + Send + Sync + 'static;

pub(crate) struct PrefetchJob<T> {
    pub(crate) generation: u64,
    pub(crate) source: crate::collection::SourceKind,
    pub(crate) cancellation: CancelToken,
    pub(crate) collect: Arc<PrefetchCollector<T>>,
}

pub(crate) struct ActivePrefetch {
    pub(crate) generation: u64,
    pub(crate) cancellation: CancelToken,
}

pub(crate) struct PrefetchState<T> {
    pub(crate) data: Option<Vec<T>>,
    pub(crate) fabricated: bool,
    pub(crate) generation: u64,
    pub(crate) published_generation: u64,
    pub(crate) last_stamp: Option<crate::collection::Stamp>,
    pub(crate) active: Option<ActivePrefetch>,
    pub(crate) pending: Option<PrefetchJob<T>>,
    pub(crate) terminal: Option<PrefetchFailure>,
    pub(crate) stopping: bool,
}

impl<T> Default for PrefetchState<T> {
    fn default() -> Self {
        Self {
            data: None,
            fabricated: false,
            generation: 0,
            published_generation: 0,
            last_stamp: None,
            active: None,
            pending: None,
            terminal: None,
            stopping: false,
        }
    }
}

pub(crate) struct PrefetchShared<T> {
    pub(crate) state: Mutex<PrefetchState<T>>,
    pub(crate) wake: Condvar,
}

impl<T> Default for PrefetchShared<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(PrefetchState::default()),
            wake: Condvar::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrefetchDispatch {
    Queued,
    Replaced,
    BusyCoalesced,
    Unavailable(PrefetchFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrefetchFailure {
    Spawn(std::io::ErrorKind),
    Panicked { generation: u64 },
    Poisoned,
    Stopped,
    GenerationExhausted,
}

impl std::fmt::Display for PrefetchFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(kind) => write!(f, "worker could not start: {kind}"),
            Self::Panicked { generation } => {
                write!(f, "worker panicked in generation {generation}")
            }
            Self::Poisoned => f.write_str("worker state is unavailable"),
            Self::Stopped => f.write_str("worker has stopped"),
            Self::GenerationExhausted => f.write_str("collection generation exhausted"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrefetchStatus {
    Idle,
    Busy { replacement_queued: bool },
    Failed(PrefetchFailure),
    Stopped,
}

pub(crate) struct PrefetchWorker<T> {
    pub(crate) shared: Arc<PrefetchShared<T>>,
    pub(crate) owner: Mutex<Option<BlockingTaskOwner>>,
    pub(crate) dispatcher: BlockingTaskDispatcher,
    pub(crate) task: Mutex<Option<BlockingTask<()>>>,
    pub(crate) task_cancellation: Mutex<Option<BlockingTaskCancellation<()>>>,
    pub(crate) spawn_error: Option<std::io::ErrorKind>,
}

impl<T: Send + 'static> PrefetchWorker<T> {
    pub(crate) fn spawn(thread_name: &str) -> Self {
        let worker_count = NonZeroUsize::MIN;
        let queue_capacity = NonZeroUsize::MIN;
        let (owner, spawn_error) =
            match BlockingTaskOwner::new(thread_name, worker_count, queue_capacity) {
                Ok(owner) => (owner, None),
                Err(error) => (
                    BlockingTaskOwner::unavailable(queue_capacity),
                    Some(error.kind()),
                ),
            };
        let dispatcher = owner.dispatcher();
        let shared = Arc::new(PrefetchShared::default());
        if let Some(kind) = spawn_error
            && let Ok(mut state) = shared.state.lock()
        {
            state.terminal = Some(PrefetchFailure::Spawn(kind));
            state.stopping = true;
        }
        Self {
            shared,
            owner: Mutex::new(Some(owner)),
            dispatcher,
            task: Mutex::new(None),
            task_cancellation: Mutex::new(None),
            spawn_error,
        }
    }

    pub(crate) fn request_with_token(
        &self,
        collect: Arc<PrefetchCollector<T>>,
        policy: PrefetchPolicy,
        token: Option<&CancelToken>,
        source: crate::collection::SourceKind,
    ) -> PrefetchDispatch {
        if let Some(kind) = self.spawn_error {
            return PrefetchDispatch::Unavailable(PrefetchFailure::Spawn(kind));
        }
        let (dispatch, generation, start_runner) = {
            let Ok(mut state) = self.shared.state.lock() else {
                return PrefetchDispatch::Unavailable(PrefetchFailure::Poisoned);
            };
            if let Some(failure) = state.terminal {
                return PrefetchDispatch::Unavailable(failure);
            }
            if state.stopping {
                return PrefetchDispatch::Unavailable(PrefetchFailure::Stopped);
            }
            let busy = state.active.is_some() || state.pending.is_some();
            if busy && policy == PrefetchPolicy::Coalesce {
                return PrefetchDispatch::BusyCoalesced;
            }
            if policy == PrefetchPolicy::Supersede {
                if let Some(active) = &state.active {
                    active.cancellation.cancel(CancelCause::Cancelled);
                }
                if let Some(replaced) = state.pending.take() {
                    replaced.cancellation.cancel(CancelCause::Cancelled);
                }
            }
            let Some(generation) = state.generation.checked_add(1) else {
                state.terminal = Some(PrefetchFailure::GenerationExhausted);
                return PrefetchDispatch::Unavailable(PrefetchFailure::GenerationExhausted);
            };
            state.generation = generation;
            state.pending = Some(PrefetchJob {
                generation,
                source,
                cancellation: token.map(CancelToken::share).unwrap_or_default(),
                collect,
            });

            (
                if busy {
                    PrefetchDispatch::Replaced
                } else {
                    PrefetchDispatch::Queued
                },
                generation,
                !busy,
            )
        };

        if start_runner {
            let shared = Arc::clone(&self.shared);
            let task = self.dispatcher.submit(
                generation,
                crate::i18n::in_worker(move |_| run_prefetch_jobs(shared)),
            );
            let cancellation = task.cancellation();
            if let Ok(mut active_task) = self.task.lock() {
                *active_task = Some(task);
            }
            if let Ok(mut active_cancellation) = self.task_cancellation.lock() {
                *active_cancellation = Some(cancellation);
            }
        }
        dispatch
    }

    pub(crate) fn status(&self) -> PrefetchStatus {
        let Ok(state) = self.shared.state.lock() else {
            return PrefetchStatus::Failed(PrefetchFailure::Poisoned);
        };
        if let Some(failure) = state.terminal {
            PrefetchStatus::Failed(failure)
        } else if state.stopping {
            PrefetchStatus::Stopped
        } else if state.active.is_some() || state.pending.is_some() {
            PrefetchStatus::Busy {
                replacement_queued: state.pending.is_some(),
            }
        } else {
            PrefetchStatus::Idle
        }
    }

    pub(crate) fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        let Ok(mut state) = self.shared.state.lock() else {
            return false;
        };
        while state.active.is_some() || state.pending.is_some() {
            let now = Instant::now();
            if deadline.is_some_and(|deadline| now >= deadline) {
                return false;
            }
            let remaining = deadline.map_or(Duration::from_millis(10), |deadline| {
                deadline.saturating_duration_since(now)
            });
            let Ok((next, wait)) = self.shared.wake.wait_timeout(state, remaining) else {
                return false;
            };
            state = next;
            if wait.timed_out() && deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return false;
            }
        }
        true
    }

    pub(crate) fn stop_nonblocking(&self) {
        // App cancellation has already reached the shared helper tokens. Do not
        // wait for a cache clone while issuing the UI's shutdown request.
        if let Ok(mut state) = self.shared.state.try_lock() {
            state.stopping = true;
            if let Some(active) = &state.active {
                active.cancellation.cancel(CancelCause::Shutdown);
            }
            if let Some(pending) = state.pending.take() {
                pending.cancellation.cancel(CancelCause::Shutdown);
            }
            self.shared.wake.notify_all();
        }
        if let Ok(mut owner) = self.owner.try_lock()
            && let Some(owner) = owner.as_mut()
        {
            owner.shutdown();
        }
    }

    pub(crate) fn request_shutdown(&self) {
        if let Ok(cancellation) = self.task_cancellation.lock()
            && let Some(cancellation) = cancellation.as_ref()
        {
            cancellation.cancel();
        }
        if let Ok(mut state) = self.shared.state.lock() {
            state.stopping = true;
            if let Some(active) = &state.active {
                active.cancellation.cancel(CancelCause::Shutdown);
            }
            if let Some(pending) = state.pending.take() {
                pending.cancellation.cancel(CancelCause::Shutdown);
            }
            self.shared.wake.notify_all();
        }
    }

    pub(crate) fn join(&self) -> bool {
        self.request_shutdown();
        let owner = match self.owner.lock() {
            Ok(mut owner) => owner.take(),
            Err(_) => return false,
        };
        owner.is_some_and(|owner| {
            matches!(
                owner.shutdown_and_join(),
                BlockingShutdown::Joined { panicked: 0, .. }
            )
        })
    }
}

impl<T: Clone> PrefetchWorker<T> {
    pub(crate) fn cached(&self) -> Option<(Vec<T>, bool)> {
        self.shared
            .state
            .lock()
            .ok()
            .and_then(|state| state.data.clone().map(|data| (data, state.fabricated)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PrefetchPolicy {
    // Keep active and pending work when a periodic request arrives.
    Coalesce,
    // Cancel active work and replace the pending job without overlapping collectors.
    Supersede,
}
