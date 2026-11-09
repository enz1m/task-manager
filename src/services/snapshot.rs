//! Snapshot data types and collection.

use super::*;

#[derive(Clone, Debug, Default)]
pub(crate) struct ResourceData {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) subtitle: String,
    pub(crate) value: String,
    pub(crate) detail: String,
    pub(crate) percent: i32,
    pub(crate) color: (u8, u8, u8),
    pub(crate) stat1_label: String,
    pub(crate) stat1_value: String,
    pub(crate) stat2_label: String,
    pub(crate) stat2_value: String,
    pub(crate) stat3_label: String,
    pub(crate) stat3_value: String,
    pub(crate) stat4_label: String,
    pub(crate) stat4_value: String,
    pub(crate) stat5_label: String,
    pub(crate) stat5_value: String,
    pub(crate) stat6_label: String,
    pub(crate) stat6_value: String,
    pub(crate) selected: bool,
}

impl ResourceData {
    pub(crate) fn empty() -> Self {
        Self {
            id: "cpu".to_owned(),
            name: "CPU".to_owned(),
            subtitle: "No data".to_owned(),
            value: "Unavailable".to_owned(),
            detail: "Waiting for snapshot".to_owned(),
            percent: 0,
            color: (42, 135, 245),
            stat1_label: "Utilization".to_owned(),
            stat1_value: "Unavailable".to_owned(),
            stat2_label: "Status".to_owned(),
            stat2_value: "No data".to_owned(),
            stat3_label: String::new(),
            stat3_value: String::new(),
            stat4_label: String::new(),
            stat4_value: String::new(),
            stat5_label: String::new(),
            stat5_value: String::new(),
            stat6_label: String::new(),
            stat6_value: String::new(),
            selected: false,
        }
    }
}

/// A destructive-action refusal with shared inspector and announcement wording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ActionRefusal {
    /// The system init process or this task manager.
    SelfOrInit,
    /// A system-owned process requiring elevated privileges for a normal caller.
    SystemPrivilege,
    /// A process belonging to another unprivileged user.
    OwnedByOther { owner: String },
    /// A target excluded by the application's safety policy.
    AppPolicy { target: String },
}

impl ActionRefusal {
    /// Creates a policy refusal with a bounded target name.
    pub(crate) fn app_policy(target: impl Into<String>) -> Self {
        let target = target.into();
        ActionRefusal::AppPolicy {
            target: bounded_subject(&target),
        }
    }

    /// Returns the refusal clause used by announcements and inspector notes.
    pub(crate) fn reason(&self) -> String {
        let label: String = match self {
            ActionRefusal::SelfOrInit => {
                "this process is the system's init or this task manager itself".to_owned()
            }
            ActionRefusal::SystemPrivilege => {
                "this is a system process; ending it needs elevated privileges".to_owned()
            }
            ActionRefusal::OwnedByOther { owner } => crate::i18n::message(
                "tm-this-process-is-owned-by-owner-107add",
                &[("arg0", crate::i18n::argument(&(owner)))],
            ),
            ActionRefusal::AppPolicy { target } => crate::i18n::message(
                "tm-target-is-protected-by-this-application-s-safety-pol-9a2626",
                &[("arg0", crate::i18n::argument(&(target)))],
            ),
        };
        crate::i18n::display(label)
    }

    /// Formats a bounded inspector note explaining unavailable process actions.
    pub(crate) fn inspector_note(&self) -> String {
        bounded_message(crate::i18n::message(
            "tm-end-force-kill-unavailable-1fc2bf",
            &[("arg0", crate::i18n::argument(&(self.reason())))],
        ))
    }
}

/// A process row identity captured when an interaction is created.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct ProcessTarget {
    pub(crate) pid: String,
    pub(crate) start_time_ticks: u64,
    pub(crate) kernel_uid: Option<u32>,
}

impl ProcessTarget {}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProcessData {
    pub(crate) parent_pid: u32,
    /// Nearest same-owner desktop application in the sampled parent chain.
    pub(crate) application_name: Option<String>,
    /// Resident memory in KiB before display rounding, when an exact sample is available.
    pub(crate) rss_kib: Option<u64>,
    pub(crate) pid: String,
    /// Linux `/proc/<pid>/stat` start time, in clock ticks since boot.
    /// Together with `pid` this is the stable process instance identity: a
    /// recycled PID must never inherit an inspector or destructive action.
    /// Zero is reserved for synthetic/test rows whose start time is unknown.
    pub(crate) start_time_ticks: u64,
    pub(crate) kernel_uid: Option<u32>,
    pub(crate) name: String,
    /// Up to 64 KiB of procfs argv joined by spaces, or `[comm]` when argv is empty.
    pub(crate) command_line: String,
    pub(crate) action_allowed: bool,
    pub(crate) cpu: String,
    pub(crate) memory: String,
    pub(crate) rss: String,
    pub(crate) user: String,
    pub(crate) state: String,
    pub(crate) kind: String,
    pub(crate) selected: bool,
}

impl ProcessData {
    pub(crate) fn target(&self) -> ProcessTarget {
        ProcessTarget {
            pid: self.pid.clone(),
            start_time_ticks: self.start_time_ticks,
            kernel_uid: self.kernel_uid,
        }
    }

    pub(crate) fn matches_target(&self, target: &ProcessTarget) -> bool {
        self.pid == target.pid
            && self.start_time_ticks == target.start_time_ticks
            && self.kernel_uid == target.kernel_uid
    }

    /// CPU text shared by the table and inspector. A first cumulative sample
    /// is unknown (`—`), not the malformed `—%` and not a fabricated `0%`.
    pub(crate) fn cpu_display(&self) -> String {
        let value = self.cpu.trim();
        if value.is_empty() || value == "—" {
            "—".to_owned()
        } else {
            format!("{}%", value.trim_end_matches('%'))
        }
    }

    /// Returns the refusal reason when actions are disabled.
    /// Checks self/init, system ownership, other-user ownership, then application policy.
    /// `caller` is the current username; root bypasses only the other-user check.
    pub(crate) fn action_refusal(&self, caller: &str) -> Option<ActionRefusal> {
        if self.action_allowed {
            return None;
        }
        let pid = self.pid.trim().parse::<u32>().unwrap_or(0);
        if pid <= 1 || pid == std::process::id() {
            return Some(ActionRefusal::SelfOrInit);
        }
        if self.user == "root" || self.user == "systemd-resolve" {
            return Some(ActionRefusal::SystemPrivilege);
        }
        if caller != "root" && !self.user.is_empty() && self.user != caller {
            return Some(ActionRefusal::OwnedByOther {
                owner: self.user.clone(),
            });
        }
        Some(ActionRefusal::app_policy(process_subject(
            &self.pid, &self.name,
        )))
    }

    /// Returns a bounded inspector note, or `None` when actions are enabled.
    pub(crate) fn action_block_note(&self, caller: &str) -> Option<String> {
        self.action_refusal(caller).map(|r| r.inspector_note())
    }

    pub(crate) fn empty() -> Self {
        Self {
            parent_pid: 0,
            application_name: None,
            rss_kib: None,
            pid: String::new(),
            start_time_ticks: 0,
            kernel_uid: None,
            name: "No process selected".to_owned(),
            command_line: String::new(),
            action_allowed: false,
            cpu: String::new(),
            memory: String::new(),
            rss: String::new(),
            user: String::new(),
            state: String::new(),
            kind: String::new(),
            selected: false,
        }
    }
}

/// Decodes the primary process state, ignoring trailing modifiers.
/// Empty input becomes "Unknown"; unrecognized primary codes retain the input.
pub(crate) fn decode_process_state(raw: &str) -> String {
    let primary = raw.as_bytes().first().copied();
    match primary {
        Some(b'R') => "Running".to_owned(),
        Some(b'S') => "Sleeping".to_owned(),
        Some(b'D') => "Disk Sleep".to_owned(),
        Some(b'T' | b't') => "Stopped".to_owned(),
        Some(b'Z') => "Zombie".to_owned(),
        Some(b'X') => "Dead".to_owned(),
        Some(b'I') => "Idle".to_owned(),
        Some(b'W') => "Paging".to_owned(),
        _ => {
            if raw.is_empty() {
                "Unknown".to_owned()
            } else {
                raw.to_owned()
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ServiceData {
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) load: String,
    pub(crate) sub_state: String,
    pub(crate) description: String,
    pub(crate) cpu: String,
    pub(crate) memory: String,
    pub(crate) fragment_path: String,
    pub(crate) action_allowed: bool,
    pub(crate) selected: bool,
}

impl ServiceData {
    pub(crate) fn empty() -> Self {
        Self {
            name: "No service selected".to_owned(),
            state: String::new(),
            load: String::new(),
            sub_state: String::new(),
            description: String::new(),
            cpu: String::new(),
            memory: String::new(),
            fragment_path: String::new(),
            action_allowed: false,
            selected: false,
        }
    }

    /// Whether the unit is up (systemd ActiveState active, activating or
    /// reloading), so Stop/Restart apply rather than Start.
    pub(crate) fn is_running(&self) -> bool {
        matches!(self.state.as_str(), "active" | "activating" | "reloading")
    }

    /// Human-readable status string for tooltips (e.g. "active (running)").
    pub(crate) fn status_text(&self) -> String {
        if self.state.is_empty() && self.sub_state.is_empty() {
            return "unknown".to_owned();
        }
        if self.sub_state.is_empty() {
            return self.state.clone();
        }
        format!("{} ({})", self.state, self.sub_state)
    }
}

/// One observation in a rolling history ring.
///
/// A sample carries the monotonic time it was taken at, from the model's clock
/// epoch. The rings are not a 1 Hz series (manual refresh, page re-keys and
/// minimize all add or withhold samples), so every elapsed-time claim derives
/// from `at`, never from the sample index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GraphPointData {
    pub(crate) user: i32,
    pub(crate) system: i32,
    /// When this sample was taken, as a monotonic offset from the model's
    /// epoch, independent of wall-clock adjustments.
    pub(crate) at: Duration,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CoreLoadData {
    pub(crate) label: String,
    /// Busy percent, or `None` when this core has not been measured yet.
    ///
    /// A `/proc/stat` reading is a delta between two samples, so the first
    /// collection has none; `None` keeps that distinct from an idle core.
    pub(crate) percent: Option<i32>,
}

/// A measured aggregate or core CPU interval, in whole percentages.
///
/// Busy includes user, nice, system, IRQ, soft IRQ and steal time; idle includes
/// idle and I/O wait. User/System are their own components, not a complete split
/// of busy when steal is present. Idle is the complement of rounded busy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CpuUsage {
    busy_percent: i32,
    user_percent: i32,
    system_percent: i32,
}

impl CpuUsage {
    pub(crate) fn busy_percent(self) -> i32 {
        self.busy_percent
    }

    pub(crate) fn user_percent(self) -> i32 {
        self.user_percent
    }

    pub(crate) fn system_percent(self) -> i32 {
        self.system_percent
    }

    pub(crate) fn idle_percent(self) -> i32 {
        100 - self.busy_percent
    }

    /// A zero-elapsed, decreasing or overflowing counter interval is unknown.
    pub(crate) fn from_samples(previous: &CpuTimes, current: &CpuTimes) -> Option<Self> {
        let user = current
            .user
            .checked_sub(previous.user)?
            .checked_add(current.nice.checked_sub(previous.nice)?)?;
        let system = current
            .system
            .checked_sub(previous.system)?
            .checked_add(current.irq.checked_sub(previous.irq)?)?
            .checked_add(current.softirq.checked_sub(previous.softirq)?)?;
        let idle = current
            .idle
            .checked_sub(previous.idle)?
            .checked_add(current.iowait.checked_sub(previous.iowait)?)?;
        let steal = current.steal.checked_sub(previous.steal)?;
        let busy = user.checked_add(system)?.checked_add(steal)?;
        let total = busy.checked_add(idle)?;
        if total == 0 {
            return None;
        }
        let percent = |ticks: u64| ((ticks as f64 / total as f64) * 100.0).round() as i32;
        Some(Self {
            busy_percent: percent(busy).clamp(0, 100),
            user_percent: percent(user).clamp(0, 100),
            system_percent: percent(system).clamp(0, 100),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CpuBreakdown {
    /// `None` is unknown, never a measured zero. All aggregate CPU surfaces
    /// derive their utilization from this interval rather than load average.
    pub(crate) measurement: Option<CpuUsage>,
    pub(crate) cores: Vec<CoreLoadData>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Snapshot {
    pub(crate) resources: Vec<ResourceData>,
    pub(crate) processes: Vec<ProcessData>,
    pub(crate) services: Vec<ServiceData>,
    #[cfg_attr(
        not(feature = "center"),
        allow(
            dead_code,
            reason = "summary is consumed by the optional Center connector"
        )
    )]
    pub(crate) uptime: String,
    #[cfg_attr(
        not(feature = "center"),
        allow(
            dead_code,
            reason = "summary is consumed by the optional Center connector"
        )
    )]
    pub(crate) cpu_summary: String,
    #[cfg_attr(
        not(feature = "center"),
        allow(
            dead_code,
            reason = "summary is consumed by the optional Center connector"
        )
    )]
    pub(crate) memory_summary: String,
    pub(crate) memory_reading: MemoryReading,
    pub(crate) disk_reading: DiskReading,
    pub(crate) gpu_reading: GpuReading,
    pub(crate) cpu_temperature: CpuTemperatureReading,
    pub(crate) network_readings: HashMap<String, NetworkReading>,
    pub(crate) thread_total: u64,
    pub(crate) cpu_breakdown: CpuBreakdown,
    /// Process data is unavailable or fabricated, rather than a usable live list.
    pub(crate) processes_fabricated: bool,
    /// Service data is unavailable or fabricated, rather than a usable live list.
    pub(crate) services_fabricated: bool,
}

impl Snapshot {
    pub(crate) fn with_network_resources(mut self) -> Self {
        for resource in self
            .resources
            .iter_mut()
            .filter(|resource| resource.id.starts_with("network"))
        {
            let reading = self.network_readings.get(&resource.id);
            let mut projected = match reading {
                Some(reading) => reading.resource(),
                None => {
                    let mut unknown = NetworkReading::unavailable(resource.name.clone()).resource();
                    unknown.subtitle.clone_from(&resource.subtitle);
                    unknown
                }
            };
            // Display labels and selection do not authorize live measurements.
            projected.id.clone_from(&resource.id);
            projected.name.clone_from(&resource.name);
            projected.selected = resource.selected;
            *resource = projected;
        }
        self
    }

    pub(crate) fn with_cpu_temperature(mut self, reading: CpuTemperatureReading) -> Self {
        // Replace the whole live sample, including identity, before projecting
        // its successor. Unknown never preserves a former sensor's value.
        self.cpu_temperature = CpuTemperatureReading::default();
        for resource in self
            .resources
            .iter_mut()
            .filter(|resource| resource.id == "cpu")
        {
            "Temperature".clone_into(&mut resource.stat6_label);
            resource.stat6_value = reading.value_text();
            resource.detail = reading.sensor_text().unwrap_or_default();
        }
        self.cpu_temperature = reading;
        self
    }
}

/// Process/service domains requested by a collection; telemetry is always read.
/// Unrequested domains may remain cached, but the App's source status must not
/// present those cached observations as newly captured data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SampleScope {
    /// Enumerate procfs processes in this collection.
    pub(crate) processes: bool,
    /// Enumerate systemd units this tick (`systemctl list-units`).
    pub(crate) services: bool,
}

impl SampleScope {
    /// Both process and service probes, used by explicit refresh.
    pub(crate) const ALL: Self = Self {
        processes: true,
        services: true,
    };
}

impl Default for SampleScope {
    fn default() -> Self {
        Self::ALL
    }
}

pub(crate) fn collect_snapshot_with_data(
    processes: Vec<ProcessData>,
    services: Vec<ServiceData>,
    processes_fabricated: bool,
    services_fabricated: bool,
) -> Snapshot {
    let start = std::time::Instant::now();
    let cpu_breakdown = collect_cpu_breakdown();
    let thread_total = collect_thread_total();
    let memory = memory_summary();
    let uptime = uptime_text();
    let disk = disk_summary();
    let network_interfaces = collect_network_interfaces();
    let gpu = gpu_summary();
    let cpu_count = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let load_avg = fs::read_to_string("/proc/loadavg")
        .ok()
        .map(|text| {
            text.split_whitespace()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());

    let cpu_temperature = CpuTemperatureReading::collect_in(
        Path::new("/sys/class/thermal"),
        Path::new("/sys/class/hwmon"),
    );

    let mut resources = vec![
        ResourceData {
            subtitle: cpu_model(),
            detail: crate::i18n::message(
                "tm-processes-edd4da",
                &[("arg0", crate::i18n::argument(&(processes.len())))],
            ),
            stat2_label: "Logical CPUs".to_owned(),
            stat2_value: cpu_count.to_string(),
            stat4_label: "Uptime".to_owned(),
            stat4_value: uptime.clone(),
            stat5_label: "Processes".to_owned(),
            stat5_value: processes.len().to_string(),
            ..cpu_resource(cpu_breakdown.measurement, load_avg)
        },
        memory.resource(),
        disk.resource(),
        gpu.resource(),
    ];

    resources.extend(network_interfaces.iter().map(network_resource));

    let snapshot = Snapshot {
        resources,
        uptime,
        cpu_summary: cpu_summary(cpu_breakdown.measurement),
        memory_summary: memory.summary(),
        memory_reading: memory,
        disk_reading: disk,
        gpu_reading: gpu,
        cpu_temperature: CpuTemperatureReading::default(),
        network_readings: network_interfaces
            .into_iter()
            .map(|reading| (reading.resource_id(), reading))
            .collect(),
        processes,
        services,
        thread_total,
        cpu_breakdown,
        processes_fabricated,
        services_fabricated,
    }
    .with_cpu_temperature(cpu_temperature);
    perf_log("collect_snapshot_with_data (UI thread)", start);
    snapshot
}
