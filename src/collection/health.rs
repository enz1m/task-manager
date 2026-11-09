//! Freshness, availability and generation status for collection domains.

use super::LIVE_CAPTURE_MAX_AGE;
use crate::services::SampleScope;
use std::collections::BTreeMap;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SourceKind {
    Boot,
    Cadence,
    Refresh,
    Processes,
    Services,
    Telemetry,
    DeviceFacts,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Terminal {
    Current,
    Empty,
    Failed,
    Cancelled,
}
#[derive(Clone, Debug)]
pub(crate) struct Stamp {
    pub(crate) source: SourceKind,
    pub(crate) generation: u64,
    pub(crate) captured_at: Instant,
    pub(crate) terminal: Terminal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Pending,
    Current,
    Stale,
    Failed,
    Empty,
}
#[derive(Clone, Debug)]
pub(crate) struct Status {
    pub(crate) phase: Phase,
    pub(crate) last: Option<Stamp>,
    pub(crate) last_good: Option<Instant>,
    pub(crate) cause: String,
}
impl Default for Status {
    fn default() -> Self {
        Self {
            phase: Phase::Pending,
            last: None,
            last_good: None,
            cause: String::new(),
        }
    }
}
#[derive(Debug, Default)]
pub(crate) struct Health {
    pub(crate) managed: bool,
    statuses: BTreeMap<SourceKind, Status>,
    pub(crate) close_notice: Option<String>,
}
impl Health {
    pub(crate) fn pending() -> Self {
        Self {
            managed: true,
            ..Self::default()
        }
    }
    pub(crate) fn phase(&self, source: SourceKind) -> Phase {
        let Some(status) = self.statuses.get(&source) else {
            return Phase::Pending;
        };
        if source != SourceKind::DeviceFacts
            && matches!(status.phase, Phase::Current | Phase::Empty)
            && status
                .last_good
                .is_some_and(|captured| captured.elapsed() > LIVE_CAPTURE_MAX_AGE)
        {
            Phase::Stale
        } else {
            status.phase
        }
    }
    pub(crate) fn begin(&mut self, source: SourceKind, keep_current: bool) {
        self.managed = true;
        let status = self.statuses.entry(source).or_default();
        if !keep_current || status.phase == Phase::Pending {
            status.phase = if status.last_good.is_some() {
                Phase::Stale
            } else {
                Phase::Pending
            };
            "Collection pending".clone_into(&mut status.cause);
        }
    }
    pub(crate) fn accept(&mut self, stamp: Stamp, cause: &str) {
        let status = self.statuses.entry(stamp.source).or_default();
        if status
            .last
            .as_ref()
            .is_some_and(|last| last.generation > stamp.generation)
        {
            return;
        }
        match stamp.terminal {
            Terminal::Current | Terminal::Empty => {
                status.last_good = Some(stamp.captured_at);
                status.phase = if stamp.terminal == Terminal::Empty {
                    Phase::Empty
                } else {
                    Phase::Current
                };
                status.cause.clear();
            }
            Terminal::Failed | Terminal::Cancelled => {
                status.phase = if status.last_good.is_some() {
                    Phase::Stale
                } else {
                    Phase::Failed
                };
                status.cause = bounded(cause);
            }
        }
        status.last = Some(stamp);
    }
    /// What a person is told about `source`, or `None` while its data is
    /// current: collecting, refreshing late, or unavailable, in plain words.
    pub(crate) fn notice(&self, source: SourceKind) -> Option<String> {
        let noun = source.noun();
        match self.phase(source) {
            Phase::Current => None,
            Phase::Empty => Some(crate::i18n::message(
                "tm-no-noun-were-found-e843e5",
                &[("arg0", crate::i18n::argument(&crate::i18n::display(noun)))],
            )),
            Phase::Pending => Some(crate::i18n::message(
                "tm-collecting-noun-7a992d",
                &[("arg0", crate::i18n::argument(&crate::i18n::display(noun)))],
            )),
            Phase::Stale => {
                let age = self
                    .statuses
                    .get(&source)
                    .and_then(|status| status.last_good)
                    .map(|at| ago(at.elapsed()))
                    .unwrap_or_default();
                Some(if age.is_empty() {
                    crate::i18n::message(
                        "tm-couldn-t-refresh-noun-28fbf9",
                        &[("arg0", crate::i18n::argument(&crate::i18n::display(noun)))],
                    )
                } else {
                    crate::i18n::message(
                        "tm-couldn-t-refresh-noun-last-read-age-ago-008ef8",
                        &[
                            ("arg0", crate::i18n::argument(&crate::i18n::display(noun))),
                            ("arg1", crate::i18n::argument(&(age))),
                        ],
                    )
                })
            }
            Phase::Failed => {
                let cause = self
                    .statuses
                    .get(&source)
                    .map(|status| status.cause.trim())
                    .filter(|cause| !cause.is_empty())
                    .map(|cause| {
                        crate::i18n::message(
                            "tm-cause-a5e937",
                            &[("arg0", crate::i18n::argument(&(cause)))],
                        )
                    })
                    .unwrap_or_default();
                Some(crate::i18n::message(
                    "tm-couldn-t-read-noun-cause-e0c657",
                    &[
                        ("arg0", crate::i18n::argument(&crate::i18n::display(noun))),
                        ("arg1", crate::i18n::argument(&(cause))),
                    ],
                ))
            }
        }
    }
}
/// An elapsed time as a person says it: "12 s", "3 min", "2 h".
fn ago(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..60 => crate::i18n::message(
            "tm-secs-s-127483",
            &[("arg0", crate::i18n::argument(&(secs)))],
        ),
        60..3600 => crate::i18n::message(
            "tm-min-e823f8",
            &[("arg0", crate::i18n::argument(&(secs / 60)))],
        ),
        _ => crate::i18n::message(
            "tm-h-4d631f",
            &[("arg0", crate::i18n::argument(&(secs / 3600)))],
        ),
    }
}

impl SourceKind {
    /// What this source collects, in a sentence.
    pub(crate) fn noun(self) -> &'static str {
        match self {
            SourceKind::Processes => "processes",
            SourceKind::Services => "services",
            SourceKind::Telemetry => "system activity",
            SourceKind::DeviceFacts => "device details",
            SourceKind::Boot | SourceKind::Cadence | SourceKind::Refresh => "system data",
        }
    }
}

pub(super) fn bounded(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(160)
        .collect()
}
pub(crate) fn domains(scope: SampleScope) -> Vec<SourceKind> {
    let mut sources = vec![SourceKind::Telemetry];
    if scope.processes {
        sources.push(SourceKind::Processes);
    }
    if scope.services {
        sources.push(SourceKind::Services);
    }
    sources
}
pub(crate) fn page_source(page: crate::model::Page) -> SourceKind {
    match page {
        crate::model::Page::Performance => SourceKind::Telemetry,
        crate::model::Page::Services => SourceKind::Services,
        _ => SourceKind::Processes,
    }
}
