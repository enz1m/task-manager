//! Retained application state, snapshots, and table projections.
//!
//! Construction performs no procfs reads. Blocking owners publish generation-checked
//! snapshots; viewport handles receive table allocation feedback for scroll clamping.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use enzui_core::cancellation::{CancelCause, CancelToken};
use enzui_core::menu_model::{CloseCause, MenuModel, MenuState};
use enzui_widgets::enz_table::TableViewportState;
use enzui_widgets::toast::ToastTone;
use enzui_widgets::toast_host::{ToastExpirySchedule, ToastHost, ToastSpec, WaitingBudget};
use iced::{Point, Size};

use crate::device_facts::DeviceFacts;
use crate::layout;
use crate::projection;
use crate::services::{
    GraphPointData, ProcessData, ProcessTarget, ResourceData, ServiceData, Snapshot,
};
use crate::table::{Column, ProcessTable, ServiceTable, TableKind, visible_columns};

/// Navigation destinations and the legacy application route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    /// A resource Performance page (CPU/Memory/Disk/GPU/Network). The specific
    /// resource is carried by `Model::selected_resource_id`.
    Performance,
    /// Legacy application projection retained for compatibility.
    Apps,
    /// Application overview followed by all individual processes.
    Processes,
    /// systemd services browser.
    Services,
}

impl Page {
    /// Whether the route has a resource sidebar; only Performance does.
    pub(crate) fn has_sidebar(self) -> bool {
        matches!(self, Page::Performance)
    }

    /// Whether the titlebar can offer list search; Performance omits it.
    pub(crate) fn has_searchable_list(self) -> bool {
        !matches!(self, Page::Performance)
    }

    /// The titlebar search placeholder for this page.
    ///
    /// The placeholder is the field's accessible name, so it names only the
    /// fields this page filters on. Performance returns the process wording as
    /// an inert default.
    pub(crate) fn search_placeholder(self) -> &'static str {
        match self {
            Page::Services => crate::SERVICES_SEARCH_PLACEHOLDER,
            _ => crate::SEARCH_PLACEHOLDER,
        }
    }

    /// Collection domains needed by this page; telemetry is sampled on every route.
    pub(crate) fn sample_scope(self) -> crate::services::SampleScope {
        crate::services::SampleScope {
            processes: !matches!(self, Page::Services),
            services: matches!(self, Page::Services),
        }
    }

    /// The page's stable storage id.
    ///
    /// The id is persisted by `session::Session`, so these strings are a
    /// storage format and must not follow variant renames.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Page::Performance => "performance",
            Page::Apps => "apps",
            Page::Processes => "processes",
            Page::Services => "services",
        }
    }

    /// Parse a stored page id, migrating the legacy applications route to Processes.
    pub(crate) fn from_id(id: &str) -> Option<Page> {
        match id {
            "performance" => Some(Page::Performance),
            "apps" => Some(Page::Processes),
            "processes" => Some(Page::Processes),
            "services" => Some(Page::Services),
            _ => None,
        }
    }
}

/// Which table column is the active sort key, and its direction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SortState {
    pub(crate) key: String,
    pub(crate) ascending: bool,
}

/// Additional process-list filter selected from the page's Filter popup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ProcessFilter {
    #[default]
    All,
    Running,
    Sleeping,
}

/// Additional service-list filter selected from the page's Filter popup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ServiceFilter {
    #[default]
    All,
    Running,
    Stopped,
    Failed,
}

use crate::filter_menus::{process_filter_menu, service_filter_menu};

/// Presentation keys owned by the shared dialog stack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DialogKey {
    Logs,
    Shortcuts,
}

/// Row identity retained while its context menu is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MenuTarget {
    /// A process row, by stable runtime identity.
    Process(ProcessTarget),
    /// A service row, by unit name.
    Service(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PendingAction {
    /// SIGTERM this exact process instance.
    EndProcess {
        pid: String,
        start_time_ticks: u64,
        kernel_uid: Option<u32>,
        name: String,
    },
    /// SIGKILL this exact process instance.
    KillProcess {
        pid: String,
        start_time_ticks: u64,
        kernel_uid: Option<u32>,
        name: String,
    },
    /// `systemctl stop` this unit.
    StopService { unit: String },
    Service {
        unit: String,
        verb: crate::service_actions::Verb,
    },
}

impl PendingAction {
    pub(crate) fn process_target(&self) -> Option<ProcessTarget> {
        match self {
            Self::EndProcess {
                pid,
                start_time_ticks,
                kernel_uid,
                ..
            }
            | Self::KillProcess {
                pid,
                start_time_ticks,
                kernel_uid,
                ..
            } => Some(ProcessTarget {
                pid: pid.clone(),
                start_time_ticks: *start_time_ticks,
                kernel_uid: *kernel_uid,
            }),
            Self::StopService { .. } | Self::Service { .. } => None,
        }
    }

    pub(crate) fn title(&self) -> String {
        match self {
            PendingAction::EndProcess { name, .. } => crate::i18n::message(
                "tm-end-name-51d17c",
                &[("arg0", crate::i18n::argument(&(name)))],
            ),
            PendingAction::KillProcess { name, .. } => crate::i18n::message(
                "tm-force-kill-name-c04bb5",
                &[("arg0", crate::i18n::argument(&(name)))],
            ),
            PendingAction::StopService { unit } => crate::i18n::message(
                "tm-stop-unit-5021f0",
                &[("arg0", crate::i18n::argument(&(unit)))],
            ),
            PendingAction::Service { unit, .. } => crate::i18n::message(
                "tm-unit-9d85ea",
                &[
                    (
                        "arg0",
                        crate::i18n::argument(&crate::i18n::display(self.confirm_label())),
                    ),
                    ("arg1", crate::i18n::argument(&(unit))),
                ],
            ),
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            PendingAction::EndProcess { pid, name, .. } => crate::i18n::message(
                "tm-sends-sigterm-to-name-pid-pid-the-process-may-refuse-ab1a78",
                &[
                    ("arg0", crate::i18n::argument(&(name))),
                    ("arg1", crate::i18n::argument(&(pid))),
                ],
            ),
            PendingAction::KillProcess { pid, name, .. } => crate::i18n::message(
                "tm-sends-sigkill-to-name-pid-pid-the-process-cannot-ref-7f892b",
                &[
                    ("arg0", crate::i18n::argument(&(name))),
                    ("arg1", crate::i18n::argument(&(pid))),
                ],
            ),
            PendingAction::Service { unit, verb } => match verb {
                crate::service_actions::Verb::Start => crate::i18n::message(
                    "tm-start-unit-this-may-also-start-services-it-depends-o-197cb3",
                    &[("arg0", crate::i18n::argument(&(unit)))],
                ),
                crate::service_actions::Verb::Restart => crate::i18n::message(
                    "tm-restart-unit-connections-and-work-using-this-service-670cf0",
                    &[("arg0", crate::i18n::argument(&(unit)))],
                ),
                crate::service_actions::Verb::Stop => stop_service_message(unit),
            },
            PendingAction::StopService { unit } => stop_service_message(unit),
        }
    }

    pub(crate) fn confirm_label(&self) -> &'static str {
        match self {
            PendingAction::EndProcess { .. } => "End",
            PendingAction::KillProcess { .. } => "Force kill",
            PendingAction::StopService { .. } => "Stop",
            PendingAction::Service { verb, .. } => match verb {
                crate::service_actions::Verb::Start => "Start",
                crate::service_actions::Verb::Stop => "Stop",
                crate::service_actions::Verb::Restart => "Restart",
            },
        }
    }
}

#[derive(Debug)]
pub(crate) struct LogsRequest {
    generation: u64,
    unit: String,
    cancellation: Option<CancelToken>,
}

impl LogsRequest {
    pub(crate) fn new(generation: u64, unit: String, cancellation: CancelToken) -> Self {
        Self {
            generation,
            unit,
            cancellation: Some(cancellation),
        }
    }

    pub(crate) fn accepts(&self, generation: u64, unit: &str) -> bool {
        self.generation == generation
            && self.unit == unit
            && self
                .cancellation
                .as_ref()
                .is_some_and(|token| !token.is_terminal())
    }

    pub(crate) fn cancel(mut self, cause: CancelCause) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.cancel(cause);
        }
    }

    pub(crate) fn complete(mut self) {
        self.cancellation = None;
    }
}

impl Drop for LogsRequest {
    fn drop(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.cancel(CancelCause::Shutdown);
        }
    }
}

/// Widget-id prefix for the global action toast and its live region.
pub(crate) const TOAST_ID_PREFIX: &str = "tm-action-toast";

/// Shared acknowledgement key; newer outcomes replace the toast and restart dwell.
pub(crate) const TOAST_COALESCE_KEY: &str = "tm-action-outcome";

fn stop_service_message(unit: &str) -> String {
    crate::i18n::message(
        "tm-stop-unit-services-that-depend-on-it-may-also-stop-6f8f3c",
        &[("arg0", crate::i18n::argument(&(unit)))],
    )
}

/// Map outcome urgency to toast tone: warnings are assertive, information is polite.
pub(crate) fn toast_tone_for(urgency: crate::services::Urgency) -> ToastTone {
    match urgency {
        crate::services::Urgency::Info => ToastTone::Info,
        crate::services::Urgency::Warning => ToastTone::Warning,
    }
}

fn network_is_up(subtitle: &str) -> bool {
    let operstate = subtitle.rsplit('•').next().unwrap_or(subtitle).trim();
    operstate.eq_ignore_ascii_case("up")
}

fn push_limited(history: &mut Vec<GraphPointData>, sample: GraphPointData) {
    let discontinuous = history.last().is_some_and(|previous| {
        sample.at < previous.at || sample.at - previous.at > MAX_SAMPLE_GAP
    });
    if discontinuous {
        history.clear();
    }
    let cutoff = sample.at.saturating_sub(HISTORY_WINDOW);
    history.push(sample);
    if let Some(first_fresh) = history.iter().position(|point| point.at >= cutoff)
        && first_fresh > 0
    {
        history.drain(0..first_fresh);
    }
    if history.len() > HISTORY_CAP {
        let overflow = history.len() - HISTORY_CAP;
        history.drain(0..overflow);
    }
}

#[path = "model_parts/core.rs"]
mod core;
pub(crate) use core::max_table_scroll;
pub(crate) use core::{HISTORY_CAP, HISTORY_WINDOW, MAX_SAMPLE_GAP, Model};
#[path = "model_parts/batch1.rs"]
mod batch1;
#[path = "model_parts/batch2.rs"]
mod batch2;
#[path = "model_parts/batch3.rs"]
mod batch3;
#[path = "model_parts/batch4.rs"]
mod batch4;
