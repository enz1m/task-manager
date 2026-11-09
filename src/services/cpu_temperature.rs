use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const MAX_HWMON_CPU_TEMP_CHANNELS: u32 = 64;
// Same inclusive engineering plausibility envelope as GPU telemetry, not a
// processor safety limit. Zero and sub-zero measurements are not sentinels.
const CPU_PLAUSIBLE_MILLI_CELSIUS: std::ops::RangeInclusive<i32> = -40_000..=150_000;
const REVIEWED_CPU_ZONE_TYPES: &[&str] = &["x86_pkg_temp", "cpu-thermal", "cpu_thermal"];
const REVIEWED_CPU_HWMON_CHIPS: &[&str] = &["coretemp", "k10temp", "zenpower", "cpu_thermal"];

// Tdie precedes Tctl because Tctl may include a control offset. The selected
// channel is named in the UI; neither is silently relabelled as a package die.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PackageThenTdieThenTctlThenZoneThenCoreThenUnlabelled {
    Package,
    Tdie,
    Tctl,
    PackageZone,
    Core,
    UnlabelledCpuChannel,
}
use PackageThenTdieThenTctlThenZoneThenCoreThenUnlabelled as Preference;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CpuTemperatureSource {
    Hwmon,
    ThermalZone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CpuTemperatureSensor {
    pub(crate) source: CpuTemperatureSource,
    pub(crate) class_path: PathBuf,
    pub(crate) chip_or_zone: String,
    pub(crate) channel: String,
    pub(crate) label: Option<String>,
    input_path: PathBuf,
    input_device: u64,
    input_inode: u64,
}

impl CpuTemperatureSensor {
    fn capture(source: CpuTemperatureSource, class_path: &Path, channel: &str) -> Option<Self> {
        let input_path = fs::canonicalize(class_path.join(channel)).ok()?;
        let metadata = fs::metadata(&input_path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        let identity_file = match source {
            CpuTemperatureSource::Hwmon => "name",
            CpuTemperatureSource::ThermalZone => "type",
        };
        let chip_or_zone = fs::read_to_string(class_path.join(identity_file))
            .ok()?
            .trim()
            .to_owned();
        let label = if source == CpuTemperatureSource::Hwmon {
            let stem = channel.strip_suffix("_input")?;
            match fs::read_to_string(class_path.join(format!("{stem}_label"))) {
                Ok(label) => Some(label.trim().to_owned()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(_) => return None,
            }
        } else {
            None
        };
        Some(Self {
            source,
            class_path: class_path.to_owned(),
            chip_or_zone,
            channel: channel.to_owned(),
            label,
            input_path,
            input_device: metadata.dev(),
            input_inode: metadata.ino(),
        })
    }

    fn description(&self) -> String {
        let class = match self.source {
            CpuTemperatureSource::Hwmon => "hwmon",
            CpuTemperatureSource::ThermalZone => "thermal",
        };
        let entry = self
            .class_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let label = self.label.as_deref().unwrap_or("unlabelled");
        format!(
            "{class}/{entry} • {} • {} • {label}",
            self.chip_or_zone, self.channel
        )
    }
}

/// A fresh CPU temperature and its exact sensor identity, or unavailable.
/// Collection retains no previous value or temperature history. Signed integer
/// milli-Celsius remains authoritative; display rounds to tenths, ties away
/// from zero, within the inclusive -40..=150°C plausibility envelope.
#[derive(Clone, Debug, Default)]
pub(crate) struct CpuTemperatureReading {
    measurement: Option<(CpuTemperatureSensor, i32)>,
}

impl CpuTemperatureReading {
    pub(crate) fn sensor(&self) -> Option<&CpuTemperatureSensor> {
        self.measurement.as_ref().map(|(sensor, _)| sensor)
    }
    pub(crate) fn milli_celsius(&self) -> Option<i32> {
        self.measurement.as_ref().map(|(_, milli)| *milli)
    }
    pub(crate) fn sensor_text(&self) -> Option<String> {
        self.sensor().map(CpuTemperatureSensor::description)
    }

    pub(crate) fn value_text(&self) -> String {
        let Some(milli) = self.milli_celsius() else {
            return "Unavailable".to_owned();
        };
        let magnitude = milli.unsigned_abs();
        // The validated magnitude is at most 150000, so integer rounding cannot
        // overflow; no float or rounded value becomes the stored measurement.
        let tenths = magnitude / 100 + u32::from(magnitude % 100 >= 50);
        let sign = if milli < 0 { "-" } else { "" };
        format!("{sign}{}.{:01}°C", tenths / 10, tenths % 10)
    }

    pub(crate) fn collect_in(thermal_root: &Path, hwmon_root: &Path) -> Self {
        Self::collect_entries(entries(thermal_root), entries(hwmon_root))
    }

    pub(super) fn collect_entries(thermal: Vec<PathBuf>, hwmon: Vec<PathBuf>) -> Self {
        let mut candidates = Vec::new();
        for path in thermal {
            let Some(index) = entry_number(&path, "thermal_zone") else {
                continue;
            };
            if let Some(sensor) =
                CpuTemperatureSensor::capture(CpuTemperatureSource::ThermalZone, &path, "temp")
                && REVIEWED_CPU_ZONE_TYPES.contains(&sensor.chip_or_zone.as_str())
            {
                candidates.push((Preference::PackageZone, index, 0, sensor));
            }
        }
        for path in hwmon {
            let Some(index) = entry_number(&path, "hwmon") else {
                continue;
            };
            let chip = fs::read_to_string(path.join("name")).ok();
            if !chip
                .as_deref()
                .is_some_and(|chip| REVIEWED_CPU_HWMON_CHIPS.contains(&chip.trim()))
            {
                continue;
            }
            for channel in 1..=MAX_HWMON_CPU_TEMP_CHANNELS {
                if let Some(sensor) = CpuTemperatureSensor::capture(
                    CpuTemperatureSource::Hwmon,
                    &path,
                    &format!("temp{channel}_input"),
                ) && let Some(preference) = hwmon_preference(&sensor)
                {
                    candidates.push((preference, index, channel, sensor));
                }
            }
        }
        candidates.sort_by(|left, right| {
            (left.0, left.3.source, left.1, left.2, &left.3.class_path).cmp(&(
                right.0,
                right.3.source,
                right.1,
                right.2,
                &right.3.class_path,
            ))
        });
        // Rank identities before reading a value. An invalid preferred sample
        // is unavailable, not a silent substitution of a lower-priority sensor.
        let measurement = candidates.into_iter().next().and_then(|(_, _, _, sensor)| {
            let raw = fs::read_to_string(&sensor.input_path).ok()?;
            let milli = parse_milli(&raw)?;
            let current =
                CpuTemperatureSensor::capture(sensor.source, &sensor.class_path, &sensor.channel)?;
            (current == sensor).then_some((sensor, milli))
        });
        Self { measurement }
    }
}

fn hwmon_preference(sensor: &CpuTemperatureSensor) -> Option<Preference> {
    match (sensor.chip_or_zone.as_str(), sensor.label.as_deref()) {
        ("coretemp", Some(label)) if numbered_label(label, "Package id ") => {
            Some(Preference::Package)
        }
        ("coretemp", Some(label)) if numbered_label(label, "Core ") => Some(Preference::Core),
        ("k10temp" | "zenpower", Some("Tdie")) => Some(Preference::Tdie),
        ("k10temp" | "zenpower", Some("Tctl")) => Some(Preference::Tctl),
        (chip, None) if REVIEWED_CPU_HWMON_CHIPS.contains(&chip) => {
            Some(Preference::UnlabelledCpuChannel)
        }
        _ => None,
    }
}

fn numbered_label(label: &str, prefix: &str) -> bool {
    label.strip_prefix(prefix).and_then(decimal).is_some()
}
fn entry_number(path: &Path, prefix: &str) -> Option<u32> {
    path.file_name()?
        .to_str()?
        .strip_prefix(prefix)
        .and_then(decimal)
}
fn decimal(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}
fn parse_milli(text: &str) -> Option<i32> {
    let text = text.trim();
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let milli = text.parse::<i32>().ok()?;
    CPU_PLAUSIBLE_MILLI_CELSIUS
        .contains(&milli)
        .then_some(milli)
}
fn entries(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default()
}
