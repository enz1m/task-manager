//! Prefetch pool and owners.

use super::*;

pub(crate) fn run_prefetch_jobs<T: Send + 'static>(shared: Arc<PrefetchShared<T>>) {
    loop {
        let job = {
            let Ok(mut state) = shared.state.lock() else {
                return;
            };
            if state.stopping {
                return;
            }
            let Some(job) = state.pending.take() else {
                return;
            };
            state.active = Some(ActivePrefetch {
                generation: job.generation,
                cancellation: job.cancellation.share(),
            });

            job
        };

        let result = catch_unwind(AssertUnwindSafe(|| {
            if job.cancellation.is_terminal() {
                (Vec::new(), true)
            } else {
                (job.collect)(&job.cancellation)
            }
        }));
        let Ok(mut state) = shared.state.lock() else {
            return;
        };
        use crate::collection::{Stamp, Terminal};
        let terminal = if job.cancellation.is_terminal() {
            Terminal::Cancelled
        } else {
            match &result {
                Ok((_, true)) | Err(_) => Terminal::Failed,
                Ok((data, false)) if data.is_empty() => Terminal::Empty,
                _ => Terminal::Current,
            }
        };
        state.last_stamp = Some(Stamp {
            source: job.source,
            generation: job.generation,
            captured_at: Instant::now(),
            terminal,
        });

        if state
            .active
            .as_ref()
            .is_some_and(|active| active.generation == job.generation)
        {
            state.active = None;
        }
        match result {
            Ok((data, fabricated)) => {
                if !state.stopping
                    && !job.cancellation.is_terminal()
                    && state.generation == job.generation
                {
                    state.data = Some(data);
                    state.fabricated = fabricated;
                    state.published_generation = job.generation;
                }
            }
            Err(_) => {
                if let Some(pending) = state.pending.take() {
                    pending.cancellation.cancel(CancelCause::Cancelled);
                }
                state.terminal = Some(PrefetchFailure::Panicked {
                    generation: job.generation,
                });
                state.stopping = true;
                shared.wake.notify_all();
                return;
            }
        }
        shared.wake.notify_all();
    }
}

// Poll worker settlement at bounded intervals to observe cancellation and deadlines.
const WAIT_SLICE: Duration = Duration::from_millis(100);

fn owned_prefetch<T: Clone + Send + 'static>(
    worker: &PrefetchWorker<T>,
    collect: &Arc<PrefetchCollector<T>>,
    token: &CancelToken,
    deadline: Instant,
    source: crate::collection::SourceKind,
) -> Result<(Vec<T>, crate::collection::Stamp), String> {
    use crate::collection::Terminal;
    while matches!(worker.status(), PrefetchStatus::Busy { .. })
        || worker.dispatcher.snapshot().live() != 0
    {
        if token.is_terminal() {
            return Err("Collection cancelled".to_owned());
        }
        if Instant::now() >= deadline {
            return Err(format!("{source:?} collection overdue"));
        }
        if worker.wait_idle(WAIT_SLICE) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    if token.is_terminal() {
        return Err("Collection cancelled".to_owned());
    }
    if let PrefetchDispatch::Unavailable(cause) = worker.request_with_token(
        Arc::clone(collect),
        PrefetchPolicy::Coalesce,
        Some(token),
        source,
    ) {
        return Err(format!("{source:?} collection failed: {cause}"));
    }
    while !worker.wait_idle(WAIT_SLICE) {
        if token.is_terminal() {
            return Err("Collection cancelled".to_owned());
        }
        if Instant::now() >= deadline {
            token.cancel(CancelCause::Cancelled);
            return Err(format!("{source:?} collection overdue"));
        }
    }
    if token.is_terminal() {
        return Err("Collection cancelled".to_owned());
    }
    if let PrefetchStatus::Failed(cause) = worker.status() {
        return Err(format!("{source:?} collection failed: {cause}"));
    }
    let state = worker
        .shared
        .state
        .lock()
        .map_err(|_| "Collection state poisoned")?;
    if state.fabricated || state.published_generation != state.generation {
        return Err(format!("{source:?} helper failed"));
    }
    let data = state
        .data
        .clone()
        .ok_or("Collection produced no settlement")?;
    let stamp = state
        .last_stamp
        .clone()
        .filter(|stamp| {
            stamp.source == source
                && stamp.generation == state.published_generation
                && matches!(stamp.terminal, Terminal::Current | Terminal::Empty)
        })
        .ok_or("Collection lacks a current terminal stamp")?;
    Ok((data, stamp))
}

pub(crate) struct PrefetchPool {
    pub(crate) processes: PrefetchWorker<ProcessData>,
    pub(crate) services: PrefetchWorker<ServiceData>,
    pub(crate) process_collect: Arc<PrefetchCollector<ProcessData>>,
    pub(crate) service_collect: Arc<PrefetchCollector<ServiceData>>,
}

impl PrefetchPool {
    fn system() -> Self {
        Self::with_collectors(collect_processes_with, collect_services_with)
    }

    fn with_collectors<P, S>(process_collect: P, service_collect: S) -> Self
    where
        P: Fn(&CancelToken) -> (Vec<ProcessData>, bool) + Send + Sync + 'static,
        S: Fn(&CancelToken) -> (Vec<ServiceData>, bool) + Send + Sync + 'static,
    {
        Self {
            processes: PrefetchWorker::spawn("tm-process-scan"),
            services: PrefetchWorker::spawn("tm-service-scan"),
            process_collect: Arc::new(process_collect),
            service_collect: Arc::new(service_collect),
        }
    }

    pub(crate) fn collect_owned(
        &self,
        scope: SampleScope,
        token: &CancelToken,
    ) -> Result<crate::collection::Collected, String> {
        self.collect_owned_with(scope, token, collect_snapshot_with_data)
    }

    pub(crate) fn collect_owned_with(
        &self,
        scope: SampleScope,
        token: &CancelToken,
        telemetry: impl FnOnce(Vec<ProcessData>, Vec<ServiceData>, bool, bool) -> Snapshot,
    ) -> Result<crate::collection::Collected, String> {
        use crate::collection::{SourceKind, Stamp, Terminal};
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut stamps = Vec::new();
        let processes = if scope.processes {
            let (data, stamp) = owned_prefetch(
                &self.processes,
                &self.process_collect,
                token,
                deadline,
                SourceKind::Processes,
            )?;
            stamps.push(stamp);
            data
        } else {
            self.processes
                .cached()
                .filter(|(_, failed)| !failed)
                .map(|(data, _)| data)
                .unwrap_or_default()
        };
        let services = if scope.services {
            let (data, stamp) = owned_prefetch(
                &self.services,
                &self.service_collect,
                token,
                deadline,
                SourceKind::Services,
            )?;
            stamps.push(stamp);
            data
        } else {
            self.services
                .cached()
                .filter(|(_, failed)| !failed)
                .map(|(data, _)| data)
                .unwrap_or_default()
        };
        if token.is_terminal() {
            return Err("Collection cancelled".to_owned());
        }
        let snapshot = telemetry(processes, services, false, false);
        if token.is_terminal() {
            return Err("Collection cancelled".to_owned());
        }
        stamps.push(Stamp {
            source: SourceKind::Telemetry,
            generation: 0,
            captured_at: Instant::now(),
            terminal: Terminal::Current,
        });
        Ok(crate::collection::Collected { snapshot, stamps })
    }

    pub(crate) fn live(&self) -> usize {
        self.processes.dispatcher.snapshot().live() + self.services.dispatcher.snapshot().live()
    }

    pub(crate) fn stop(&self) {
        self.processes.stop_nonblocking();
        self.services.stop_nonblocking();
    }
}

pub(crate) struct PrefetchOwner {
    pool: Arc<PrefetchPool>,
}

impl PrefetchOwner {
    pub(crate) fn start() -> Self {
        Self {
            pool: Arc::new(PrefetchPool::system()),
        }
    }

    pub(crate) fn pool(&self) -> Arc<PrefetchPool> {
        Arc::clone(&self.pool)
    }
    pub(crate) fn live(&self) -> usize {
        self.pool.live()
    }
    pub(crate) fn stop(&self) {
        self.pool.stop();
    }

    pub(crate) fn join(self) -> bool {
        self.pool.stop();
        let processes = self.pool.processes.join();
        let services = self.pool.services.join();
        processes && services
    }
}
