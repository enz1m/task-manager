use super::{DeviceGpuFacts, ResourceData, format_bytes, gpu_vendor_name};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GpuIdentityKey {
    device: PathBuf,
    filesystem: u64,
    inode: u64,
}

#[derive(Clone, Debug)]
struct Adapter {
    key: GpuIdentityKey,
    card: String,
}

impl Adapter {
    fn at(root: &Path, card: String) -> io::Result<Self> {
        let device = fs::canonicalize(root.join(&card).join("device"))?;
        let metadata = fs::metadata(&device)?;
        if !metadata.is_dir() {
            return Err(io::Error::other("DRM device is not a directory"));
        }
        Ok(Self {
            key: GpuIdentityKey {
                device,
                filesystem: metadata.dev(),
                inode: metadata.ino(),
            },
            card,
        })
    }
}

#[derive(Clone, Debug, Default)]
enum Presence {
    #[default]
    Unknown,
    Absent,
    Present(Adapter),
}

/// A direct kernel busy percentage or RC6 awake-time estimate, including zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GpuBusy(u8);

impl GpuBusy {
    fn parse(text: &str) -> Option<Self> {
        let value = u8::try_from(decimal(text)?).ok()?;
        (value <= 100).then_some(Self(value))
    }

    pub(crate) fn percent(self) -> i32 {
        i32::from(self.0)
    }
}

/// Positive integer MHz for clock metadata, with current no greater than maximum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrequencyProxy {
    current_mhz: u64,
    maximum_mhz: u64,
}

impl FrequencyProxy {
    fn parse(current: &str, maximum: &str) -> Option<Self> {
        let current_mhz = decimal(current)?;
        let maximum_mhz = decimal(maximum)?;
        if current_mhz == 0 || maximum_mhz == 0 || current_mhz > maximum_mhz {
            return None;
        }
        Some(Self {
            current_mhz,
            maximum_mhz,
        })
    }

    /// The clock against its maximum: "1450 of 1550 MHz".
    pub(crate) fn text(self) -> String {
        crate::i18n::message(
            "tm-of-mhz-c34bb9",
            &[
                ("arg0", crate::i18n::argument(&self.current_mhz)),
                ("arg1", crate::i18n::argument(&self.maximum_mhz)),
            ],
        )
    }

    /// The current clock as a headline: "1.45 GHz", or "850 MHz".
    pub(crate) fn clock(self) -> String {
        if self.current_mhz >= 1000 {
            format!("{:.2} GHz", self.current_mhz as f64 / 1000.0)
        } else {
            format!("{} MHz", self.current_mhz)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GpuMemory {
    total: Option<u64>,
    used: Option<u64>,
}

impl GpuMemory {
    fn parse(total: Option<&str>, used: Option<&str>) -> Self {
        let total = total.and_then(decimal).filter(|total| *total > 0);
        let used = total.and_then(|total| used.and_then(decimal).filter(|used| *used <= total));
        Self { total, used }
    }

    fn text(self) -> String {
        match (self.total, self.used) {
            (Some(total), Some(used)) => {
                format!("{} / {}", format_bytes(used), format_bytes(total))
            }
            (Some(total), None) => crate::i18n::message(
                "tm-used-unavailable-total-4ca19c",
                &[(
                    "arg0",
                    crate::i18n::argument(&crate::i18n::reading(&format_bytes(total))),
                )],
            ),
            _ => "Unavailable".to_owned(),
        }
    }
}

/// A selected adapter's hwmon channel in millidegrees Celsius. The display
/// accepts -40..=150°C as a plausibility envelope, not a hardware safety limit.
#[derive(Clone, Debug)]
pub(crate) struct GpuTemperature {
    milli_celsius: i32,
    sensor: String,
}

impl GpuTemperature {
    fn parse(text: &str, sensor: String) -> Option<Self> {
        let text = text.trim();
        let digits = text.strip_prefix('-').unwrap_or(text);
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let milli_celsius = text.parse().ok()?;
        (-40_000..=150_000)
            .contains(&milli_celsius)
            .then_some(Self {
                milli_celsius,
                sensor,
            })
    }

    pub(crate) fn text(&self) -> String {
        crate::i18n::message(
            "tm-1-c-2152e5",
            &[
                (
                    "arg0",
                    crate::i18n::argument(&crate::i18n::number_text(format!(
                        "{:.1}",
                        f64::from(self.milli_celsius) / 1000.0
                    ))),
                ),
                ("arg1", crate::i18n::argument(&self.sensor)),
            ],
        )
    }
}

/// One selected DRM adapter's live data. Each telemetry family may be unknown
/// independently; only `busy` can feed a utilization history or sampled meter.
#[derive(Clone, Debug, Default)]
pub(crate) struct GpuReading {
    presence: Presence,
    adapter_name: Option<String>,
    busy: Option<GpuBusy>,
    frequency: Option<FrequencyProxy>,
    memory: GpuMemory,
    temperature: Option<GpuTemperature>,
}

impl GpuReading {
    pub(crate) fn identity_key(&self) -> Option<&GpuIdentityKey> {
        match &self.presence {
            Presence::Present(adapter) => Some(&adapter.key),
            _ => None,
        }
    }
    pub(crate) fn busy(&self) -> Option<GpuBusy> {
        self.busy
    }

    pub(crate) fn utilization_text(&self) -> String {
        match (&self.presence, self.busy) {
            (Presence::Present(_), Some(busy)) => format!("{}%", busy.percent()),
            (Presence::Present(_), None) => "Utilization unavailable".to_owned(),
            (Presence::Absent, _) => "No GPU detected".to_owned(),
            (Presence::Unknown, _) => "GPU unavailable".to_owned(),
        }
    }

    /// Returns utilization when available, then current clock, then availability status.
    pub(crate) fn headline(&self) -> String {
        if self.busy.is_none()
            && let Some(proxy) = self.frequency
        {
            return proxy.clock();
        }
        self.utilization_text()
    }

    pub(super) fn static_facts(&self) -> DeviceGpuFacts {
        DeviceGpuFacts {
            adapter: self.adapter_name.clone(),
            vram: self.memory.total.map(format_bytes),
        }
    }

    pub(crate) fn resource(&self) -> ResourceData {
        let adapter = self
            .adapter_name
            .clone()
            .unwrap_or_else(|| self.utilization_text());
        let card = match &self.presence {
            Presence::Present(adapter) => adapter.card.as_str(),
            _ => "Unavailable",
        };
        ResourceData {
            id: "gpu".to_owned(),
            name: "GPU".to_owned(),
            subtitle: if self.identity_key().is_some() {
                adapter.clone()
            } else {
                self.utilization_text()
            },
            value: self.headline(),
            detail: self.frequency.map(FrequencyProxy::text).unwrap_or_default(),
            percent: self.busy.map_or(0, GpuBusy::percent),
            color: (167, 105, 230),
            stat1_label: "Utilization".to_owned(),
            stat1_value: self.utilization_text(),
            stat2_label: "Adapter".to_owned(),
            stat2_value: adapter,
            stat3_label: "Temperature".to_owned(),
            stat3_value: self
                .temperature
                .as_ref()
                .map_or_else(|| "Unavailable".to_owned(), GpuTemperature::text),
            stat4_label: "VRAM".to_owned(),
            stat4_value: self.memory.text(),
            stat5_label: "Clock".to_owned(),
            stat5_value: self
                .frequency
                .map_or_else(|| "Unavailable".to_owned(), FrequencyProxy::text),
            stat6_label: "Device".to_owned(),
            stat6_value: card.to_owned(),
            ..ResourceData::default()
        }
    }
}

#[derive(Default)]
pub(super) struct GpuCollector {
    selected: Option<GpuIdentityKey>,
    rc6: Option<(GpuIdentityKey, u64, std::time::Instant)>,
}

// Reject short residency intervals to limit millisecond counter quantization.
const RC6_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

impl GpuCollector {
    // Select the lowest numeric card initially, retaining its device identity
    // across enumeration changes instead of following a newly inserted card.
    pub(super) fn collect_in(&mut self, root: &Path) -> GpuReading {
        self.collect_at(root, std::time::Instant::now())
    }

    pub(super) fn collect_at(&mut self, root: &Path, now: std::time::Instant) -> GpuReading {
        let Ok(cards) = adapters(root) else {
            return GpuReading::default();
        };
        let selected = cards
            .iter()
            .find(|adapter| Some(&adapter.key) == self.selected.as_ref())
            .or_else(|| cards.first());
        let Some(adapter) = selected else {
            self.selected = None;
            return GpuReading {
                presence: Presence::Absent,
                ..GpuReading::default()
            };
        };
        let device = &adapter.key.device;
        let read = |name: &str| fs::read_to_string(device.join(name)).ok();
        let vendor = read("vendor").and_then(|text| pci_id(&text));
        let product = read("device").and_then(|text| pci_id(&text));
        let name = match (vendor, product) {
            (Some(vendor), Some(product)) => {
                let maker = gpu_vendor_name(&format!("{vendor:04x}"));
                match pci_device_name(vendor, product) {
                    Some(model) => format!("{maker} {model}"),
                    None => format!("{maker} ({vendor:04X}:{product:04X})"),
                }
            }
            _ => "Graphics adapter".to_owned(),
        };
        // A missing direct percentage falls back to the fraction of time outside RC6.
        let rc6 = rc6_residency_ms(&root.join(&adapter.card));
        let busy = match read("gpu_busy_percent").as_deref().and_then(GpuBusy::parse) {
            Some(busy) => Some(busy),
            None => self.rc6_busy(&adapter.key, rc6, now),
        };
        let frequency_path = device.join("drm").join(&adapter.card);
        let current = read_frequency(&frequency_path, "gt_cur_freq_mhz", "gt_act_freq_mhz");
        let maximum = read_frequency(&frequency_path, "gt_max_freq_mhz", "gt_RP0_freq_mhz");
        let frequency = current
            .as_deref()
            .zip(maximum.as_deref())
            .and_then(|(current, maximum)| FrequencyProxy::parse(current, maximum));
        let memory = GpuMemory::parse(
            read("mem_info_vram_total").as_deref(),
            read("mem_info_vram_used").as_deref(),
        );
        let temperature = temperature(device);
        // Refuse telemetry if the class link or device inode changed during the read.
        if Adapter::at(root, adapter.card.clone())
            .ok()
            .as_ref()
            .map(|current| &current.key)
            != Some(&adapter.key)
        {
            return GpuReading::default();
        }
        self.selected = Some(adapter.key.clone());
        GpuReading {
            presence: Presence::Present(adapter.clone()),
            adapter_name: Some(name),
            busy,
            frequency,
            memory,
            temperature,
        }
    }
}

impl GpuCollector {
    fn rc6_busy(
        &mut self,
        key: &GpuIdentityKey,
        residency: Option<u64>,
        now: std::time::Instant,
    ) -> Option<GpuBusy> {
        let Some(residency) = residency else {
            self.rc6 = None;
            return None;
        };
        let (previous_key, previous, at) = match self.rc6.take() {
            Some(baseline) if baseline.0 == *key => baseline,
            _ => {
                self.rc6 = Some((key.clone(), residency, now));
                return None;
            }
        };
        let elapsed = now.saturating_duration_since(at);
        if elapsed < RC6_MIN_INTERVAL {
            // Keep measuring from the older baseline.
            self.rc6 = Some((previous_key, previous, at));
            return None;
        }
        self.rc6 = Some((key.clone(), residency, now));
        let asleep = residency.checked_sub(previous)?;
        let elapsed_ms = elapsed.as_millis().max(1) as u64;
        let awake = elapsed_ms.saturating_sub(asleep.min(elapsed_ms));
        let percent = ((awake * 100 + elapsed_ms / 2) / elapsed_ms).min(100);
        Some(GpuBusy(percent as u8))
    }
}

// Prefer GT0 residency, retaining the older card power path as a fallback.
fn rc6_residency_ms(card: &Path) -> Option<u64> {
    ["gt/gt0/rc6_residency_ms", "power/rc6_residency_ms"]
        .iter()
        .find_map(|file| fs::read_to_string(card.join(file)).ok())
        .and_then(|text| decimal(&text))
}

fn adapters(root: &Path) -> io::Result<Vec<Adapter>> {
    let mut cards = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Some(index) = numbered_entry(name, "card") {
            cards.push((index, Adapter::at(root, name.to_owned())?));
        }
    }
    cards.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.card.cmp(&right.1.card))
    });
    Ok(cards.into_iter().map(|(_, adapter)| adapter).collect())
}

fn numbered_entry(name: &str, prefix: &str) -> Option<u64> {
    let suffix = name.strip_prefix(prefix)?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    suffix.parse().ok()
}

fn decimal(text: &str) -> Option<u64> {
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn pci_id(text: &str) -> Option<u16> {
    let text = text.trim();
    let text = text.strip_prefix("0x").unwrap_or(text);
    if text.len() != 4 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(text, 16).ok()
}

fn read_frequency(path: &Path, primary: &str, fallback: &str) -> Option<String> {
    match fs::read_to_string(path.join(primary)) {
        Ok(value) => Some(value),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::read_to_string(path.join(fallback)).ok()
        }
        Err(_) => None,
    }
}

fn temperature(device: &Path) -> Option<GpuTemperature> {
    let entries = fs::read_dir(device.join("hwmon")).ok()?;
    let mut channels = Vec::new();
    for entry in entries {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Some(number) = numbered_entry(name, "hwmon") {
            channels.push((number, name.to_owned(), entry.path()));
        }
    }
    channels.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    // Select the first plausible channel in numeric hwmon/sensor order, and name
    // that channel explicitly rather than implying an aggregate GPU temperature.
    for (_, hwmon, path) in channels {
        for sensor in 1..=6 {
            let raw = fs::read_to_string(path.join(format!("temp{sensor}_input"))).ok();
            if let Some(value) =
                raw.and_then(|raw| GpuTemperature::parse(&raw, format!("{hwmon}/temp{sensor}")))
            {
                return Some(value);
            }
        }
    }
    None
}

fn pci_device_name(vendor: u16, product: u16) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type Names = HashMap<(u16, u16), Option<String>>;
    static NAMES: OnceLock<Mutex<Names>> = OnceLock::new();
    let names = NAMES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(name) = names.lock().ok()?.get(&(vendor, product)) {
        return name.clone();
    }
    let found = ["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids"]
        .iter()
        .find_map(|path| {
            let text = fs::File::open(path).ok()?;
            lookup_pci_name(std::io::BufReader::new(text), vendor, product)
        });
    if let Ok(mut names) = names.lock() {
        names.insert((vendor, product), found.clone());
    }
    found
}

fn lookup_pci_name(ids: impl std::io::BufRead, vendor: u16, product: u16) -> Option<String> {
    let vendor_tag = format!("{vendor:04x}");
    let product_tag = format!("\t{product:04x}");
    let mut in_vendor = false;
    for line in ids.lines() {
        let line = line.ok()?;
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if !line.starts_with('\t') {
            if in_vendor {
                return None;
            }
            in_vendor = line.starts_with(&vendor_tag);
            continue;
        }
        if in_vendor && line.starts_with(&product_tag) && !line.starts_with("\t\t") {
            let name = line.strip_prefix(&product_tag)?.trim();
            let bracketed = name
                .rfind('[')
                .zip(name.rfind(']'))
                .filter(|(open, close)| open < close)
                .and_then(|(open, close)| name.get(open + 1..close).map(str::trim));
            return Some(bracketed.unwrap_or(name).to_owned());
        }
    }
    None
}
