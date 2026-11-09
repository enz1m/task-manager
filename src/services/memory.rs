use super::{ResourceData, format_bytes};

/// A consistent byte-capacity reading. Available is MemAvailable for RAM and
/// SwapFree for swap; zero capacity is valid only for a disabled swap pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MemoryUsage {
    total_bytes: u64,
    available_bytes: u64,
    used_bytes: u64,
    used_percent: i32,
}

impl MemoryUsage {
    fn from_bytes(total_bytes: u64, available_bytes: u64) -> Option<Self> {
        let used_bytes = total_bytes.checked_sub(available_bytes)?;
        let used_percent = if total_bytes == 0 {
            0
        } else {
            let percent = u128::from(used_bytes)
                .checked_mul(100)?
                .checked_add(u128::from(total_bytes) / 2)?
                .checked_div(u128::from(total_bytes))?;
            i32::try_from(percent).ok()?
        };
        Some(Self {
            total_bytes,
            available_bytes,
            used_bytes,
            used_percent,
        })
    }

    pub(crate) fn total_bytes(self) -> u64 {
        self.total_bytes
    }

    pub(crate) fn available_bytes(self) -> u64 {
        self.available_bytes
    }

    pub(crate) fn used_bytes(self) -> u64 {
        self.used_bytes
    }

    pub(crate) fn used_percent(self) -> i32 {
        self.used_percent
    }
}

/// Independent RAM and optional swap measurements from one meminfo read.
/// Missing or invalid fields invalidate their pair, not the other pair. RAM
/// requires positive total capacity; an explicit zero/zero swap pair is valid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MemoryReading {
    ram: Option<MemoryUsage>,
    swap: Option<MemoryUsage>,
}

impl MemoryReading {
    pub(crate) fn from_meminfo(text: Option<&str>) -> Self {
        let Some(text) = text else {
            return Self::default();
        };
        Self {
            ram: capacity(text, "MemTotal", "MemAvailable").filter(|usage| usage.total_bytes > 0),
            swap: capacity(text, "SwapTotal", "SwapFree"),
        }
    }

    pub(crate) fn ram(self) -> Option<MemoryUsage> {
        self.ram
    }

    #[cfg(test)]
    pub(crate) fn swap(self) -> Option<MemoryUsage> {
        self.swap
    }

    pub(crate) fn summary(self) -> String {
        self.ram.map_or_else(
            || "Memory unavailable".to_owned(),
            |usage| {
                format!(
                    "{} / {}",
                    format_bytes(usage.used_bytes),
                    format_bytes(usage.total_bytes)
                )
            },
        )
    }

    pub(crate) fn resource(self) -> ResourceData {
        let ram_value = |value: fn(MemoryUsage) -> u64| {
            self.ram.map_or_else(
                || "Unavailable".to_owned(),
                |usage| format_bytes(value(usage)),
            )
        };
        let swap_value = |value: fn(MemoryUsage) -> u64| {
            self.swap.map_or_else(
                || "Unavailable".to_owned(),
                |usage| format_bytes(value(usage)),
            )
        };
        ResourceData {
            id: "memory".to_owned(),
            name: "Memory".to_owned(),
            subtitle: self.summary(),
            value: self.ram.map_or_else(
                || "Unavailable".to_owned(),
                |usage| format!("{}%", usage.used_percent),
            ),
            detail: "System RAM".to_owned(),
            percent: self.ram.map_or(0, MemoryUsage::used_percent),
            color: (82, 143, 219),
            stat1_label: "Used".to_owned(),
            stat1_value: ram_value(MemoryUsage::used_bytes),
            stat2_label: "Available".to_owned(),
            stat2_value: ram_value(MemoryUsage::available_bytes),
            stat3_label: "Total".to_owned(),
            stat3_value: ram_value(MemoryUsage::total_bytes),
            stat4_label: "Swap Used".to_owned(),
            stat4_value: self.swap.map_or_else(
                || "Unavailable".to_owned(),
                |usage| {
                    format!(
                        "{} ({}%)",
                        format_bytes(usage.used_bytes),
                        usage.used_percent
                    )
                },
            ),
            stat5_label: "Swap Free".to_owned(),
            stat5_value: swap_value(MemoryUsage::available_bytes),
            stat6_label: "Swap Total".to_owned(),
            stat6_value: swap_value(MemoryUsage::total_bytes),
            ..ResourceData::default()
        }
    }
}

fn capacity(text: &str, total: &str, available: &str) -> Option<MemoryUsage> {
    MemoryUsage::from_bytes(field_bytes(text, total)?, field_bytes(text, available)?)
}

fn field_bytes(text: &str, key: &str) -> Option<u64> {
    let mut matching = text
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.trim() == key);
    let (_, value) = matching.next()?;
    if matching.next().is_some() {
        return None;
    }
    // Linux meminfo's kB unit is 1024 bytes, not a decimal kilobyte.
    parse_kib(value)?.checked_mul(1024)
}

pub(super) fn parse_kib(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let digits = parts.next()?;
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let kib = digits.parse::<u64>().ok()?;
    if parts.next() != Some("kB") || parts.next().is_some() {
        return None;
    }
    Some(kib)
}
