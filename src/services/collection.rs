//! Process and service collection.

use super::*;

pub(crate) fn collect_processes_with(cancellation: &CancelToken) -> (Vec<ProcessData>, bool) {
    collect_processes_in(std::path::Path::new("/proc"), cancellation)
}

struct ScannedProcess {
    pid: String,
    stat_line: ProcessStatLine,
    state: String,
    threads: u64,
    rss_kib: u64,
    command: String,
}

pub(crate) fn collect_processes_in(
    proc_root: &std::path::Path,
    cancellation: &CancelToken,
) -> (Vec<ProcessData>, bool) {
    use std::sync::atomic::Ordering;
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return (Vec::new(), true);
    };
    let previous_samples = process_cpu_previous()
        .lock()
        .ok()
        .map(|mut samples| std::mem::take(&mut *samples))
        .unwrap_or_default();
    let mut samples = ProcessCpuSamples::new();
    let start = std::time::Instant::now();
    let app_index = desktop_app_index();
    let mem_total_kib = read_mem_total_kib(proc_root);

    let mut scanned = Vec::new();
    for entry in entries.flatten() {
        if cancellation.is_terminal() {
            return (Vec::new(), true);
        }
        let Ok(pid) = entry.file_name().into_string() else {
            continue;
        };
        if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        if let Some(process) = scan_process(&entry.path(), pid) {
            scanned.push(process);
        }
    }
    let uptime_ticks = read_uptime_ticks(proc_root);
    scanned.sort_by(|a, b| {
        lifetime_share(b, uptime_ticks).total_cmp(&lifetime_share(a, uptime_ticks))
    });

    let mut rows = Vec::new();
    let mut thread_total: u64 = 0;
    let sampled_at = std::time::Instant::now();
    let ticks_per_second = clock_ticks_per_second();
    let logical_cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    let caller = current_user();
    // Prune identities absent from this scan to bound cache growth under process churn.
    let mut seen_identities: std::collections::HashSet<ProcessIdentityKey> =
        std::collections::HashSet::new();
    for row in scanned {
        let ScannedProcess {
            pid,
            stat_line,
            state,
            threads,
            rss_kib,
            command,
        } = row;
        thread_total = thread_total.saturating_add(threads);
        let comm = stat_line.comm;
        let user = user_name(stat_line.kernel_uid);
        let start_time_ticks = stat_line.stat.start_time_ticks;
        let process_instance = ProcessTarget {
            pid: pid.clone(),
            start_time_ticks,
            kernel_uid: stat_line.kernel_uid,
        };
        let current = ProcessCpuSample {
            user_ticks: stat_line.stat.user_ticks,
            system_ticks: stat_line.stat.system_ticks,
            sampled_at,
        };
        let cpu = process_cpu_reading(
            &previous_samples,
            &mut samples,
            process_instance,
            current,
            ticks_per_second,
            logical_cpus,
        );
        let identity_key = ProcessIdentityKey {
            comm: comm.clone(),
            command: command.clone(),
        };
        let identity = {
            let cached = process_identity_cache()
                .lock()
                .ok()
                .and_then(|cache| cache.get(&identity_key).cloned());
            match cached {
                Some(identity) => identity,
                None => {
                    let identity = derive_process_identity(app_index, &comm, &command);
                    if let Ok(mut cache) = process_identity_cache().lock() {
                        cache.insert(identity_key.clone(), identity.clone());
                    }
                    identity
                }
            }
        };
        seen_identities.insert(identity_key);
        let display_name = identity.display_name.clone();
        let kind = identity.kind.clone();
        let action_allowed = row_action_allowed(&identity, &pid, &user, &caller);
        rows.push(ProcessData {
            parent_pid: stat_line.stat.parent_pid,
            application_name: None,
            rss_kib: Some(rss_kib),
            pid,
            start_time_ticks,
            kernel_uid: stat_line.kernel_uid,
            name: display_name.clone(),
            command_line: command,
            action_allowed,
            cpu,
            memory: format_percent(&percent_of(rss_kib, mem_total_kib)),
            rss: format_mib(rss_kib),
            user,
            state: decode_process_state(&state),
            kind,
            selected: false,
        });
    }

    attribute_application_trees(&mut rows);
    if let Ok(mut cache) = process_identity_cache().lock() {
        cache.retain(|key, _| seen_identities.contains(key));
    }
    if let Ok(mut previous) = process_cpu_previous().lock() {
        *previous = samples;
    }
    PROCESS_THREAD_TOTAL.store(thread_total, Ordering::Relaxed);
    perf_log("collect_processes (background)", start);

    let fabricated = rows.is_empty();
    (rows, fabricated)
}

fn scan_process(directory: &std::path::Path, pid: String) -> Option<ScannedProcess> {
    use std::os::fd::AsRawFd;
    // One open proc directory prevents PID reuse from mixing files from two processes.
    let handle = std::fs::File::open(directory).ok()?;
    let fd = handle.as_raw_fd();
    let stat = read_action_proc_file(format!("/proc/self/fd/{fd}/stat")).ok()?;
    let counters = parse_process_stat(&stat)?;
    let comm = parse_process_stat_comm(&stat)
        .unwrap_or_default()
        .to_owned();
    let status = read_action_proc_file(format!("/proc/self/fd/{fd}/status")).ok()?;
    let fields = parse_process_status(&status);
    let argv = read_cmdline(&format!("/proc/self/fd/{fd}/cmdline"));
    let command = if argv.is_empty() {
        format!("[{comm}]")
    } else {
        argv
    };
    Some(ScannedProcess {
        pid,
        stat_line: ProcessStatLine {
            stat: counters,
            kernel_uid: fields.uid,
            comm,
        },
        state: fields.state,
        threads: fields.threads,
        rss_kib: fields.rss_kib,
        command,
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusFields {
    pub(crate) state: String,
    pub(crate) threads: u64,
    /// Zero for a kernel thread, which has no `VmRSS` line.
    pub(crate) rss_kib: u64,
    pub(crate) uid: Option<u32>,
}

pub(crate) fn parse_process_status(text: &str) -> StatusFields {
    let mut fields = StatusFields::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let first = value.split_whitespace().next().unwrap_or("");
        match key {
            "State" => first.clone_into(&mut fields.state),
            "Threads" => fields.threads = first.parse().unwrap_or(0),
            "VmRSS" => fields.rss_kib = first.parse().unwrap_or(0),
            "Uid" => fields.uid = first.parse().ok(),
            _ => {}
        }
    }
    fields
}

// Retain at most 64 KiB of argv, replacing NUL separators with spaces.
fn read_cmdline(path: &str) -> String {
    use std::io::Read;
    const CMDLINE_MAX_BYTES: u64 = 64 * 1024;
    let mut bytes = Vec::new();
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.take(CMDLINE_MAX_BYTES).read_to_end(&mut bytes);
    }
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    for byte in &mut bytes {
        if *byte == 0 {
            *byte = b' ';
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn lifetime_share(process: &ScannedProcess, uptime_ticks: u64) -> f64 {
    let stat = process.stat_line.stat;
    let alive = uptime_ticks.saturating_sub(stat.start_time_ticks).max(1);
    (stat.user_ticks + stat.system_ticks) as f64 / alive as f64
}

fn read_mem_total_kib(proc_root: &std::path::Path) -> u64 {
    std::fs::read_to_string(proc_root.join("meminfo"))
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("MemTotal:"))
                .and_then(|value| value.split_whitespace().next())
                .and_then(|kib| kib.parse().ok())
        })
        .unwrap_or(0)
}

fn read_uptime_ticks(proc_root: &std::path::Path) -> u64 {
    let seconds = std::fs::read_to_string(proc_root.join("uptime"))
        .ok()
        .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0);
    (seconds * clock_ticks_per_second()) as u64
}

fn percent_of(rss_kib: u64, mem_total_kib: u64) -> String {
    if mem_total_kib == 0 {
        return "0.0".to_owned();
    }
    format!("{:.1}", rss_kib as f64 * 100.0 / mem_total_kib as f64)
}

// Match the caller by UID even when its account exists only through NSS.
fn user_name(uid: Option<u32>) -> String {
    static PASSWD: std::sync::LazyLock<HashMap<u32, String>> = std::sync::LazyLock::new(|| {
        std::fs::read_to_string("/etc/passwd")
            .map(|text| parse_passwd(&text))
            .unwrap_or_default()
    });
    let Some(uid) = uid else {
        return String::new();
    };
    if uid == 0 {
        return "root".to_owned();
    }
    if uid == rustix::process::getuid().as_raw() {
        return current_user();
    }
    PASSWD.get(&uid).cloned().unwrap_or_else(|| uid.to_string())
}

pub(crate) fn parse_passwd(text: &str) -> HashMap<u32, String> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let uid = fields.nth(1)?.parse().ok()?;
            (!name.is_empty()).then(|| (uid, name.to_owned()))
        })
        .collect()
}

pub(crate) const DEFAULT_CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Cumulative cgroup CPU time and current memory; missing readings remain unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ServiceUsage {
    /// Cumulative CPU time consumed by the unit, in nanoseconds.
    pub(crate) cpu_nsec: Option<u64>,
    /// The unit's current memory footprint, in bytes.
    pub(crate) memory_bytes: Option<u64>,
}

pub(crate) fn parse_cgroup_cpu_usage_usec(text: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(rest) = line.trim().strip_prefix("usage_usec") {
            return rest.trim().parse::<u64>().ok();
        }
    }
    None
}

pub(crate) fn read_service_usage_in(cgroup_root: &Path, unit: &str) -> ServiceUsage {
    let unit = unit.trim();
    if unit.is_empty() {
        return ServiceUsage::default();
    }
    for rel in [format!("system.slice/{unit}"), unit.to_owned()] {
        let base = cgroup_root.join(&rel);
        let mem = fs::read_to_string(base.join("memory.current"))
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok());
        let cpu = fs::read_to_string(base.join("cpu.stat"))
            .ok()
            .and_then(|t| parse_cgroup_cpu_usage_usec(&t))
            .map(|usec| usec.saturating_mul(1_000));
        if mem.is_some() || cpu.is_some() {
            return ServiceUsage {
                cpu_nsec: cpu,
                memory_bytes: mem,
            };
        }
    }
    ServiceUsage::default()
}

pub(crate) fn format_cpu_time(nsec: u64) -> String {
    let secs = nsec as f64 / 1_000_000_000.0;
    if secs < 1.0 {
        format!("{:.0} ms", nsec as f64 / 1_000_000.0)
    } else if secs < 60.0 {
        format!("{secs:.1} s")
    } else {
        let total = secs as u64;
        format!("{}m {:02}s", total / 60, total % 60)
    }
}

/// Formats CPU duration and memory size, using empty strings for missing readings.
pub(crate) fn format_service_usage(usage: &ServiceUsage) -> (String, String) {
    let cpu = usage.cpu_nsec.map(format_cpu_time).unwrap_or_default();
    let memory = usage.memory_bytes.map(format_bytes).unwrap_or_default();
    (cpu, memory)
}

pub(crate) fn collect_services_with(cancellation: &CancelToken) -> (Vec<ServiceData>, bool) {
    let mut run = run_request;
    collect_services_via(cancellation, &mut run)
}

pub(crate) fn collect_services_via(
    cancellation: &CancelToken,
    run: &mut impl FnMut(ProcessRequest, &CancelToken) -> Result<ProcessOutput, ProcessError>,
) -> (Vec<ServiceData>, bool) {
    // `list-units` and the `show` fragment fetch share one deadline, so the
    // compound refresh cannot run for twice the ceiling.
    collect_services_budgeted_via(SNAPSHOT_HELPER_DEADLINE, cancellation, run)
}

pub(crate) fn collect_services_budgeted_via(
    route_deadline: Duration,
    cancellation: &CancelToken,
    run: &mut impl FnMut(ProcessRequest, &CancelToken) -> Result<ProcessOutput, ProcessError>,
) -> (Vec<ServiceData>, bool) {
    let budget = RouteBudget::new(route_deadline);
    let request = ProcessRequest::new(
        SYSTEMCTL_EXE,
        StdinPolicy::Null,
        SERVICES_STDOUT_MAX_BYTES,
        HELPER_STDERR_MAX_BYTES,
        budget.remaining(),
    )
    .args([
        "list-units",
        "--type=service",
        "--all",
        "--no-legend",
        "--no-pager",
    ]);
    let output = match run(request, cancellation) {
        Ok(output) if output.status.success() => output,
        _ => return (Vec::new(), true),
    };

    let mut service_names = Vec::new();
    let mut services = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let cleaned = line.trim_start().trim_start_matches('●').trim_start();
        let parts: Vec<&str> = cleaned.split_whitespace().collect();
        if parts.len() < 5 {
            continue;
        }
        service_names.push(parts[0].to_owned());
        services.push(ServiceData {
            name: parts[0].to_owned(),
            load: parts[1].to_owned(),
            state: parts[2].to_owned(),
            sub_state: parts[3].to_owned(),
            description: parts[4..].join(" "),
            cpu: String::new(),
            memory: String::new(),
            fragment_path: String::new(),
            action_allowed: !is_protected_service(parts[0]),
            selected: false,
        });
    }

    if services.is_empty() {
        return (
            Vec::new(),
            !output.stdout.iter().all(u8::is_ascii_whitespace),
        );
    }

    // Keep the primary unit list when too little shared budget remains for metadata.
    let fragment_paths = match budget.next_child_deadline() {
        Some(remaining) => batch_fragment_paths_via(&service_names, remaining, cancellation, run),
        None => std::collections::HashMap::new(),
    };
    let cgroup_root = Path::new(DEFAULT_CGROUP_ROOT);
    for service in &mut services {
        if let Some(path) = fragment_paths.get(&service.name) {
            service.fragment_path = path.clone();
        }
        let (cpu, memory) =
            format_service_usage(&read_service_usage_in(cgroup_root, &service.name));
        service.cpu = cpu;
        service.memory = memory;
    }

    (services, false)
}

fn attribute_application_trees(rows: &mut [ProcessData]) {
    let by_pid: HashMap<u32, usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| row.pid.parse().ok().map(|pid| (pid, index)))
        .collect();
    let mut names: Vec<Option<String>> = rows
        .iter()
        .map(|row| (row.kind == "App").then(|| row.name.clone()))
        .collect();
    let mut resolved: Vec<bool> = names.iter().map(Option::is_some).collect();
    for origin in 0..rows.len() {
        if resolved[origin] {
            continue;
        }
        let mut path = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut current = origin;
        let name = loop {
            if resolved[current] {
                break names[current].clone();
            }
            if !visited.insert(current) {
                break None;
            }
            path.push(current);
            let Some(&parent) = by_pid.get(&rows[current].parent_pid) else {
                break None;
            };
            if rows[current].kernel_uid.is_none()
                || rows[current].kernel_uid != rows[parent].kernel_uid
                || rows[parent].start_time_ticks > rows[current].start_time_ticks
            {
                break None;
            }
            current = parent;
        };
        for index in path {
            names[index].clone_from(&name);
            resolved[index] = true;
        }
    }
    for (row, name) in rows.iter_mut().zip(names) {
        row.application_name = name;
    }
}

#[cfg(test)]
mod application_tree_tests {
    use super::*;
    fn row(pid: u32, parent: u32, kind: &str, name: &str, kib: u64, cpu: &str) -> ProcessData {
        ProcessData {
            pid: pid.to_string(),
            parent_pid: parent,
            kind: kind.into(),
            name: name.into(),
            kernel_uid: Some(1000),
            start_time_ticks: u64::from(pid),
            rss_kib: Some(kib),
            rss: format_mib(kib),
            cpu: cpu.into(),
            ..Default::default()
        }
    }
    #[test]
    fn application_totals_include_nested_helpers_and_keep_real_process_identity() {
        let mut rows = vec![
            row(10, 1, "App", "Zen Browser", 512 * 1024, "1.0%"),
            row(11, 10, "System", "Web Content", 1024 * 1024, "3.0%"),
            row(12, 11, "System", "GPU Process", 128 * 1024, "2.0%"),
            row(20, 1, "System", "unrelated", 1024 * 1024, "9.0%"),
        ];
        let targets: Vec<_> = rows.iter().map(ProcessData::target).collect();
        attribute_application_trees(&mut rows);
        assert_eq!(rows[2].application_name.as_deref(), Some("Zen Browser"));
        assert!(rows[3].application_name.is_none());
        assert_eq!(
            targets,
            rows.iter().map(ProcessData::target).collect::<Vec<_>>()
        );
        let mut model = crate::model::Model::from_snapshot(Snapshot {
            processes: rows,
            ..Default::default()
        });
        model.select_page(crate::model::Page::Apps);
        let table = model.process_table();
        let group = &table.rows()[0];
        assert_eq!(group.name, "Zen Browser");
        assert_eq!(group.cpu, "6.0%");
        assert_eq!(group.rss, "1664.0 MiB");
        assert_eq!(table.process_count(), 3);
        assert!(group.pid.is_empty());
        assert_eq!(model.visible_processes(false).len(), 1);
        model.query = "zen".into();
        assert_eq!(model.visible_processes(true).len(), 3);
    }
    #[test]
    fn attribution_rejects_changed_owners_younger_parents_and_cycles() {
        let mut rows = vec![
            row(10, 1, "App", "Zen Browser", 1, "1.0%"),
            row(11, 10, "System", "other owner", 1, "1.0%"),
            row(12, 10, "System", "reused parent", 1, "1.0%"),
            row(20, 21, "System", "cycle", 1, "1.0%"),
            row(21, 20, "System", "cycle", 1, "1.0%"),
        ];
        rows[1].kernel_uid = Some(1001);
        rows[2].start_time_ticks = 5;
        attribute_application_trees(&mut rows);
        assert!(rows[1..].iter().all(|row| row.application_name.is_none()));
    }
}
