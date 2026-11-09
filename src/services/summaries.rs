//! Resource summaries and formatters.

use super::*;

pub(crate) fn cpu_summary(measurement: Option<CpuUsage>) -> String {
    measurement.map_or_else(
        || "CPU utilization unavailable".to_owned(),
        |cpu| format!("{}% utilization", cpu.busy_percent()),
    )
}

pub(crate) fn cpu_resource(measurement: Option<CpuUsage>, load_average: String) -> ResourceData {
    let value = measurement.map_or_else(
        || "Unavailable".to_owned(),
        |cpu| format!("{}%", cpu.busy_percent()),
    );
    ResourceData {
        id: "cpu".to_owned(),
        name: "CPU".to_owned(),
        value: value.clone(),
        percent: measurement.map_or(0, CpuUsage::busy_percent),
        color: (52, 152, 219),
        stat1_label: "Utilization".to_owned(),
        stat1_value: value,
        stat3_label: "Load average".to_owned(),
        stat3_value: load_average,
        ..ResourceData::default()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct NetworkCounterSample {
    pub(crate) name: String,
    pub(crate) identity: Option<NetworkIdentity>,
    pub(crate) rx_bytes: Option<u64>,
    pub(crate) tx_bytes: Option<u64>,
    pub(crate) operstate: String,
    pub(crate) speed_mbps: Option<u64>,
}

#[derive(Clone, Debug)]
pub(crate) struct NetworkCounterBatch {
    pub(crate) captured_at: Instant,
    pub(crate) interfaces: Vec<NetworkCounterSample>,
}

#[derive(Default)]
pub(crate) struct NetworkCollector {
    pub(crate) previous: Option<NetworkCounterBatch>,
}

impl NetworkCollector {
    pub(crate) fn sample(&mut self, current: NetworkCounterBatch) -> Vec<NetworkReading> {
        let summaries = match &self.previous {
            Some(previous) => network::readings_from_samples(
                &previous.interfaces,
                &current.interfaces,
                current
                    .captured_at
                    .checked_duration_since(previous.captured_at)
                    .unwrap_or(Duration::ZERO),
            ),
            None => network::readings_from_samples(&[], &current.interfaces, Duration::ZERO),
        };
        // Only the latest observed names retain counters; an observed absence
        // breaks continuity even if the same name later returns with higher counters.
        self.previous = Some(current);
        if summaries.is_empty() {
            vec![NetworkReading::no_adapter()]
        } else {
            summaries
        }
    }
}

pub(crate) fn memory_summary() -> MemoryReading {
    memory_summary_with(|| fs::read_to_string("/proc/meminfo"))
}

pub(crate) fn memory_summary_with(read: impl FnOnce() -> std::io::Result<String>) -> MemoryReading {
    let text = read().ok();
    MemoryReading::from_meminfo(text.as_deref())
}

pub(crate) fn uptime_text() -> String {
    let seconds = fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
        .map_or(0, |value| value as u64);
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let clock = format!("{hours:02}:{minutes:02}");
    if days > 0 {
        crate::i18n::message(
            "tm-uptime-days",
            &[
                ("days", crate::i18n::argument(&days)),
                ("clock", crate::i18n::argument(&clock)),
            ],
        )
    } else {
        crate::i18n::message(
            "tm-uptime-clock",
            &[("clock", crate::i18n::argument(&clock))],
        )
    }
}

pub(crate) fn cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("model name"))
                .and_then(|line| line.split_once(':'))
                .map(|(_, model)| crate::device_facts::tidy_cpu_model(model))
        })
        .unwrap_or_else(|| "Linux CPU".to_owned())
}

#[allow(
    clippy::useless_conversion,
    reason = "libc integer widths vary across supported targets"
)]
pub(crate) fn block_device_major_minor(path: &str) -> Option<(u32, u32)> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = fs::metadata(path).ok()?;
    if !metadata.file_type().is_block_device() {
        return None;
    }
    let device = metadata.rdev();
    Some((
        u32::try_from(libc::major(device)).ok()?,
        u32::try_from(libc::minor(device)).ok()?,
    ))
}

#[allow(
    unsafe_code,
    reason = "statvfs FFI uses a live CString and checked initialized output"
)]
#[allow(
    clippy::useless_conversion,
    reason = "libc integer widths vary across supported targets"
)]
pub(crate) fn root_capacity() -> Option<DiskCapacity> {
    let path = std::ffi::CString::new("/").ok()?;
    let mut raw = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a live NUL-terminated string and `raw` points to
    // writable storage for exactly one statvfs result.
    let status = unsafe { libc::statvfs(path.as_ptr(), raw.as_mut_ptr()) };
    if status != 0 {
        return None;
    }
    // SAFETY: statvfs returned success and initialized the result.
    let raw = unsafe { raw.assume_init() };
    DiskCapacity::from_blocks(
        u64::try_from(raw.f_frsize).ok()?,
        u64::try_from(raw.f_blocks).ok()?,
        u64::try_from(raw.f_bfree).ok()?,
        u64::try_from(raw.f_bavail).ok()?,
    )
}

pub(crate) fn disk_summary() -> DiskReading {
    disk_summary_with(
        DISK_IO_PREVIOUS.get_or_init(|| Mutex::new(DiskIoCollector::default())),
        || {
            let mountinfo = fs::read_to_string("/proc/self/mountinfo").ok();
            let diskstats = fs::read_to_string("/proc/diskstats").ok();
            (mountinfo, diskstats, Instant::now())
        },
        root_capacity(),
        block_device_major_minor,
    )
}

pub(crate) fn disk_summary_with(
    collector: &Mutex<DiskIoCollector>,
    read: impl FnOnce() -> (Option<String>, Option<String>, Instant),
    capacity: Option<DiskCapacity>,
    resolve_device: impl FnOnce(&str) -> Option<(u32, u32)>,
) -> DiskReading {
    let reading = match collector.lock() {
        Ok(mut collector) => {
            let (mountinfo, diskstats, at) = read();
            collector.sample(
                mountinfo.as_deref(),
                diskstats.as_deref(),
                at,
                resolve_device,
            )
        }
        Err(_) => DiskReading::default(),
    };
    reading.with_capacity(capacity)
}

pub(crate) fn format_disk_rate(kib_per_sec: u64) -> String {
    if kib_per_sec < 1024 {
        format!("{kib_per_sec} KiB/s")
    } else if kib_per_sec < 1024 * 1024 {
        format!("{:.1} MiB/s", kib_per_sec as f64 / 1024.0)
    } else {
        format!("{:.1} GiB/s", kib_per_sec as f64 / 1024.0 / 1024.0)
    }
}

// Byte counters need a nonzero elapsed interval, rather than a USER_HZ tick window.
pub(crate) const NETWORK_PRIME_INTERVAL: Duration = Duration::from_millis(100);

// Name-derived resource IDs remain stable; counter continuity additionally
// requires matching ifindex and sysfs object identity on successive reads.

pub(crate) fn collect_network_interfaces() -> Vec<NetworkReading> {
    let cache = NETWORK_PREVIOUS.get_or_init(|| Mutex::new(NetworkCollector::default()));
    let Ok(mut collector) = cache.lock() else {
        return vec![NetworkReading::no_adapter()];
    };
    let current = read_network_counter_batch();
    if collector.previous.is_none() {
        let _ = collector.sample(current);
        std::thread::sleep(NETWORK_PRIME_INTERVAL);
        collector.sample(read_network_counter_batch())
    } else {
        collector.sample(current)
    }
}

pub(crate) fn read_network_counter_batch() -> NetworkCounterBatch {
    network::read_batch_in(Path::new("/sys/class/net"), Instant::now)
}

pub(crate) fn format_rate(kbps: u64) -> String {
    if kbps < 1_000 {
        format!("{kbps} Kbps")
    } else {
        format!("{:.1} Mbps", kbps as f64 / 1_000.0)
    }
}

pub(crate) fn network_resource(iface: &NetworkReading) -> ResourceData {
    iface.resource()
}

pub(crate) fn format_mib(kib: u64) -> String {
    if kib == 0 {
        "0 KiB".to_owned()
    } else {
        format!("{:.1} MiB", kib as f64 / 1024.0)
    }
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    let kib = bytes as f64 / 1024.0;
    if kib < 1024.0 {
        format!("{kib:.0} KiB")
    } else if kib < 1024.0 * 1024.0 {
        format!("{:.1} MiB", kib / 1024.0)
    } else {
        format!("{:.1} GiB", kib / 1024.0 / 1024.0)
    }
}

pub(crate) fn format_percent(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.ends_with('%') {
        trimmed.to_owned()
    } else {
        format!("{trimmed}%")
    }
}
