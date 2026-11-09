use super::{NetworkCounterBatch, NetworkCounterSample, ResourceData, format_bytes, format_rate};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NetworkIdentity {
    ifindex: u32,
    path: PathBuf,
    filesystem: u64,
    inode: u64,
}

impl NetworkIdentity {
    fn capture(path: &Path) -> Option<Self> {
        let path = fs::canonicalize(path).ok()?;
        let metadata = fs::metadata(&path).ok()?;
        if !metadata.is_dir() {
            return None;
        }
        let ifindex = u32::try_from(read_decimal(&path.join("ifindex"))?).ok()?;
        if ifindex == 0 {
            return None;
        }
        Some(Self {
            ifindex,
            path,
            filesystem: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

// Decimal Kbps uses 8 bits per byte and 1000 bits per kilobit, rounded half up.
// Displayed rates must fit nonnegative i32 history; the retained exact fraction
// prevents rounding an above-capacity interval down to an apparently valid 100%.
#[derive(Clone, Copy, Debug)]
struct Rate {
    kbps: i32,
    numerator: u128,
    denominator: u128,
}

impl Rate {
    fn from_delta(bytes: u64, elapsed: Duration) -> Option<Self> {
        let numerator = u128::from(bytes)
            .checked_mul(8)?
            .checked_mul(1_000_000_000)?;
        let denominator = elapsed.as_nanos().checked_mul(1_000)?;
        let kbps = i32::try_from(rounded_ratio(numerator, denominator)?).ok()?;
        Some(Self {
            kbps,
            numerator,
            denominator,
        })
    }
    fn combined(self, other: Self) -> Option<Self> {
        if self.denominator != other.denominator {
            return None;
        }
        let numerator = self.numerator.checked_add(other.numerator)?;
        let kbps = i32::try_from(rounded_ratio(numerator, self.denominator)?).ok()?;
        Some(Self {
            kbps,
            numerator,
            denominator: self.denominator,
        })
    }
}
fn rounded_ratio(numerator: u128, denominator: u128) -> Option<u128> {
    numerator
        .checked_add(denominator / 2)?
        .checked_div(denominator)
}

#[derive(Clone, Copy, Debug)]
struct LinkSpeed {
    mbps: u64,
    kbps: u64,
}
impl LinkSpeed {
    fn from_mbps(mbps: u64) -> Option<Self> {
        // The all-ones ethtool speed sentinel is not a reported capacity.
        if mbps == u64::from(u32::MAX) {
            return None;
        }
        (mbps > 0).then_some(Self {
            mbps,
            kbps: mbps.checked_mul(1_000)?,
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum Capacity {
    Unavailable,
    AboveReportedSpeed,
    Measured(i32),
}
impl Capacity {
    fn from_combined(rate: Option<Rate>, speed: Option<LinkSpeed>) -> Self {
        let (Some(rate), Some(speed)) = (rate, speed) else {
            return Self::Unavailable;
        };
        let Some(denominator) = rate.denominator.checked_mul(u128::from(speed.kbps)) else {
            return Self::Unavailable;
        };
        // RX+TX may exceed a single reported speed on full-duplex links. Do not
        // infer duplex or clip to 100%; preserve rates and refuse this percentage.
        if rate.numerator > denominator || rate.kbps as u64 > speed.kbps {
            return Self::AboveReportedSpeed;
        }
        rate.numerator
            .checked_mul(100)
            .and_then(|numerator| rounded_ratio(numerator, denominator))
            .and_then(|percent| i32::try_from(percent).ok())
            .map_or(Self::Unavailable, Self::Measured)
    }
    fn percent(self) -> Option<i32> {
        match self {
            Self::Measured(percent) => Some(percent),
            _ => None,
        }
    }
}

/// Live data for one named interface incarnation. Missing/reset RX or TX
/// invalidates that direction only; RX+TX history/headline require both and a
/// representable total rounded once from their exact sum. Link-capacity validity
/// is separate.
#[derive(Clone, Debug)]
pub(crate) struct NetworkReading {
    pub(crate) name: String,
    identity: Option<NetworkIdentity>,
    received_bytes: Option<u64>,
    sent_bytes: Option<u64>,
    operstate: String,
    speed: Option<LinkSpeed>,
    rx: Option<Rate>,
    tx: Option<Rate>,
    combined: Option<Rate>,
    capacity: Capacity,
}
impl NetworkReading {
    #[cfg(test)]
    pub(crate) fn tx_rate_kbps(&self) -> Option<i32> {
        self.tx.map(|rate| rate.kbps)
    }

    #[cfg(test)]
    pub(crate) fn rx_rate_kbps(&self) -> Option<i32> {
        self.rx.map(|rate| rate.kbps)
    }

    pub(crate) fn resource_id(&self) -> String {
        format!("network_{}", self.name)
    }
    pub(crate) fn identity(&self) -> Option<&NetworkIdentity> {
        self.identity.as_ref()
    }

    pub(crate) fn combined_kbps(&self) -> Option<i32> {
        self.combined.map(|rate| rate.kbps)
    }
    pub(crate) fn capacity_percent(&self) -> Option<i32> {
        self.capacity.percent()
    }

    fn capacity_text(&self) -> String {
        let Some(speed) = self.speed else {
            return crate::i18n::display("Capacity percentage unavailable: link speed unavailable");
        };
        match self.capacity {
            Capacity::Measured(percent) => crate::i18n::message(
                "tm-rx-tx-percent-of-reported-mbps-e6bfb9",
                &[
                    ("arg0", crate::i18n::argument(&percent)),
                    ("arg1", crate::i18n::argument(&speed.mbps)),
                ],
            ),
            Capacity::AboveReportedSpeed => crate::i18n::message(
                "tm-rx-tx-exceeds-reported-mbps-percentage-unavailable-d-ace4bc",
                &[("arg0", crate::i18n::argument(&speed.mbps))],
            ),
            Capacity::Unavailable => crate::i18n::message(
                "tm-reported-mbps-rx-tx-percentage-unavailable-465ef0",
                &[("arg0", crate::i18n::argument(&speed.mbps))],
            ),
        }
    }

    pub(crate) fn resource(&self) -> ResourceData {
        let rate_text = |rate: Option<Rate>| {
            rate.map_or_else(
                || "Unavailable".to_owned(),
                |rate| format_rate(rate.kbps as u64),
            )
        };
        let total_text = |total: Option<u64>| {
            total.map_or_else(
                || "Unavailable".to_owned(),
                |bytes| format!("{bytes} bytes ({})", format_bytes(bytes)),
            )
        };
        ResourceData {
            id: self.resource_id(),
            name: self.name.clone(),
            subtitle: format!("Network • {}", self.operstate),
            value: rate_text(self.combined),
            detail: self.capacity_text(),
            percent: self.capacity_percent().unwrap_or(0),
            color: (219, 85, 145),
            stat1_label: "Interface".to_owned(),
            stat1_value: self.name.clone(),
            stat2_label: "Received".to_owned(),
            stat2_value: total_text(self.received_bytes),
            stat3_label: "Sent".to_owned(),
            stat3_value: total_text(self.sent_bytes),
            stat4_label: "Link".to_owned(),
            stat4_value: self.operstate.clone(),
            stat5_label: "RX rate".to_owned(),
            stat5_value: rate_text(self.rx),
            stat6_label: "TX rate".to_owned(),
            stat6_value: rate_text(self.tx),
            ..ResourceData::default()
        }
    }

    pub(super) fn unavailable(name: String) -> Self {
        Self {
            name,
            identity: None,
            received_bytes: None,
            sent_bytes: None,
            operstate: "unknown".to_owned(),
            speed: None,
            rx: None,
            tx: None,
            combined: None,
            capacity: Capacity::Unavailable,
        }
    }

    pub(super) fn no_adapter() -> Self {
        Self {
            operstate: "down".to_owned(),
            ..Self::unavailable("No active adapter".to_owned())
        }
    }
}

pub(super) fn readings_from_samples(
    previous: &[NetworkCounterSample],
    current: &[NetworkCounterSample],
    elapsed: Duration,
) -> Vec<NetworkReading> {
    let previous: HashMap<_, _> = previous
        .iter()
        .map(|sample| (sample.name.as_str(), sample))
        .collect();
    current
        .iter()
        .map(|sample| {
            let old = previous
                .get(sample.name.as_str())
                .filter(|old| sample.identity.is_some() && old.identity == sample.identity);
            let interval = |current: Option<u64>, previous: Option<u64>| {
                Rate::from_delta(current?.checked_sub(previous?)?, elapsed)
            };
            let rx = old.and_then(|old| interval(sample.rx_bytes, old.rx_bytes));
            let tx = old.and_then(|old| interval(sample.tx_bytes, old.tx_bytes));
            let combined = rx.zip(tx).and_then(|(rx, tx)| rx.combined(tx));
            let speed = sample.speed_mbps.and_then(LinkSpeed::from_mbps);
            NetworkReading {
                name: sample.name.clone(),
                identity: sample.identity.clone(),
                received_bytes: sample.rx_bytes,
                sent_bytes: sample.tx_bytes,
                operstate: sample.operstate.clone(),
                speed,
                rx,
                tx,
                combined,
                capacity: Capacity::from_combined(combined, speed),
            }
        })
        .collect()
}

pub(super) fn read_batch_in(root: &Path, now: impl FnOnce() -> Instant) -> NetworkCounterBatch {
    let mut interfaces = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        let mut names: Vec<_> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name != "lo")
            .collect();
        names.sort();
        for name in names {
            let base = root.join(&name);
            if !base.is_dir() {
                continue;
            }
            let identity = NetworkIdentity::capture(&base);
            let rx_bytes = read_decimal(&base.join("statistics/rx_bytes"));
            let tx_bytes = read_decimal(&base.join("statistics/tx_bytes"));
            let speed_mbps = read_decimal(&base.join("speed")).filter(|speed| *speed > 0);
            let operstate = fs::read_to_string(base.join("operstate"))
                .ok()
                .map(|state| state.trim().to_owned())
                .filter(|state| {
                    matches!(
                        state.as_str(),
                        "up" | "down"
                            | "unknown"
                            | "notpresent"
                            | "lowerlayerdown"
                            | "testing"
                            | "dormant"
                    )
                })
                .unwrap_or_else(|| "unknown".to_owned());
            let stable = identity.is_some() && NetworkIdentity::capture(&base) == identity;
            interfaces.push(if stable {
                NetworkCounterSample {
                    name,
                    identity,
                    rx_bytes,
                    tx_bytes,
                    speed_mbps,
                    operstate,
                }
            } else {
                NetworkCounterSample {
                    name,
                    identity: None,
                    rx_bytes: None,
                    tx_bytes: None,
                    speed_mbps: None,
                    operstate: "unknown".to_owned(),
                }
            });
        }
    }
    NetworkCounterBatch {
        captured_at: now(),
        interfaces,
    }
}
fn read_decimal(path: &Path) -> Option<u64> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}
