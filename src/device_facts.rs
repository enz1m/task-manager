//! One-shot Linux device facts, prepared without I/O and executed on a blocking worker.

use std::collections::HashSet;
use std::ffi::CStr;
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

/// Device metadata. Every field is optional because Linux exposes different
/// subsets by architecture, driver, container and permission boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DeviceFacts {
    pub(crate) cpu_model: Option<String>,
    pub(crate) cpu_topology: Option<String>,
    pub(crate) cpu_temperature: Option<String>,
    pub(crate) memory_total: Option<String>,
    pub(crate) swap_total: Option<String>,
    pub(crate) disk_model: Option<String>,
    pub(crate) disk_mount: Option<String>,
    pub(crate) disk_filesystem: Option<String>,
    pub(crate) network_interface: Option<String>,
    pub(crate) network_state: Option<String>,
    pub(crate) network_ips: Vec<String>,
    pub(crate) gpu_adapter: Option<String>,
    pub(crate) gpu_vram: Option<String>,
    pub(crate) gpu_temperature: Option<String>,
}

impl DeviceFacts {
    /// Whether collection resolved any fact, distinguishing Empty from Current.
    #[must_use]
    pub(crate) fn has_any(&self) -> bool {
        [
            self.cpu_model.as_ref(),
            self.cpu_topology.as_ref(),
            self.cpu_temperature.as_ref(),
            self.memory_total.as_ref(),
            self.swap_total.as_ref(),
            self.disk_model.as_ref(),
            self.disk_mount.as_ref(),
            self.disk_filesystem.as_ref(),
            self.network_interface.as_ref(),
            self.network_state.as_ref(),
            self.network_ips.first(),
            self.gpu_adapter.as_ref(),
            self.gpu_vram.as_ref(),
            self.gpu_temperature.as_ref(),
        ]
        .into_iter()
        .any(|value| value.is_some())
    }
}

/// Injectable source for a prepared, blocking device-facts operation.
pub(crate) trait DeviceFactsSource: Send + Sync {
    /// Prepares an operation without file I/O; execute it on a blocking worker.
    fn prepare(&self) -> DeviceFactsOp;
}

/// Owns a device-facts read that begins only when executed.
pub(crate) struct DeviceFactsOp {
    execute: Box<dyn FnOnce() -> DeviceFacts + Send + 'static>,
}

impl DeviceFactsOp {
    pub(crate) fn new(execute: impl FnOnce() -> DeviceFacts + Send + 'static) -> Self {
        Self {
            execute: Box::new(execute),
        }
    }

    /// Perform the blocking reads. Call only from a blocking worker.
    #[must_use]
    pub(crate) fn execute(self) -> DeviceFacts {
        (self.execute)()
    }
}

// Prepared reads and their results must cross the blocking worker boundary.
const _: () = {
    const fn assert_send_static<T: Send + 'static>() {}
    assert_send_static::<DeviceFactsOp>();
    assert_send_static::<DeviceFacts>();
};

/// Real Linux source. It starts no subprocesses; all data comes from procfs,
/// sysfs, or `getifaddrs(3)`.
#[derive(Debug, Default)]
pub(crate) struct LinuxDeviceFactsSource;

impl DeviceFactsSource for LinuxDeviceFactsSource {
    fn prepare(&self) -> DeviceFactsOp {
        DeviceFactsOp::new(collect_linux_device_facts)
    }
}

fn collect_linux_device_facts() -> DeviceFacts {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok();
    let meminfo = fs::read_to_string("/proc/meminfo").ok();
    let mount = root_mount();
    let network = primary_network();
    let gpu = crate::services::device_gpu_facts();

    DeviceFacts {
        cpu_model: cpuinfo.as_deref().and_then(cpu_model),
        cpu_topology: cpuinfo.as_deref().and_then(cpu_topology),
        cpu_temperature: None,
        memory_total: meminfo
            .as_deref()
            .and_then(|text| meminfo_total(text, "MemTotal:")),
        swap_total: meminfo
            .as_deref()
            .and_then(|text| meminfo_total(text, "SwapTotal:")),
        disk_model: mount.as_ref().and_then(|mount| block_model(&mount.device)),
        disk_mount: mount.as_ref().map(|mount| mount.mount.clone()),
        disk_filesystem: mount.as_ref().map(|mount| mount.filesystem.clone()),
        network_interface: network.as_ref().map(|network| network.name.clone()),
        network_state: network.as_ref().and_then(|network| network.state.clone()),
        network_ips: network
            .as_ref()
            .map(|network| network.ips.clone())
            .unwrap_or_default(),
        gpu_adapter: gpu.adapter,
        gpu_vram: gpu.vram,
        gpu_temperature: None,
    }
}

fn nonempty(value: impl Into<String>) -> Option<String> {
    let value = value.into().trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn cpu_model(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        matches!(key.trim(), "model name" | "Hardware" | "Processor")
            .then(|| tidy_cpu_model(value))
            .and_then(nonempty)
    })
}

pub(crate) fn tidy_cpu_model(model: &str) -> String {
    let mut model = model.to_owned();
    for mark in ["(R)", "(r)", "(TM)", "(tm)"] {
        model = model.replace(mark, "");
    }
    if let Some(at) = model.find(" @ ") {
        model.truncate(at);
        model = model.trim_end().trim_end_matches(" CPU").to_owned();
    }
    model.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn counted(count: usize, noun: &str) -> String {
    let key = match noun {
        "socket" => "tm-socket-count",
        "core" => "tm-core-count",
        _ => "tm-logical-count",
    };
    crate::i18n::message(key, &[("count", crate::i18n::argument(&count))])
}

fn cpu_topology(text: &str) -> Option<String> {
    let blocks: Vec<&str> = text
        .split("\n\n")
        .filter(|block| !block.trim().is_empty())
        .collect();
    if blocks.is_empty() {
        return None;
    }
    let logical = blocks
        .iter()
        .filter(|block| block.lines().any(|line| line.starts_with("processor")))
        .count();
    if logical == 0 {
        return None;
    }
    let mut sockets = HashSet::new();
    let mut cores = HashSet::new();
    for block in blocks {
        let mut physical = None;
        let mut core = None;
        for line in block.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            match key.trim() {
                "physical id" => physical = nonempty(value),
                "core id" => core = nonempty(value),
                _ => {}
            }
        }
        if let Some(physical) = physical.as_ref() {
            sockets.insert(physical.clone());
        }
        if let (Some(physical), Some(core)) = (physical, core) {
            cores.insert((physical, core));
        }
    }
    if sockets.is_empty() || cores.is_empty() {
        Some(counted(logical, "logical processor"))
    } else {
        Some(crate::i18n::message(
            "tm-logical-logical-833ae7",
            &[
                (
                    "arg0",
                    crate::i18n::argument(&(counted(sockets.len(), "socket"))),
                ),
                (
                    "arg1",
                    crate::i18n::argument(&(counted(cores.len(), "core"))),
                ),
                (
                    "arg2",
                    crate::i18n::argument(&counted(logical, "logical processor")),
                ),
            ],
        ))
    }
}

fn meminfo_total(text: &str, key: &str) -> Option<String> {
    let kib = text.lines().find_map(|line| {
        line.strip_prefix(key)?
            .split_whitespace()
            .next()?
            .parse::<u64>()
            .ok()
    })?;
    Some(format_kib(kib))
}

fn format_kib(kib: u64) -> String {
    if kib < 1024 {
        crate::i18n::message(
            "tm-kib-kib-958864",
            &[("arg0", crate::i18n::argument(&(kib)))],
        )
    } else if kib < 1024 * 1024 {
        crate::i18n::message(
            "tm-1-mib-8333f2",
            &[(
                "arg0",
                crate::i18n::argument(&crate::i18n::number_text(format!(
                    "{:.1}",
                    kib as f64 / 1024.0
                ))),
            )],
        )
    } else {
        crate::i18n::message(
            "tm-1-gib-a89d0e",
            &[(
                "arg0",
                crate::i18n::argument(&crate::i18n::number_text(format!(
                    "{:.1}",
                    kib as f64 / 1024.0 / 1024.0
                ))),
            )],
        )
    }
}

#[derive(Debug)]
struct RootMount {
    device: String,
    mount: String,
    filesystem: String,
}

fn root_mount() -> Option<RootMount> {
    let text = fs::read_to_string("/proc/self/mountinfo").ok()?;
    text.lines().find_map(|line| {
        let (left, right) = line.split_once(" - ")?;
        let mut left = left.split_whitespace();
        let mount = left.nth(4)?;
        if mount != "/" {
            return None;
        }
        let mut right = right.split_whitespace();
        let filesystem = nonempty(right.next()?)?;
        let device = nonempty(right.next()?)?;
        Some(RootMount {
            device,
            mount: mount.to_owned(),
            filesystem,
        })
    })
}

fn block_model(device: &str) -> Option<String> {
    let name = Path::new(device).file_name()?.to_str()?;
    let class = PathBuf::from("/sys/class/block").join(name);
    read_trimmed(class.join("device/model")).or_else(|| {
        let canonical = fs::canonicalize(&class).ok()?;
        read_trimmed(canonical.parent()?.join("device/model"))
    })
}

#[derive(Debug)]
struct NetworkFact {
    name: String,
    state: Option<String>,
    ips: Vec<String>,
}

fn primary_network() -> Option<NetworkFact> {
    let mut addresses = interface_addresses();
    addresses.sort_by(|left, right| left.0.cmp(&right.0));
    let mut fallback = None;
    for (name, ip) in addresses {
        if name == "lo" {
            continue;
        }
        let state = read_trimmed(Path::new("/sys/class/net").join(&name).join("operstate"));
        let fact = NetworkFact {
            name,
            state: state.clone(),
            ips: ip,
        };
        if state.as_deref() == Some("up") {
            return Some(fact);
        }
        if fallback.is_none() {
            fallback = Some(fact);
        }
    }
    fallback
}

#[allow(
    unsafe_code,
    reason = "getifaddrs FFI checks the return code and releases the initialized list"
)]
fn interface_addresses() -> Vec<(String, Vec<String>)> {
    let mut result: Vec<(String, Vec<String>)> = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs initializes `head` on success; every pointer is
    // checked before dereference and the list is released exactly once.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return result;
    }
    let mut current = head;
    while !current.is_null() {
        // SAFETY: `current` belongs to the live getifaddrs list.
        let entry = unsafe { &*current };
        if !entry.ifa_name.is_null() && !entry.ifa_addr.is_null() {
            // SAFETY: ifa_name is a NUL-terminated C string by contract.
            let name = unsafe { CStr::from_ptr(entry.ifa_name) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: the non-null address belongs to the live getifaddrs list.
            let family = i32::from(unsafe { (*entry.ifa_addr).sa_family });
            let address = match family {
                libc::AF_INET => {
                    // SAFETY: AF_INET means sockaddr_in.
                    let socket = unsafe { &*(entry.ifa_addr.cast::<libc::sockaddr_in>()) };
                    Some(Ipv4Addr::from(socket.sin_addr.s_addr.to_ne_bytes()).to_string())
                }
                libc::AF_INET6 => {
                    // SAFETY: AF_INET6 means sockaddr_in6.
                    let socket = unsafe { &*(entry.ifa_addr.cast::<libc::sockaddr_in6>()) };
                    Some(Ipv6Addr::from(socket.sin6_addr.s6_addr).to_string())
                }
                _ => None,
            };
            if let Some(address) = address {
                if let Some((_, values)) = result.iter_mut().find(|(seen, _)| seen == &name) {
                    if !values.contains(&address) {
                        values.push(address);
                    }
                } else {
                    result.push((name, vec![address]));
                }
            }
        }
        current = entry.ifa_next;
    }
    // SAFETY: `head` is the pointer returned by successful getifaddrs.
    unsafe { libc::freeifaddrs(head) };
    result
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok().and_then(nonempty)
}
