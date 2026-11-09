//! CPU measurement.

use super::*;

/// A parsed `/proc/stat` sample for the aggregate line and every `cpuN` line.
#[derive(Clone, Debug, Default)]
pub(crate) struct CpuStatSnapshot {
    /// Aggregate `cpu` line jiffies.
    pub(crate) aggregate: CpuTimes,
    /// Counters keyed by the logical CPU number; offline CPUs may leave gaps.
    pub(crate) cores: std::collections::BTreeMap<usize, CpuTimes>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CpuTimes {
    pub(crate) user: u64,
    pub(crate) nice: u64,
    pub(crate) system: u64,
    pub(crate) idle: u64,
    pub(crate) iowait: u64,
    pub(crate) irq: u64,
    pub(crate) softirq: u64,
    pub(crate) steal: u64,
}

/// Parse the multi-line `/proc/stat` payload into an aggregate + per-core snapshot.
pub(crate) fn parse_cpu_stat(text: &str) -> Option<CpuStatSnapshot> {
    let mut aggregate: Option<CpuTimes> = None;
    let mut cores = std::collections::BTreeMap::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(label) = parts.next() else { continue };
        if label != "cpu"
            && !label
                .strip_prefix("cpu")
                .is_some_and(|index| index.parse::<usize>().is_ok())
        {
            continue;
        }
        let mut values = [0_u64; 8];
        for (index, slot) in values.iter_mut().enumerate() {
            match parts.next() {
                Some(token) => *slot = token.parse::<u64>().ok()?,
                None if index < 4 => return None,
                None => break,
            }
        }
        let times = CpuTimes {
            user: values[0],
            nice: values[1],
            system: values[2],
            idle: values[3],
            iowait: values[4],
            irq: values[5],
            softirq: values[6],
            steal: values[7],
        };
        if label == "cpu" {
            aggregate = Some(times);
        } else if let Some(index_str) = label.strip_prefix("cpu")
            && let Ok(index) = index_str.parse::<usize>()
        {
            cores.insert(index, times);
        }
    }
    let aggregate = aggregate?;
    Some(CpuStatSnapshot { aggregate, cores })
}

/// Convert counter pairs using the same busy convention.
/// Newly appearing CPU IDs remain unmeasured until a second sample.
pub(crate) fn cpu_breakdown_from_samples(
    previous: &CpuStatSnapshot,
    current: &CpuStatSnapshot,
) -> CpuBreakdown {
    let cores = current
        .cores
        .iter()
        .map(|(index, current)| CoreLoadData {
            label: index.to_string(),
            percent: previous
                .cores
                .get(index)
                .and_then(|previous| CpuUsage::from_samples(previous, current))
                .map(CpuUsage::busy_percent),
        })
        .collect();
    CpuBreakdown {
        measurement: CpuUsage::from_samples(&previous.aggregate, &current.aggregate),
        cores,
    }
}

// Allow roughly 50 USER_HZ ticks per core before publishing the first delta.
pub(crate) const CPU_PRIME_INTERVAL: Duration = Duration::from_millis(500);

fn read_cpu_stat() -> Option<CpuStatSnapshot> {
    parse_cpu_stat(&fs::read_to_string("/proc/stat").ok()?)
}

pub(crate) fn prime_cpu_breakdown_with<R, S>(
    first: &CpuStatSnapshot,
    mut read: R,
    sleep: S,
) -> (CpuBreakdown, Option<CpuStatSnapshot>)
where
    R: FnMut() -> Option<CpuStatSnapshot>,
    S: FnOnce(Duration),
{
    sleep(CPU_PRIME_INTERVAL);
    match read() {
        Some(second) => (cpu_breakdown_from_samples(first, &second), Some(second)),
        None => (CpuBreakdown::default(), None),
    }
}

pub(crate) fn collect_cpu_breakdown() -> CpuBreakdown {
    let cache = CPU_STAT_PREVIOUS.get_or_init(|| Mutex::new(None));
    let Ok(mut previous) = cache.lock() else {
        return CpuBreakdown::default();
    };
    // Reading under the cache lock keeps concurrent collectors in sample order.
    collect_cpu_breakdown_with(&mut previous, read_cpu_stat, std::thread::sleep)
}

pub(crate) fn collect_cpu_breakdown_with<R, S>(
    previous: &mut Option<CpuStatSnapshot>,
    mut read: R,
    sleep: S,
) -> CpuBreakdown
where
    R: FnMut() -> Option<CpuStatSnapshot>,
    S: FnOnce(Duration),
{
    let Some(current) = read() else {
        *previous = None;
        return CpuBreakdown::default();
    };
    let breakdown = match previous.as_ref() {
        Some(previous) => cpu_breakdown_from_samples(previous, &current),
        None => {
            let (breakdown, second) = prime_cpu_breakdown_with(&current, read, sleep);
            *previous = second;
            return breakdown;
        }
    };
    *previous = Some(current);
    breakdown
}

pub(crate) fn collect_thread_total() -> u64 {
    use std::sync::atomic::Ordering;
    let cached = PROCESS_THREAD_TOTAL.load(Ordering::Relaxed);
    if cached > 0 {
        return cached;
    }
    // Zero is also the uninitialized cache value, so it triggers a direct thread census.
    let Ok(entries) = fs::read_dir("/proc") else {
        return 0;
    };
    let mut total = 0_u64;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let task_dir = entry.path().join("task");
        if let Ok(children) = fs::read_dir(&task_dir) {
            total = total.saturating_add(children.count() as u64);
        }
    }
    total
}

pub(crate) fn gpu_summary() -> GpuReading {
    static COLLECTOR: OnceLock<Mutex<gpu::GpuCollector>> = OnceLock::new();
    match COLLECTOR
        .get_or_init(|| Mutex::new(gpu::GpuCollector::default()))
        .lock()
    {
        Ok(mut collector) => collector.collect_in(Path::new("/sys/class/drm")),
        Err(_) => GpuReading::default(),
    }
}

pub(crate) struct DeviceGpuFacts {
    pub(crate) adapter: Option<String>,
    pub(crate) vram: Option<String>,
}

pub(crate) fn device_gpu_facts() -> DeviceGpuFacts {
    gpu_summary().static_facts()
}

pub(crate) fn gpu_vendor_name(vendor_id: &str) -> &'static str {
    match vendor_id
        .trim_start_matches("0x")
        .to_ascii_lowercase()
        .as_str()
    {
        "1002" | "1022" => "AMD",
        "10de" => "NVIDIA",
        "8086" => "Intel",
        "13b5" => "Arm",
        "1af4" => "Virtio",
        "1234" => "QEMU",
        _ => "Unknown GPU",
    }
}
