//! Center functions for process listings, system summaries and page navigation.
//!
//! Listings omit command lines, which may carry credentials. Destructive process
//! and service actions are confined to user confirmation.

#[cfg(not(test))]
use enzui_agent::center::CenterHost;
#[cfg(not(test))]
use enzui_agent::center::{
    AppFunction, AppFunctionRegistry, AppMetadata, ConsentScope, FunctionRisk, FunctionSpec,
    object_schema,
};
use enzui_agent::center::{CenterFailure, CenterOutcome, CenterTurn, ConnectorEvent};
#[cfg(not(test))]
use serde::Deserialize;
use serde_json::{Value, json};

use super::*;

#[cfg(not(test))]
pub(crate) const CENTER_APP_ID: &str = "io.github.enzui.TaskManager";
#[cfg(not(test))]
pub(crate) const LIST_LIMIT_MAX: usize = 100;

#[cfg_attr(
    test,
    expect(
        dead_code,
        reason = "Calls are constructed by the native desktop connector"
    )
)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CenterCall {
    ListProcesses { limit: usize, apps_only: bool },
    SystemSummary,
    OpenPage { page: Page },
}

#[cfg(not(test))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    apps_only: bool,
}

#[cfg(not(test))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageArgs {
    page: PageName,
}

#[cfg(not(test))]
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum PageName {
    Performance,
    Apps,
    Processes,
    Services,
}

#[cfg(not(test))]
impl PageName {
    fn page(&self) -> Page {
        match self {
            Self::Performance => Page::Performance,
            Self::Apps => Page::Apps,
            Self::Processes => Page::Processes,
            Self::Services => Page::Services,
        }
    }
}

const PAGES: [(&str, Page); 4] = [
    ("performance", Page::Performance),
    ("apps", Page::Apps),
    ("processes", Page::Processes),
    ("services", Page::Services),
];

#[cfg(not(test))]
pub(crate) fn registry() -> AppFunctionRegistry<CenterCall> {
    let list = FunctionSpec::new(
        "list_processes",
        "List processes",
        "Lists running processes in the table's current order: pid, name, CPU, memory, \
         user and state. Command lines are not included.",
        object_schema(
            json!({
                "limit": {"type": "integer", "minimum": 1, "maximum": LIST_LIMIT_MAX},
                "apps_only": {"type": "boolean"}
            }),
            &[],
        ),
        object_schema(
            json!({
                "processes": {"type": "array", "items": object_schema(json!({
                    "pid": {"type": "string"},
                    "name": {"type": "string"},
                    "cpu": {"type": "string"},
                    "memory": {"type": "string"},
                    "user": {"type": "string"},
                    "state": {"type": "string"}
                }), &["pid", "name", "cpu", "memory", "user", "state"])},
                "total": {"type": "integer"}
            }),
            &["processes", "total"],
        ),
        FunctionRisk::SensitiveRead,
        vec![ConsentScope::Once, ConsentScope::Session],
    );
    let summary = FunctionSpec::new(
        "system_summary",
        "System summary",
        "Reports the CPU and memory summaries, uptime and the process count.",
        object_schema(json!({}), &[]),
        object_schema(
            json!({
                "cpu": {"type": "string"},
                "memory": {"type": "string"},
                "uptime": {"type": "string"},
                "process_count": {"type": "integer"}
            }),
            &["cpu", "memory", "uptime", "process_count"],
        ),
        FunctionRisk::ReadOnly,
        vec![
            ConsentScope::Once,
            ConsentScope::Session,
            ConsentScope::Always,
        ],
    );
    let open_page = FunctionSpec::new(
        "open_page",
        "Open a page",
        "Shows the performance, apps, processes or services page.",
        object_schema(
            json!({"page": {"type": "string", "enum": ["performance", "apps", "processes", "services"]}}),
            &["page"],
        ),
        object_schema(json!({"page": {"type": "string"}}), &["page"]),
        FunctionRisk::Mutating,
        vec![ConsentScope::Once, ConsentScope::Session],
    );
    AppFunctionRegistry::from_functions(vec![
        AppFunction::typed(list, |args: ListArgs| CenterCall::ListProcesses {
            limit: args.limit.unwrap_or(25).clamp(1, LIST_LIMIT_MAX),
            apps_only: args.apps_only,
        })
        .expect("list_processes spec"),
        AppFunction::new(summary, |_| CenterCall::SystemSummary).expect("system_summary spec"),
        AppFunction::typed(open_page, |args: PageArgs| CenterCall::OpenPage {
            page: args.page.page(),
        })
        .expect("open_page spec"),
    ])
    .expect("the task manager registry is valid")
}

#[cfg(not(test))]
pub(crate) fn metadata() -> AppMetadata {
    AppMetadata {
        id: CENTER_APP_ID.into(),
        name: "Task Manager".into(),
        description: "Processes, services and system resources".into(),
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

fn page_name(page: Page) -> &'static str {
    PAGES
        .iter()
        .find(|(_, candidate)| *candidate == page)
        .map_or("performance", |(name, _)| name)
}

impl App {
    #[cfg(not(test))]
    pub(crate) fn start_center(&mut self) {
        let Some(provision) = enzui_agent::center::CenterProvision::discover(CENTER_APP_ID) else {
            return;
        };
        match CenterHost::spawn(provision, metadata(), registry()) {
            Ok(host) => self.center = Some(host),
            Err(error) => eprintln!("task-manager-app: center connector disabled: {error}"),
        }
    }

    pub(crate) fn center_subscription(&self) -> Subscription<Message> {
        self.center
            .as_ref()
            .map_or_else(Subscription::none, |host| {
                host.subscription(Message::Center)
            })
    }

    pub(crate) fn shutdown_center(&mut self) {
        if let Some(host) = self.center.take() {
            host.shutdown();
        }
    }

    pub(crate) fn center_event(&mut self, event: ConnectorEvent) -> Task<Message> {
        let Some(host) = self.center.as_mut() else {
            return Task::none();
        };
        let CenterTurn::Call(call, request) = host.receive_with_ends(event).turn else {
            return Task::none();
        };
        let (task, outcome) = self.apply_center_call(request);
        if let Some(host) = self.center.as_mut() {
            host.answer(call, outcome);
        }
        task
    }

    fn apply_center_call(&mut self, request: CenterCall) -> (Task<Message>, CenterOutcome) {
        match request {
            CenterCall::ListProcesses { limit, apps_only } => {
                let rows = self.model.visible_processes(apps_only);
                let processes: Vec<Value> = rows
                    .iter()
                    .take(limit)
                    .map(|process| {
                        json!({
                            "pid": process.pid,
                            "name": process.name,
                            "cpu": process.cpu,
                            "memory": process.memory,
                            "user": process.user,
                            "state": process.state,
                        })
                    })
                    .collect();
                (
                    Task::none(),
                    Ok(json!({ "processes": processes, "total": rows.len() })),
                )
            }
            CenterCall::SystemSummary => {
                let snapshot = self.model.snapshot();
                (
                    Task::none(),
                    Ok(json!({
                        "cpu": snapshot.cpu_summary,
                        "memory": snapshot.memory_summary,
                        "uptime": snapshot.uptime,
                        "process_count": snapshot.processes.len(),
                    })),
                )
            }
            CenterCall::OpenPage { page } => {
                let task = self.update(Message::SelectPage(page));
                let outcome = if self.model.active == page {
                    Ok(json!({ "page": page_name(page) }))
                } else {
                    Err(CenterFailure::new(
                        "unavailable",
                        "the page could not be shown",
                    ))
                };
                (task, outcome)
            }
        }
    }
}

#[cfg(test)]
#[path = "center_tests.rs"]
mod tests;
