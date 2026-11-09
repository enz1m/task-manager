use super::{ResourceData, format_bytes, format_disk_rate};
use std::time::{Duration, Instant};

const SECTOR_BYTES: u128 = 512;
const KIB_BYTES: u128 = 1024;
const NANOS_PER_SECOND: u128 = 1_000_000_000;

fn rounded_ratio(numerator: u128, denominator: u128) -> Option<u128> {
    numerator
        .checked_add(denominator / 2)?
        .checked_div(denominator)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiskIdentity {
    mount_id: u64,
    major_minor: (u32, u32),
    device: String,
    filesystem: String,
}

#[derive(Clone, Copy, Debug)]
struct DiskCounters {
    sectors_read: u64,
    sectors_written: u64,
    io_ticks_ms: u64,
}

/// A measured interval. Kernel sectors are 512 bytes; each KiB/s rate and
/// utilization percentage rounds half up. The headline sums the rounded rates.
/// Rates and their sum must fit the existing nonnegative i32 history channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DiskActivity {
    read: i32,
    write: i32,
    total: i32,
    utilization: i32,
}

impl DiskActivity {
    fn between(previous: DiskCounters, current: DiskCounters, elapsed: Duration) -> Option<Self> {
        let nanos = elapsed.as_nanos();
        if nanos == 0 {
            return None;
        }
        let rate = |delta: u64| {
            let numerator = u128::from(delta)
                .checked_mul(SECTOR_BYTES)?
                .checked_mul(NANOS_PER_SECOND)?;
            let denominator = nanos.checked_mul(KIB_BYTES)?;
            i32::try_from(rounded_ratio(numerator, denominator)?).ok()
        };
        let read = rate(current.sectors_read.checked_sub(previous.sectors_read)?)?;
        let write = rate(
            current
                .sectors_written
                .checked_sub(previous.sectors_written)?,
        )?;
        let total = read.checked_add(write)?;
        let busy_nanos = u128::from(current.io_ticks_ms.checked_sub(previous.io_ticks_ms)?)
            .checked_mul(1_000_000)?;
        let utilization =
            i32::try_from(rounded_ratio(busy_nanos.checked_mul(100)?, nanos)?).ok()?;
        if utilization > 100 {
            return None;
        }
        Some(Self {
            read,
            write,
            total,
            utilization,
        })
    }

    pub(crate) fn history_values(self) -> (i32, i32) {
        (self.read, self.write)
    }

    pub(crate) fn utilization_percent(self) -> i32 {
        self.utilization
    }

    pub(crate) fn read_kib_per_sec(self) -> u64 {
        self.read as u64
    }

    pub(crate) fn write_kib_per_sec(self) -> u64 {
        self.write as u64
    }

    pub(crate) fn total_kib_per_sec(self) -> u64 {
        self.total as u64
    }
}

/// Root filesystem capacity in bytes. Used is total minus all free blocks;
/// Available is caller-available space, which may exclude reserved blocks.
/// Percentage is Used / Total, not Used / (Used + Available), rounded half up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DiskCapacity {
    total: u64,
    used: u64,
    available: u64,
    used_percent: i32,
}

impl DiskCapacity {
    pub(crate) fn from_blocks(
        block_size: u64,
        blocks: u64,
        free: u64,
        available: u64,
    ) -> Option<Self> {
        if block_size == 0 || blocks == 0 || available > free {
            return None;
        }
        let used_blocks = blocks.checked_sub(free)?;
        let total = blocks.checked_mul(block_size)?;
        let used = used_blocks.checked_mul(block_size)?;
        let available = available.checked_mul(block_size)?;
        let used_percent = i32::try_from(rounded_ratio(
            u128::from(used).checked_mul(100)?,
            u128::from(total),
        )?)
        .ok()?;
        Some(Self {
            total,
            used,
            available,
            used_percent,
        })
    }

    #[cfg(test)]
    pub(crate) fn used_percent(self) -> i32 {
        self.used_percent
    }
}

/// Current root identity, activity and capacity have independent validity.
/// Missing activity never borrows capacity or a previous interval as its value.
#[derive(Clone, Debug, Default)]
pub(crate) struct DiskReading {
    identity: Option<DiskIdentity>,
    activity: Option<DiskActivity>,
    capacity: Option<DiskCapacity>,
}

impl DiskReading {
    pub(crate) fn activity(&self) -> Option<DiskActivity> {
        self.activity
    }
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> Option<DiskCapacity> {
        self.capacity
    }
    pub(crate) fn device(&self) -> Option<&str> {
        self.identity
            .as_ref()
            .map(|identity| identity.device.as_str())
    }
    pub(crate) fn mount(&self) -> Option<&str> {
        self.identity.as_ref().map(|_| "/")
    }
    pub(crate) fn filesystem(&self) -> Option<&str> {
        self.identity
            .as_ref()
            .map(|identity| identity.filesystem.as_str())
    }

    pub(crate) fn with_capacity(mut self, capacity: Option<DiskCapacity>) -> Self {
        self.capacity = capacity;
        self
    }

    pub(crate) fn resource(&self) -> ResourceData {
        let rate = |component: fn(DiskActivity) -> u64| {
            self.activity.map_or_else(
                || "Unavailable".to_owned(),
                |activity| format_disk_rate(component(activity)),
            )
        };
        ResourceData {
            id: "disk".to_owned(),
            name: "Disk".to_owned(),
            subtitle: self.device().unwrap_or("Unavailable").to_owned(),
            value: rate(DiskActivity::total_kib_per_sec),
            detail: self.filesystem().unwrap_or_default().to_owned(),
            percent: self.activity.map_or(0, DiskActivity::utilization_percent),
            color: (139, 176, 55),
            stat1_label: "Read rate".to_owned(),
            stat1_value: rate(DiskActivity::read_kib_per_sec),
            stat2_label: "Write rate".to_owned(),
            stat2_value: rate(DiskActivity::write_kib_per_sec),
            stat3_label: "Utilization".to_owned(),
            stat3_value: self.activity.map_or_else(
                || "Unavailable".to_owned(),
                |activity| format!("{}%", activity.utilization),
            ),
            stat4_label: "Used".to_owned(),
            stat4_value: self.capacity.map_or_else(
                || "Unavailable".to_owned(),
                |capacity| {
                    format!(
                        "{} / {} ({}%)",
                        format_bytes(capacity.used),
                        format_bytes(capacity.total),
                        capacity.used_percent
                    )
                },
            ),
            stat5_label: "Available".to_owned(),
            stat5_value: self.capacity.map_or_else(
                || "Unavailable".to_owned(),
                |capacity| format_bytes(capacity.available),
            ),
            stat6_label: "Mount".to_owned(),
            stat6_value: self.mount().unwrap_or("Unavailable").to_owned(),
            ..ResourceData::default()
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct DiskIoCollector {
    previous: Option<(DiskIdentity, DiskCounters, Instant)>,
}

impl DiskIoCollector {
    pub(crate) fn sample(
        &mut self,
        mountinfo: Option<&str>,
        diskstats: Option<&str>,
        captured_at: Instant,
        resolve_device: impl FnOnce(&str) -> Option<(u32, u32)>,
    ) -> DiskReading {
        let Some(identity) = mountinfo.and_then(|text| root_identity(text, resolve_device)) else {
            self.previous = None;
            return DiskReading::default();
        };
        let Some(current) = diskstats
            .filter(|_| identity.major_minor.0 != 0)
            .and_then(|text| counters(text, identity.major_minor))
        else {
            self.previous = None;
            return DiskReading {
                identity: Some(identity),
                ..DiskReading::default()
            };
        };
        let activity = self
            .previous
            .as_ref()
            .filter(|(previous, _, _)| previous == &identity)
            .and_then(|(_, previous, at)| {
                DiskActivity::between(*previous, current, captured_at.checked_duration_since(*at)?)
            });
        // Parsed current counters can seed the next interval, but an invalid
        // interval is never published. Missing input discards the baseline above.
        self.previous = Some((identity.clone(), current, captured_at));
        DiskReading {
            identity: Some(identity),
            activity,
            capacity: None,
        }
    }
}

fn root_identity(
    text: &str,
    resolve_device: impl FnOnce(&str) -> Option<(u32, u32)>,
) -> Option<DiskIdentity> {
    let mut roots = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .filter(|fields| fields.get(4).copied() == Some("/"));
    let fields = roots.next()?;
    if roots.next().is_some() {
        return None;
    }
    let mount_id = decimal::<u64>(fields.first()?)?;
    let parent_id = decimal::<u64>(fields.get(1)?)?;
    if mount_id == 0 || parent_id == 0 {
        return None;
    }
    let mut major_minor = device_numbers(fields.get(2)?)?;
    if !decode_mount_field(fields.get(3)?)?.starts_with('/') {
        return None;
    }
    let separator = fields.iter().position(|field| *field == "-")?;
    if separator < 6 || fields.len() != separator.checked_add(4)? {
        return None;
    }
    let filesystem = fields.get(separator + 1)?.to_string();
    let device = decode_mount_field(fields.get(separator + 2)?)?;
    if filesystem == "-" || device.is_empty() || device == "-" {
        return None;
    }
    // Anonymous btrfs mount IDs need the block source's rdev for diskstats.
    // An unresolved source retains its valid mount identity, not invented I/O.
    if major_minor.0 == 0
        && device.starts_with("/dev/")
        && let Some(resolved) = resolve_device(&device)
    {
        major_minor = resolved;
    }
    Some(DiskIdentity {
        mount_id,
        major_minor,
        device,
        filesystem,
    })
}

fn device_numbers(text: &str) -> Option<(u32, u32)> {
    let (major, minor) = text.split_once(':')?;
    Some((decimal(major)?, decimal(minor)?))
}

fn decode_mount_field(text: &str) -> Option<String> {
    let mut decoded = String::new();
    let mut remaining = text;
    while let Some(index) = remaining.find('\\') {
        decoded.push_str(remaining.get(..index)?);
        let escape = remaining.get(index..index.checked_add(4)?)?;
        decoded.push(match escape {
            "\\040" => ' ',
            "\\011" => '\t',
            "\\012" => '\n',
            "\\134" => '\\',
            _ => return None,
        });
        remaining = remaining.get(index + 4..)?;
    }
    decoded.push_str(remaining);
    Some(decoded)
}

fn counters(text: &str, device: (u32, u32)) -> Option<DiskCounters> {
    let mut matching = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .filter(|fields| {
            fields.first().and_then(|value| decimal(value)) == Some(device.0)
                && fields.get(1).and_then(|value| decimal(value)) == Some(device.1)
        });
    let fields = matching.next()?;
    if matching.next().is_some() || fields.len() < 14 {
        return None;
    }
    for field in &fields[3..] {
        decimal::<u64>(field)?;
    }
    Some(DiskCounters {
        sectors_read: decimal(fields.get(5)?)?,
        sectors_written: decimal(fields.get(9)?)?,
        io_ticks_ms: decimal(fields.get(12)?)?,
    })
}

fn decimal<T: std::str::FromStr>(value: &str) -> Option<T> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}
