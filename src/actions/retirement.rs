//! Retirement pool: helper custody, reaping, and shutdown.

use super::intents::Report;
use enzui_core::blocking_task::{BlockingShutdown, BlockingTask, BlockingTaskOwner};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const RETIREMENT_CAPACITY: usize = 8;
const RETIREMENT_POLL: Duration = Duration::from_millis(50);
// Process-lifetime custody has no teardown hook: dropping the pool would abandon unfinished helpers.
static RETIREMENT: OnceLock<Option<Arc<RetirementPool>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum RetirementStatus {
    Reserved,
    WaitingOwner,
    WaitingJoin,
    Joining,
    Joined,
    Released,
    SpawnFailedBeforeTake,
    LostAfterTake,
    ReleaseableFault,
    ReaperFailed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JoinFault {
    HelperFailedAfterTake,
    ReaperFailed,
}
pub(crate) type JoinResult = Result<BlockingShutdown, JoinFault>;
pub(super) struct RetirementRecord {
    pub(super) status: AtomicU8,
    pub(super) result: Mutex<Option<JoinResult>>,
    pub(super) reaper_failed: Arc<AtomicBool>,
}
#[derive(Clone)]
pub(crate) struct RetirementReceipt(pub(super) Arc<RetirementRecord>);
impl RetirementReceipt {
    pub(crate) fn status(&self) -> RetirementStatus {
        match self.0.status.load(Ordering::Acquire) {
            0 => RetirementStatus::Reserved,
            1 => RetirementStatus::WaitingOwner,
            2 => RetirementStatus::WaitingJoin,
            3 => RetirementStatus::Joining,
            4 => RetirementStatus::Joined,
            5 => RetirementStatus::Released,
            6 => RetirementStatus::SpawnFailedBeforeTake,
            7 => RetirementStatus::LostAfterTake,
            8 => RetirementStatus::ReleaseableFault,
            _ => RetirementStatus::ReaperFailed,
        }
    }
    pub(crate) fn outcome(&self) -> Option<JoinResult> {
        *self
            .0
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    pub(crate) fn clean_join(&self) -> bool {
        self.status() == RetirementStatus::Joined
            && matches!(
                self.outcome(),
                Some(Ok(BlockingShutdown::Joined { panicked: 0, .. }))
            )
            && !self.0.reaper_failed.load(Ordering::Acquire)
    }
    pub(super) fn set(&self, status: RetirementStatus) {
        self.0.status.store(status as u8, Ordering::Release);
    }
    pub(super) fn publish(&self, result: JoinResult, status: RetirementStatus) {
        *self
            .0
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
        self.set(status);
    }
}
type RetainedOwner = Arc<Mutex<Option<BlockingTaskOwner>>>;
pub(super) enum RetirementResource {
    Free,
    Reserved,
    Owner(RetainedOwner),
    Join {
        handle: Option<JoinHandle<JoinResult>>,
        owner: RetainedOwner,
    },
    LostAfterTake(
        #[expect(dead_code, reason = "retains custody after an unverified owner join")]
        RetainedOwner,
    ),
}
pub(super) struct RetirementSlot {
    pub(super) resource: RetirementResource,
    pub(super) receipt: Option<RetirementReceipt>,
    pub(super) retired: bool,
    pub(super) retry_after: Option<Instant>,
}
impl RetirementSlot {
    pub(super) fn free() -> Self {
        Self {
            resource: RetirementResource::Free,
            receipt: None,
            retired: false,
            retry_after: None,
        }
    }
}
pub(super) struct RetirementPool {
    pub(super) slots: Mutex<Vec<RetirementSlot>>,
    pub(super) changed: Condvar,
    pub(super) worker: Mutex<Option<JoinHandle<()>>>,
    pub(super) failed: Arc<AtomicBool>,
}
struct ReaperWatch<'a>(&'a RetirementPool);
impl Drop for ReaperWatch<'_> {
    fn drop(&mut self) {
        self.0.mark_failed();
    }
}
#[derive(Clone, Copy, Debug)]
pub(super) enum RetirementAdmission {
    Full,
    Unavailable,
}
pub(super) struct RetirementLease {
    pub(super) pool: Arc<RetirementPool>,
    pub(super) index: usize,
    pub(super) receipt: RetirementReceipt,
}
impl RetirementPool {
    pub(super) fn create() -> Option<Arc<Self>> {
        let pool = Arc::new(Self {
            slots: Mutex::new(
                (0..RETIREMENT_CAPACITY)
                    .map(|_| RetirementSlot::free())
                    .collect(),
            ),
            changed: Condvar::new(),
            worker: Mutex::new(None),
            failed: Arc::new(AtomicBool::new(false)),
        });
        let worker_pool = Arc::clone(&pool);
        let worker = std::thread::Builder::new()
            .name("tm-action-reaper".into())
            .spawn(move || {
                let _watch = ReaperWatch(&worker_pool);
                worker_pool.run();
            })
            .ok()?;
        *pool
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(worker);
        Some(pool)
    }
    pub(super) fn global() -> Option<Arc<Self>> {
        RETIREMENT.get_or_init(Self::create).clone()
    }
    pub(super) fn mark_failed(&self) {
        self.failed.store(true, Ordering::Release);
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for slot in slots.iter_mut() {
            if matches!(
                slot.resource,
                RetirementResource::Reserved
                    | RetirementResource::Owner(_)
                    | RetirementResource::Join { .. }
            ) && let Some(receipt) = &slot.receipt
            {
                receipt
                    .0
                    .result
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_or_insert(Err(JoinFault::ReaperFailed));
                receipt.set(RetirementStatus::ReaperFailed);
            }
        }
        self.changed.notify_all();
    }
    pub(super) fn healthy(&self) -> bool {
        if self.failed.load(Ordering::Acquire) {
            return false;
        }
        let stopped = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_none_or(JoinHandle::is_finished);
        if stopped {
            self.mark_failed();
        }
        !self.failed.load(Ordering::Acquire)
    }
    pub(super) fn reserve(self: &Arc<Self>) -> Result<RetirementLease, RetirementAdmission> {
        if !self.healthy() {
            return Err(RetirementAdmission::Unavailable);
        }
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.failed.load(Ordering::Acquire) {
            return Err(RetirementAdmission::Unavailable);
        }
        let Some(index) = slots
            .iter()
            .position(|slot| matches!(slot.resource, RetirementResource::Free))
        else {
            return Err(RetirementAdmission::Full);
        };
        let receipt = RetirementReceipt(Arc::new(RetirementRecord {
            status: AtomicU8::new(RetirementStatus::Reserved as u8),
            result: Mutex::new(None),
            reaper_failed: Arc::clone(&self.failed),
        }));
        slots[index] = RetirementSlot {
            resource: RetirementResource::Reserved,
            receipt: Some(receipt.clone()),
            ..RetirementSlot::free()
        };
        Ok(RetirementLease {
            pool: Arc::clone(self),
            index,
            receipt,
        })
    }
    pub(super) fn start_slot_join(slot: &mut RetirementSlot) -> bool {
        let RetirementResource::Owner(cell) = &slot.resource else {
            return false;
        };
        let mut guard = cell
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(owner) = guard.as_mut() else {
            drop(guard);
            let cell = Arc::clone(cell);
            Self::lose_after_take(slot, cell);
            return false;
        };
        owner.shutdown();
        drop(guard);
        slot.retired = true;
        let worker_cell = Arc::clone(cell);
        match std::thread::Builder::new()
            .name("tm-action-join".into())
            .spawn(move || {
                let owner = {
                    let mut guard = worker_cell
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    guard.take()
                };
                let Some(owner) = owner else {
                    return Err(JoinFault::HelperFailedAfterTake);
                };
                // After this move, a helper unwind cannot prove worker custody through the empty cell.
                let result = owner.shutdown_and_join();
                Ok(result)
            }) {
            Ok(handle) => {
                slot.resource = RetirementResource::Join {
                    handle: Some(handle),
                    owner: Arc::clone(cell),
                };
                slot.retry_after = None;
                if let Some(receipt) = &slot.receipt {
                    receipt.set(RetirementStatus::WaitingJoin);
                }
                true
            }
            Err(_) => {
                slot.retry_after = Instant::now().checked_add(RETIREMENT_POLL);
                if let Some(receipt) = &slot.receipt {
                    receipt.set(RetirementStatus::SpawnFailedBeforeTake);
                }
                false
            }
        }
    }
    pub(super) fn lose_after_take(slot: &mut RetirementSlot, cell: RetainedOwner) {
        if let Some(receipt) = &slot.receipt {
            receipt.publish(
                Err(JoinFault::HelperFailedAfterTake),
                RetirementStatus::LostAfterTake,
            );
        }
        slot.resource = RetirementResource::LostAfterTake(cell);
        slot.retired = false;
        slot.retry_after = None;
    }
    pub(super) fn finish(slot: &mut RetirementSlot, result: JoinResult) {
        let RetirementResource::Join { owner, .. } = &slot.resource else {
            return;
        };
        let cell = Arc::clone(owner);
        let still_owned = cell
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some();
        if let Ok(result @ BlockingShutdown::Joined { panicked, .. }) = result
            && !still_owned
        {
            let Some(receipt) = &slot.receipt else {
                return;
            };
            // Publish the exact outcome before announcing or releasing its capacity.
            receipt.publish(
                Ok(result),
                if panicked == 0 {
                    RetirementStatus::Joined
                } else {
                    RetirementStatus::ReleaseableFault
                },
            );
            *slot = RetirementSlot::free();
        } else if still_owned {
            if let Some(receipt) = &slot.receipt {
                receipt.set(RetirementStatus::SpawnFailedBeforeTake);
            }
            slot.resource = RetirementResource::Owner(cell);
            slot.retired = true;
            slot.retry_after = Instant::now().checked_add(RETIREMENT_POLL);
        } else {
            Self::lose_after_take(slot, cell);
        }
    }
    pub(super) fn run(&self) {
        loop {
            let mut slots = self
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for slot in slots.iter_mut().filter(|slot| slot.retired) {
                if slot.retry_after.is_none_or(|at| Instant::now() >= at) {
                    Self::start_slot_join(slot);
                }
            }
            let ready = slots.iter().position(|slot| matches!(&slot.resource, RetirementResource::Join { handle: Some(handle), .. } if handle.is_finished()));
            if let Some(index) = ready {
                let slot = &mut slots[index];
                if let Some(receipt) = &slot.receipt {
                    receipt.set(RetirementStatus::Joining);
                }
                // Keep the cell and slot visible if joining or classifying the finished helper unwinds.
                let result = match &mut slot.resource {
                    RetirementResource::Join { handle, .. } => handle.take().map(|handle| {
                        handle
                            .join()
                            .unwrap_or(Err(JoinFault::HelperFailedAfterTake))
                    }),
                    _ => None,
                };
                if let Some(result) = result {
                    Self::finish(slot, result);
                }
            } else if slots.iter().any(|slot| slot.retired) {
                drop(
                    self.changed
                        .wait_timeout(slots, RETIREMENT_POLL)
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
            } else {
                drop(
                    self.changed
                        .wait(slots)
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
            }
        }
    }
}
impl RetirementLease {
    pub(super) fn owns(&self, slot: &RetirementSlot) -> bool {
        slot.receipt
            .as_ref()
            .is_some_and(|receipt| Arc::ptr_eq(&receipt.0, &self.receipt.0))
    }
    pub(super) fn install(&self, mut owner: BlockingTaskOwner) {
        let mut slots = self
            .pool
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.pool.failed.load(Ordering::Acquire) {
            owner.shutdown();
        }
        slots[self.index].resource = RetirementResource::Owner(Arc::new(Mutex::new(Some(owner))));
    }
    pub(super) fn live(&self) -> usize {
        let slots = self
            .pool
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &slots[self.index].resource {
            RetirementResource::Owner(cell) if self.owns(&slots[self.index]) => cell
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .map_or(0, |owner| owner.dispatcher().snapshot().live()),
            _ => 0,
        }
    }
    pub(super) fn submit<F>(&self, generation: u64, function: F) -> Option<BlockingTask<Report>>
    where
        F: FnOnce(enzui_core::blocking_task::BlockingTaskContext) -> Report + Send + 'static,
    {
        if !self.pool.healthy() {
            return None;
        }
        let slots = self
            .pool
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &slots[self.index].resource {
            RetirementResource::Owner(cell)
                if self.owns(&slots[self.index]) && !slots[self.index].retired =>
            {
                cell.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .map(|owner| owner.submit(generation, function))
            }
            _ => None,
        }
    }
    pub(super) fn start_join(&self) -> bool {
        if !self.pool.healthy() {
            return false;
        }
        let mut slots = self
            .pool
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = &mut slots[self.index];
        if !self.owns(slot) {
            return self.receipt.clean_join();
        }
        let started = match slot.resource {
            RetirementResource::Join { .. } => true,
            _ => RetirementPool::start_slot_join(slot),
        };
        self.pool.changed.notify_all();
        started
    }
    pub(super) fn poll_join(&self) -> Option<JoinResult> {
        if !self.pool.healthy() {
            return Some(Err(JoinFault::ReaperFailed));
        }
        self.receipt.outcome()
    }
    pub(super) fn clean_join(&self) -> bool {
        self.pool.healthy() && self.receipt.clean_join()
    }
}
impl Drop for RetirementLease {
    fn drop(&mut self) {
        let healthy = self.pool.healthy();
        let mut slots = self
            .pool
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = &mut slots[self.index];
        if !self.owns(slot) {
            return;
        }
        match &slot.resource {
            RetirementResource::Reserved
                if healthy && !self.pool.failed.load(Ordering::Acquire) =>
            {
                self.receipt.set(RetirementStatus::Released);
                *slot = RetirementSlot::free();
            }
            RetirementResource::Owner(cell) => {
                if let Some(owner) = cell
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_mut()
                {
                    owner.shutdown();
                }
                slot.retired = true;
                if healthy && !self.pool.failed.load(Ordering::Acquire) {
                    self.receipt.set(RetirementStatus::WaitingOwner);
                }
            }
            RetirementResource::Join { .. } => {
                slot.retired = true;
            }
            RetirementResource::Reserved
            | RetirementResource::Free
            | RetirementResource::LostAfterTake(_) => {}
        }
        self.pool.changed.notify_all();
    }
}
