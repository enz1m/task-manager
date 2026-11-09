//! Process stat and identity cache.

use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ProcessIdentityKey {
    pub(crate) comm: String,
    pub(crate) command: String,
}

pub(crate) static PROCESS_IDENTITY_CACHE: OnceLock<
    Mutex<std::collections::HashMap<ProcessIdentityKey, ProcessIdentity>>,
> = OnceLock::new();

#[derive(Clone, Copy, Debug)]
pub(crate) struct ProcessCpuSample {
    pub(crate) user_ticks: u64,
    pub(crate) system_ticks: u64,
    pub(crate) sampled_at: std::time::Instant,
}

pub(crate) type ProcessCpuSamples = HashMap<ProcessTarget, ProcessCpuSample>;

pub(crate) static PROCESS_CPU_PREVIOUS: OnceLock<Mutex<ProcessCpuSamples>> = OnceLock::new();

pub(crate) fn process_cpu_previous() -> &'static Mutex<ProcessCpuSamples> {
    PROCESS_CPU_PREVIOUS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessStat {
    pub(crate) parent_pid: u32,
    pub(crate) user_ticks: u64,
    pub(crate) system_ticks: u64,
    pub(crate) start_time_ticks: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessStatLine {
    pub(crate) stat: ProcessStat,
    pub(crate) kernel_uid: Option<u32>,
    pub(crate) comm: String,
}

// The remainder starts at field 3: utime, stime and starttime are fields 14, 15 and 22.
pub(crate) fn parse_process_stat(text: &str) -> Option<ProcessStat> {
    let (_, remainder) = text.rsplit_once(") ")?;
    let fields: Vec<&str> = remainder.split_whitespace().collect();
    let user = fields.get(11)?.parse::<u64>().ok()?;
    let system = fields.get(12)?.parse::<u64>().ok()?;
    let start_time_ticks = fields.get(19)?.parse::<u64>().ok()?;
    Some(ProcessStat {
        parent_pid: fields.get(1)?.parse().ok()?,
        user_ticks: user,
        system_ticks: system,
        start_time_ticks,
    })
}

pub(crate) fn process_uid_in(directory: &std::fs::File) -> Result<u32, SignalFailure> {
    use std::os::fd::AsRawFd;
    let text = read_action_proc_file(format!("/proc/self/fd/{}/status", directory.as_raw_fd()))?;
    text.lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|uids| uids.split_whitespace().next())
        .and_then(|uid| uid.parse().ok())
        .ok_or(SignalFailure::IdentityUnverifiable)
}

// Kernel comm may contain spaces and parentheses, so its final delimiter is authoritative.
pub(crate) fn parse_process_stat_comm(text: &str) -> Option<&str> {
    let open = text.find('(')?;
    let close = text.rfind(") ")?;
    if close < open + 1 {
        return None;
    }
    text.get(open + 1..close)
}

#[allow(
    unsafe_code,
    reason = "sysconf FFI takes a constant selector and validates its result"
)] // reviewed libc sysconf FFI (inventoried).
pub(crate) fn clock_ticks_per_second() -> f64 {
    // SAFETY: `sysconf` has no pointer arguments and `_SC_CLK_TCK` is a
    // read-only process-global query. A non-positive result is an error.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks > 0 { ticks as f64 } else { 100.0 }
}

pub(crate) fn process_cpu_percent(
    previous: Option<ProcessCpuSample>,
    current: ProcessCpuSample,
    ticks_per_second: f64,
    logical_cpus: usize,
) -> String {
    let Some(previous) = previous else {
        return "—".to_owned();
    };
    let Some(elapsed) = current
        .sampled_at
        .checked_duration_since(previous.sampled_at)
    else {
        return "—".to_owned();
    };
    if elapsed.is_zero()
        || elapsed > crate::model::MAX_SAMPLE_GAP
        || !ticks_per_second.is_finite()
        || ticks_per_second <= 0.0
        || logical_cpus == 0
    {
        return "—".to_owned();
    }
    let delta_ticks = current
        .user_ticks
        .checked_sub(previous.user_ticks)
        .and_then(|user| {
            current
                .system_ticks
                .checked_sub(previous.system_ticks)
                .and_then(|system| user.checked_add(system))
        });
    let Some(delta_ticks) = delta_ticks else {
        return "—".to_owned();
    };
    let percent =
        delta_ticks as f64 / ticks_per_second / elapsed.as_secs_f64() * 100.0 / logical_cpus as f64;
    if !percent.is_finite() || percent > 100.0 {
        return "—".to_owned();
    }
    format!("{percent:.1}%")
}

pub(crate) fn process_cpu_reading(
    previous: &ProcessCpuSamples,
    samples: &mut ProcessCpuSamples,
    target: ProcessTarget,
    current: ProcessCpuSample,
    ticks_per_second: f64,
    logical_cpus: usize,
) -> String {
    let cpu = process_cpu_percent(
        previous.get(&target).copied(),
        current,
        ticks_per_second,
        logical_cpus,
    );
    samples.insert(target, current);
    cpu
}

pub(crate) fn process_identity_cache()
-> &'static Mutex<std::collections::HashMap<ProcessIdentityKey, ProcessIdentity>> {
    PROCESS_IDENTITY_CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

pub(crate) fn derive_process_identity(
    app_index: &DesktopIndex,
    comm: &str,
    command: &str,
) -> ProcessIdentity {
    let process_identity = format!("{comm} {command}").to_ascii_lowercase();
    // Desktop matching may name a helper; classification keeps it out of Apps.
    let app = match_desktop_app(app_index, comm, command);
    let display_name = app.as_ref().map_or_else(
        || display_process_name(comm, command),
        |app| app.name.clone(),
    );
    let kind = classify_process(comm, command, app);
    debug_assert!(
        !(is_obvious_helper_process(&process_identity) && kind == ProcessKind::App.as_str()),
        "a helper row must never be classed App: {process_identity}"
    );
    ProcessIdentity {
        display_name,
        kind,
        protected: is_protected_process(comm, command),
    }
}

pub(crate) fn is_protected_process(comm: &str, command: &str) -> bool {
    let lower = format!("{comm} {command}").to_ascii_lowercase();
    let comm_lower = comm.to_ascii_lowercase();
    if comm_lower.starts_with('[')
        || comm_lower.starts_with("kworker")
        || comm_lower.starts_with("kthread")
    {
        return true;
    }
    [
        "gnome-shell",
        "kwin_wayland",
        "kwin_x11",
        "plasmashell",
        "mutter",
        "cinnamon",
        "mate-session",
        "mate-panel",
        "xfwm4",
        "openbox",
        "i3",
        "sway",
        "hyprland",
        "wayfire",
        "labwc",
        "xorg",
        "xwayland",
        "gdm",
        "sddm",
        "lightdm",
        "systemd",
        "dbus-daemon",
        "polkit",
        "pipewire",
        "wireplumber",
        "pulseaudio",
        "xdg-desktop-portal",
        "login",
        "gnome-session",
        "ksmserver",
        "kded",
        "kactivitymanagerd",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

pub(crate) fn is_protected_service(service: &str) -> bool {
    let lower = service.to_ascii_lowercase();
    [
        "display-manager.service",
        "gdm.service",
        "gdm3.service",
        "sddm.service",
        "lightdm.service",
        "ly.service",
        "systemd-logind.service",
        "dbus.service",
        "polkit.service",
        "accounts-daemon.service",
        "upower.service",
        "pipewire.service",
        "pipewire-pulse.service",
        "wireplumber.service",
        "xdg-desktop-portal.service",
        "xdg-desktop-portal-gnome.service",
        "xdg-desktop-portal-kde.service",
    ]
    .iter()
    .any(|needle| lower == *needle || lower.starts_with(needle.trim_end_matches(".service")))
}
