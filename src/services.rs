//! Linux platform layer (procfs / sysfs / systemctl / DRM).
//!
//! External short commands route through the framework's bounded process
//! transport, while this module keeps Task Manager-specific argv, limits,
//! fallbacks, and cache policy.

use std::collections::HashMap;
use std::fs;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use enzui_core::blocking_task::{
    BlockingShutdown, BlockingTask, BlockingTaskCancellation, BlockingTaskDispatcher,
    BlockingTaskOwner,
};
use enzui_core::cancellation::{CancelCause, CancelToken};
use enzui_services::process_transport::{
    self, OutputStream, ProcessError, ProcessErrorKind, ProcessOutput, ProcessRequest, StdinPolicy,
};
use std::time::{Duration, Instant};

mod memory;
pub(crate) use memory::{MemoryReading, MemoryUsage};

mod disk;
pub(crate) use disk::DiskIoCollector;
pub(crate) use disk::{DiskActivity, DiskCapacity, DiskReading};

mod gpu;
pub(crate) use gpu::{GpuBusy, GpuReading};

mod cpu_temperature;
pub(crate) use cpu_temperature::CpuTemperatureReading;

mod network;
mod prefetch_types;
pub(crate) use prefetch_types::{
    ActivePrefetch, FRAGMENT_STDOUT_MAX_BYTES, HELPER_STDERR_MAX_BYTES, JOURNALCTL_EXE,
    PrefetchCollector, PrefetchDispatch, PrefetchFailure, PrefetchPolicy, PrefetchShared,
    PrefetchStatus, PrefetchWorker, RouteBudget, SERVICE_LOG_DEADLINE, SERVICES_STDOUT_MAX_BYTES,
    SNAPSHOT_HELPER_DEADLINE, SYSTEMCTL_EXE, run_request,
};
mod prefetch_pool;
pub(crate) use prefetch_pool::{PrefetchOwner, run_prefetch_jobs};
static CPU_STAT_PREVIOUS: OnceLock<Mutex<Option<CpuStatSnapshot>>> = OnceLock::new();
static NETWORK_PREVIOUS: OnceLock<Mutex<NetworkCollector>> = OnceLock::new();
static DISK_IO_PREVIOUS: OnceLock<Mutex<DiskIoCollector>> = OnceLock::new();

// Updated during process scanning so the UI does not walk /proc/*/status.
static PROCESS_THREAD_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

mod actions;
mod collection;
pub(crate) mod desktop_names;
pub(crate) use desktop_names::ProcessKind;
pub(crate) use desktop_names::{
    DesktopIndex, classify_process, desktop_app_index, display_process_name,
    is_obvious_helper_process, match_desktop_app,
};
mod summaries;
#[cfg(test)]
pub(crate) use collection::collect_processes_in;
pub(crate) use collection::{collect_processes_with, collect_services_with};
#[cfg(test)]
pub(crate) use summaries::disk_summary_with;
pub(crate) use summaries::{
    NetworkCollector, NetworkCounterBatch, NetworkCounterSample, collect_network_interfaces,
    cpu_model, cpu_resource, cpu_summary, disk_summary, format_bytes, format_disk_rate, format_mib,
    format_percent, format_rate, memory_summary, network_resource, uptime_text,
};
mod fragments;
pub(crate) use fragments::batch_fragment_paths_via;
mod signals;
pub(crate) use signals::current_user;
#[cfg(test)]
pub(crate) use signals::process_action_allowed;
pub(crate) use signals::{ActionOutcome, Urgency};
pub(crate) use signals::{
    LinuxProcessSignalBackend, PinnedProcess, ProcessIdentity, ProcessSignalBackend, SignalFailure,
    process_subject, read_action_proc_file, row_action_allowed,
};
mod process_stat;
pub(crate) use actions::{ProcessAction, Signal};
#[cfg(test)]
pub(crate) use process_stat::is_protected_process;
pub(crate) use process_stat::{
    ProcessCpuSample, ProcessCpuSamples, ProcessIdentityKey, ProcessStatLine,
    clock_ticks_per_second, derive_process_identity, is_protected_service, parse_process_stat,
    parse_process_stat_comm, process_cpu_previous, process_cpu_reading, process_identity_cache,
    process_uid_in,
};
mod helper_text;
pub(crate) use helper_text::{
    HelperRoute, bounded_detail, bounded_message, bounded_subject, helper_failure_message,
};
mod logs;
pub(crate) use logs::{journal_tail_caption, service_logs};
mod snapshot;
pub(crate) use snapshot::collect_snapshot_with_data;
pub(crate) use snapshot::{
    ActionRefusal, CoreLoadData, CpuBreakdown, CpuUsage, GraphPointData, ProcessData,
    ProcessTarget, ResourceData, SampleScope, ServiceData, Snapshot, decode_process_state,
};
mod cpu;
#[cfg(test)]
pub(crate) use cpu::collect_cpu_breakdown_with;
#[cfg(test)]
pub(crate) use cpu::parse_cpu_stat;
pub(crate) use cpu::{CpuStatSnapshot, CpuTimes};
pub(crate) use cpu::{
    DeviceGpuFacts, collect_cpu_breakdown, collect_thread_total, device_gpu_facts, gpu_summary,
    gpu_vendor_name,
};
use network::NetworkIdentity;
pub(crate) use network::NetworkReading;

// Cache the first environment reading; later changes do not affect perf logging.
pub(crate) fn perf_enabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    static CACHED: AtomicU8 = AtomicU8::new(0);
    match CACHED.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = std::env::var("ENZUI_TASK_MANAGER_PERF")
                .is_ok_and(|value| !value.is_empty() && value != "0");
            CACHED.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

pub(crate) fn perf_log(label: &str, start: std::time::Instant) {
    if perf_enabled() {
        eprintln!(
            "[task-manager perf] {label}: {}ms",
            start.elapsed().as_millis()
        );
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod cpu_measurement_tests;

#[cfg(test)]
mod memory_measurement_tests;

#[cfg(test)]
mod disk_measurement_tests;

#[cfg(test)]
mod gpu_measurement_tests;

#[cfg(test)]
pub(crate) mod cpu_temperature_authority_tests;

#[cfg(test)]
pub(crate) mod network_measurement_tests;
