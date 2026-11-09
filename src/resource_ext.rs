//! Resource statistics, device facts and accents for the view.

use iced::Color;

use crate::device_facts::DeviceFacts;
use crate::services::ResourceData;

/// A labelled statistic for the Performance page stats grid.
///
/// Additional readings share the label and optional Lucide icon. An unmapped
/// icon is absent rather than reserving an empty slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stat {
    pub(crate) label: String,
    pub(crate) value: String,
    pub(crate) additional_values: Vec<String>,
    pub(crate) icon: Option<String>,
}

pub(crate) trait ResourceView {
    /// Returns slots with a nonempty label or value, preserving slot order.
    fn stats(&self) -> Vec<Stat>;
    /// The resource's accent colour as an iced `Color`.
    fn accent(&self) -> Color;
}

impl ResourceView for ResourceData {
    fn stats(&self) -> Vec<Stat> {
        let pairs = [
            (&self.stat1_label, &self.stat1_value),
            (&self.stat2_label, &self.stat2_value),
            (&self.stat3_label, &self.stat3_value),
            (&self.stat4_label, &self.stat4_value),
            (&self.stat5_label, &self.stat5_value),
            (&self.stat6_label, &self.stat6_value),
        ];
        pairs
            .into_iter()
            .filter(|(label, value)| !label.is_empty() || !value.is_empty())
            .map(|(label, value)| Stat {
                label: label.clone(),
                value: value.clone(),
                additional_values: Vec::new(),
                icon: icon_for_label(label),
            })
            .collect()
    }

    fn accent(&self) -> Color {
        // Per-resource accents come from resource data rather than theme roles.
        let (r, g, b) = self.color;
        Color::from_rgb8(r, g, b)
    }
}

/// Adds available device facts, using live metadata for disk and GPU resources.
///
/// Startup facts equal to the name, subtitle or sampler values are omitted
/// after trimming and case folding; network facts require a matching interface.
pub(crate) fn device_stats(facts: &DeviceFacts, resource: &ResourceData) -> Vec<Stat> {
    if resource.id == "disk" {
        // Disk facts are live mountinfo metadata, not unkeyed startup facts for
        // a root device that may have disappeared or changed.
        return if resource.detail.is_empty() {
            Vec::new()
        } else {
            vec![Stat {
                label: "Filesystem".to_owned(),
                value: resource.detail.clone(),
                additional_values: Vec::new(),
                icon: icon_for_label("Filesystem"),
            }]
        };
    }
    if resource.id == "gpu" {
        // Unkeyed startup facts cannot override the selected adapter's live data.
        return if resource.detail.is_empty() {
            Vec::new()
        } else {
            vec![Stat {
                label: "Frequency proxy".to_owned(),
                value: resource.detail.clone(),
                additional_values: Vec::new(),
                icon: icon_for_label("Utilization"),
            }]
        };
    }
    let already_shown: Vec<String> = [resource.name.clone(), resource.subtitle.clone()]
        .into_iter()
        .chain(resource.stats().into_iter().map(|stat| stat.value))
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
        .collect();
    let pairs: Vec<(&str, &Option<String>)> = match resource.id.as_str() {
        "cpu" => vec![
            ("CPU model", &facts.cpu_model),
            ("CPU topology", &facts.cpu_topology),
        ],
        // Live memory slots own these capacities, including their unknown state;
        // one-shot facts must not restore a stale total after a failed reading.
        "memory" => Vec::new(),
        id if id.starts_with("network")
            && facts.network_interface.as_deref() == Some(resource.name.as_str()) =>
        {
            vec![
                ("Network interface", &facts.network_interface),
                ("Interface state", &facts.network_state),
            ]
        }
        _ => Vec::new(),
    };
    let mut stats: Vec<Stat> = pairs
        .into_iter()
        .filter_map(|(label, value)| {
            value.as_ref().map(|value| Stat {
                label: label.to_owned(),
                value: value.clone(),
                additional_values: Vec::new(),
                icon: icon_for_label(label),
            })
        })
        .filter(|stat| !already_shown.contains(&stat.value.trim().to_lowercase()))
        .collect();
    if resource.id == "cpu" && resource.stat6_label == "Temperature" && !resource.detail.is_empty()
    {
        stats.push(Stat {
            label: "Temperature sensor".to_owned(),
            value: resource.detail.clone(),
            additional_values: Vec::new(),
            icon: icon_for_label("Temperature"),
        });
    }
    if resource.id.starts_with("network") && !resource.detail.is_empty() {
        stats.push(Stat {
            label: "Link capacity".to_owned(),
            value: resource.detail.clone(),
            additional_values: Vec::new(),
            icon: icon_for_label("Link"),
        });
    }
    if resource.id.starts_with("network")
        && facts.network_interface.as_deref() == Some(resource.name.as_str())
    {
        let mut addresses = facts
            .network_ips
            .iter()
            .filter(|address| !already_shown.contains(&address.trim().to_lowercase()));
        if let Some(first) = addresses.next() {
            stats.push(Stat {
                label: "IP address".to_owned(),
                value: first.clone(),
                additional_values: addresses.cloned().collect(),
                icon: icon_for_label("IP address"),
            });
        }
    }
    stats
}

/// Maps an exact statistic label to a Lucide glyph, returning `None` for unknown labels.
pub(crate) fn icon_for_label(label: &str) -> Option<String> {
    let name = match label {
        "Utilization" | "Use" => "gauge",
        "Logical CPUs" => "cpu",
        "Load average" => "activity",
        "Uptime" => "clock",
        "Processes" => "layers",
        "Architecture" => "microchip",
        "Temperature" | "CPU temperature" | "GPU temperature" => "thermometer",
        "CPU model" | "CPU topology" => "cpu",
        "Used" => "chart-column",
        "Available" | "Swap Free" => "box",
        "Total" | "Size" | "Swap Total" | "Memory total" | "Swap total" => "database",
        "Swap Used" => "layers",
        "Mount" | "Mount point" => "folder",
        "Device" | "Disk model" | "Filesystem" => "hard-drive",
        "Adapter" | "GPU adapter" => "monitor",
        "VRAM" | "GPU VRAM" => "memory-stick",
        "Source" => "tag",
        "Interface" | "Network interface" | "IP address" => "network",
        "Interface state" => "signal",
        "Received" | "RX rate" => "arrow-down",
        "Sent" | "TX rate" => "arrow-up",
        "Link" => "signal",
        "Status" => "activity",
        _ => return None,
    };
    Some(name.to_owned())
}
