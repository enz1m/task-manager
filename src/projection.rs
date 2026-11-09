//! Filtering, sorting, and application grouping for cached table projections.

use std::collections::{BTreeSet, HashMap};

use crate::services::{ProcessData, ServiceData};
use crate::table::RowShape;

pub(crate) fn process_matches(p: &ProcessData, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    p.application_name
        .as_ref()
        .is_some_and(|name| enzui_core::palette::normalize(name).contains(query))
        || enzui_core::palette::normalize(&p.name).contains(query)
        || p.pid.contains(query)
        || enzui_core::palette::normalize(&p.user).contains(query)
}

pub(crate) fn service_matches(s: &ServiceData, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    enzui_core::palette::normalize(&s.name).contains(query)
        || enzui_core::palette::normalize(&s.description).contains(query)
        || enzui_core::palette::normalize(&s.state).contains(query)
}

pub(crate) fn sort_processes(items: &mut [ProcessData], key: &str, ascending: bool) {
    let compare_text = text_comparator();
    items.sort_by(|left, right| {
        let ordering = match key {
            "name" => compare_text(&left.name, &right.name),
            "pid" => numeric_text(&left.pid).total_cmp(&numeric_text(&right.pid)),
            "cpu" => numeric_text(&left.cpu).total_cmp(&numeric_text(&right.cpu)),
            "memory" | "rss" => memory_text(&left.rss).total_cmp(&memory_text(&right.rss)),
            "mem_percent" => numeric_text(&left.memory).total_cmp(&numeric_text(&right.memory)),
            "user" => compare_text(&left.user, &right.user),
            "state" => compare_text(&left.state, &right.state),
            _ => compare_text(&left.name, &right.name),
        };
        if ascending {
            ordering
        } else {
            ordering.reverse()
        }
    });
}

pub(crate) fn sort_services(items: &mut [ServiceData], key: &str, ascending: bool) {
    let compare_text = text_comparator();
    items.sort_by(|left, right| {
        let ordering = match key {
            "name" => compare_text(&left.name, &right.name),
            "load" => compare_text(&left.load, &right.load),
            "state" => compare_text(&left.state, &right.state),
            "sub_state" => compare_text(&left.sub_state, &right.sub_state),
            "description" => compare_text(&left.description, &right.description),
            _ => compare_text(&left.name, &right.name),
        };
        if ascending {
            ordering
        } else {
            ordering.reverse()
        }
    });
}

pub(crate) fn default_process_sort_ascending(key: &str) -> bool {
    !matches!(key, "pid" | "cpu" | "memory" | "rss" | "mem_percent")
}

pub(crate) fn default_service_sort_ascending(_key: &str) -> bool {
    true
}

// Group after filtering, so searches expand only matching members and totals reflect them.
pub(crate) fn group_applications(
    items: Vec<ProcessData>,
    expanded: &BTreeSet<String>,
    expand_all: bool,
    key: &str,
    ascending: bool,
) -> (Vec<ProcessData>, Vec<RowShape>) {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for item in &items {
        *counts
            .entry(item.application_name.as_deref().unwrap_or(&item.name))
            .or_default() += 1;
    }
    let grouped: BTreeSet<String> = counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name.to_owned())
        .collect();
    if grouped.is_empty() {
        let shapes = vec![RowShape::Process { member: false }; items.len()];
        return (items, shapes);
    }
    let mut members: HashMap<String, Vec<ProcessData>> = HashMap::new();
    let mut top = Vec::with_capacity(items.len());
    for item in items {
        let application = item.application_name.as_ref().unwrap_or(&item.name);
        if grouped.contains(application) {
            members.entry(application.clone()).or_default().push(item);
        } else {
            top.push(item);
        }
    }
    top.extend(members.values().map(|group| group_row(group)));
    sort_processes(&mut top, key, ascending);
    let mut rows = Vec::with_capacity(top.len());
    let mut shapes = Vec::with_capacity(top.len());
    for row in top {
        let group = if row.pid.is_empty() {
            members.remove(&row.name)
        } else {
            None
        };
        let Some(group) = group else {
            rows.push(row);
            shapes.push(RowShape::Process { member: false });
            continue;
        };
        let open = expand_all || expanded.contains(&row.name);
        shapes.push(RowShape::Group {
            name: row.name.clone(),
            members: group.clone(),
            expanded: open,
        });
        rows.push(row);
        if open {
            shapes.extend(std::iter::repeat_n(
                RowShape::Process { member: true },
                group.len(),
            ));
            rows.extend(group);
        }
    }
    (rows, shapes)
}

// Aggregate rows have no process identity and must never become action targets.
fn group_row(members: &[ProcessData]) -> ProcessData {
    let first = &members[0];
    let cpu_known: Vec<f64> = members
        .iter()
        .filter(|p| !matches!(p.cpu.trim(), "" | "—"))
        .map(|p| numeric_text(&p.cpu))
        .collect();
    let cpu = if cpu_known.is_empty() {
        "—".to_owned()
    } else {
        format!("{:.1}%", cpu_known.iter().sum::<f64>())
    };
    let exact_rss = members
        .iter()
        .try_fold(0u64, |total, p| total.checked_add(p.rss_kib?));
    let rss: f64 = members.iter().map(|p| memory_text(&p.rss)).sum();
    let memory: f64 = members.iter().map(|p| numeric_text(&p.memory)).sum();
    let user = if members.iter().all(|p| p.user == first.user) {
        first.user.clone()
    } else {
        "—".to_owned()
    };
    let state = members
        .iter()
        .find(|p| p.state.eq_ignore_ascii_case("running"))
        .unwrap_or(first)
        .state
        .clone();
    ProcessData {
        name: first
            .application_name
            .clone()
            .unwrap_or_else(|| first.name.clone()),
        action_allowed: members.iter().any(|p| p.action_allowed),
        cpu,
        memory: format!("{memory:.1}%"),
        rss: exact_rss.map_or_else(
            || {
                crate::i18n::message(
                    "tm-rss-1-mib-2c9d03",
                    &[(
                        "arg0",
                        crate::i18n::argument(&crate::i18n::number_text(format!("{rss:.1}"))),
                    )],
                )
            },
            crate::services::format_mib,
        ),
        user,
        state,
        kind: first.kind.clone(),
        ..ProcessData::default()
    }
}

fn text_comparator() -> impl Fn(&str, &str) -> std::cmp::Ordering {
    let collator = enzui_core::localization::current()
        .locale_for(enzui_core::localization::LocaleCategory::Collate)
        .collator(enzui_core::localization::Collation::NATURAL)
        .ok();
    move |left, right| {
        collator
            .as_ref()
            .map_or_else(|| left.cmp(right), |collator| collator.compare(left, right))
    }
}

fn numeric_text(value: &str) -> f64 {
    value
        .trim()
        .trim_end_matches('%')
        .parse::<f64>()
        .unwrap_or(0.0)
}

fn memory_text(value: &str) -> f64 {
    let mut parts = value.split_whitespace();
    let amount = parts
        .next()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0);
    match parts.next().unwrap_or_default() {
        "GiB" => amount * 1024.0,
        "MiB" => amount,
        "KiB" => amount / 1024.0,
        _ => amount,
    }
}
