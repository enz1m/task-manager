//! Model-owned table providers with keyed columns and cached rows.
//!
//! Process identities use PID, start time, and UID; application groups use
//! their names. Responsive column removal preserves keyed sort state.

use enzui_core::table::{ColumnKey, TableProvider};
use enzui_core::virtual_sequence::{ItemProvider, KeyedProvider, SequenceKey};

use crate::services::{ProcessData, ServiceData};

/// Cell content and presentation hints consumed by the table renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Cell {
    /// Left-aligned prose.
    Text(String),
    /// Right-aligned numeric value.
    Numeric(String),
    /// A status dot plus its label, with the dot in its own leading slot so
    /// the dot and the name stay aligned across rows.
    Status { label: String, tone: StatusTone },
    /// A process/unit name; `protected` dims and marks it, `app` puts the
    /// application's icon before it, and `indent` is how many group-chevron
    /// steps it sits in: one beside a group, two under one.
    Name {
        label: String,
        protected: bool,
        app: bool,
        indent: u8,
    },
    /// An application group's name, with its process count and the chevron
    /// that folds its members.
    Group {
        label: String,
        count: usize,
        expanded: bool,
    },
}

/// Which of the three status colours a row's state maps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusTone {
    Running,
    Failed,
    Stopped,
}

/// One column of a table: its stable key, its header label, and its layout
/// weight.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Column {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
    pub(crate) weight: f32,
}

impl Column {
    const fn new(key: &'static str, label: &'static str, weight: f32) -> Self {
        Self { key, label, weight }
    }
}

/// Process-table columns in layout order.
///
/// Shared memory remains available in the inspector instead of taking a table
/// column.
pub(crate) const PROCESS_COLUMNS: &[Column] = &[
    Column::new("name", "Name", 4.0),
    Column::new("cpu", "CPU", 1.4),
    Column::new("rss", "Memory", 1.8),
    Column::new("pid", "PID", 1.3),
    Column::new("user", "User", 2.0),
    Column::new("state", "State", 1.4),
];

/// Every service-table column.
pub(crate) const SERVICE_COLUMNS: &[Column] = &[
    Column::new("name", "Name", 4.0),
    Column::new("state", "State", 1.8),
    Column::new("sub_state", "Sub-state", 1.8),
    Column::new("load", "Load", 1.5),
    Column::new("description", "Description", 4.5),
];

/// Selects the process or service column set independently of slice addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TableKind {
    Processes,
    Services,
}

impl TableKind {
    /// Every column of this table, in layout order.
    pub(crate) fn columns(self) -> &'static [Column] {
        match self {
            TableKind::Processes => PROCESS_COLUMNS,
            TableKind::Services => SERVICE_COLUMNS,
        }
    }

    fn compact_keys(self) -> &'static [&'static str] {
        match self {
            TableKind::Processes => &["name", "cpu", "rss"],
            TableKind::Services => &["name", "state"],
        }
    }
}

/// Columns retained for a regular or compact presentation.
pub(crate) fn visible_columns(kind: TableKind, compact: bool) -> Vec<&'static Column> {
    let all = kind.columns();
    if !compact {
        return all.iter().collect();
    }
    let keep = kind.compact_keys();
    all.iter().filter(|c| keep.contains(&c.key)).collect()
}

/// What a process-table row stands for.
#[derive(Clone, Debug)]
pub(crate) enum RowShape {
    /// One process; `member` when it is listed under an expanded group.
    Process { member: bool },
    /// An application's processes folded into one row of their totals.
    Group {
        name: String,
        members: Vec<ProcessData>,
        expanded: bool,
    },
}

/// A [`TableProvider`] over filtered, sorted process rows and application groups.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProcessTable {
    rows: Vec<ProcessData>,
    // Parallel to rows for grouped projections; empty for plain process tables.
    shapes: Vec<RowShape>,
    // Overview rows share the disclosure gutter when any application is grouped.
    has_groups: bool,
    process_start: Option<usize>,
    columns: Vec<&'static Column>,
}

impl ProcessTable {
    pub(crate) fn new(rows: Vec<ProcessData>, columns: Vec<&'static Column>) -> Self {
        Self {
            rows,
            shapes: Vec::new(),
            has_groups: false,
            process_start: None,
            columns,
        }
    }

    /// A table whose rows may include application groups; `shapes` is
    /// parallel to `rows`.
    pub(crate) fn grouped(
        rows: Vec<ProcessData>,
        shapes: Vec<RowShape>,
        columns: Vec<&'static Column>,
    ) -> Self {
        debug_assert_eq!(
            rows.len(),
            shapes.len(),
            "each table row requires one cell shape"
        );
        let has_groups = shapes
            .iter()
            .any(|shape| matches!(shape, RowShape::Group { .. }));
        Self {
            rows,
            shapes,
            has_groups,
            process_start: None,
            columns,
        }
    }

    pub(crate) fn unified(
        mut application_rows: Vec<ProcessData>,
        mut shapes: Vec<RowShape>,
        processes: Vec<ProcessData>,
        columns: Vec<&'static Column>,
    ) -> Self {
        if application_rows.is_empty() {
            return Self::new(processes, columns);
        }
        let process_start = application_rows.len();
        shapes.extend(std::iter::repeat_n(
            RowShape::Process { member: false },
            processes.len(),
        ));
        application_rows.extend(processes);
        let mut table = Self::grouped(application_rows, shapes, columns);
        table.process_start = Some(process_start);
        table
    }

    pub(crate) fn is_process_boundary(&self, row: usize) -> bool {
        self.process_start
            .is_some_and(|start| start > 0 && start == row)
    }

    /// What the row at `index` stands for.
    pub(crate) fn shape(&self, index: usize) -> Option<&RowShape> {
        const PROCESS: RowShape = RowShape::Process { member: false };
        if index >= self.rows.len() {
            return None;
        }
        Some(self.shapes.get(index).unwrap_or(&PROCESS))
    }

    /// The process at `index`; `None` for a group row or past the end.
    pub(crate) fn process(&self, index: usize) -> Option<&ProcessData> {
        match self.shape(index)? {
            RowShape::Process { .. } => self.rows.get(index),
            RowShape::Group { .. } => None,
        }
    }

    /// How many processes the table lists, folded members included.
    pub(crate) fn process_count(&self) -> usize {
        if self.has_groups || self.process_start.is_some() {
            self.processes().count()
        } else {
            self.rows.len()
        }
    }

    /// Every individual process once, excluding duplicate overview entries.
    pub(crate) fn processes(&self) -> impl Iterator<Item = &ProcessData> {
        self.rows
            .iter()
            .enumerate()
            .skip(self.process_start.unwrap_or(0))
            .flat_map(move |(index, row)| {
                let (own, folded): (Option<&ProcessData>, &[ProcessData]) =
                    match self.shapes.get(index) {
                        Some(RowShape::Group {
                            members,
                            expanded: false,
                            ..
                        }) => (None, members),
                        Some(RowShape::Group { .. }) => (None, &[]),
                        _ => (Some(row), &[]),
                    };
                own.into_iter().chain(folded)
            })
    }

    pub(crate) fn rows(&self) -> &[ProcessData] {
        &self.rows
    }

    /// The row at `index`, or `None` past the end.
    pub(crate) fn row(&self, index: usize) -> Option<&ProcessData> {
        self.rows.get(index)
    }

    /// Update only the `selected` flag on each existing row without
    /// re-filtering or re-sorting. Used when the selection identity
    /// changes but the underlying data/filter/sort/width have not.
    pub(crate) fn refresh_selection(&mut self, selected_pid: &str, selected_start_time_ticks: u64) {
        for row in &mut self.rows {
            row.selected = !selected_pid.is_empty()
                && row.pid == selected_pid
                && row.start_time_ticks == selected_start_time_ticks;
        }
    }
}

impl ItemProvider<SequenceKey> for ProcessTable {
    fn len(&self) -> u64 {
        self.rows.len() as u64
    }
    fn item(&self, index: u64) -> Option<SequenceKey> {
        if let Some(RowShape::Group { name, .. }) = self.shape(index as usize) {
            return Some(SequenceKey::new(format!("application-group:{name}")));
        }
        self.rows.get(index as usize).map(|p| {
            SequenceKey::new(format!(
                "{}{}:{}:{:?}",
                if self
                    .process_start
                    .is_some_and(|start| (index as usize) < start)
                {
                    "application-member:"
                } else {
                    ""
                },
                p.pid,
                p.start_time_ticks,
                p.kernel_uid
            ))
        })
    }
}

impl KeyedProvider<SequenceKey> for ProcessTable {
    fn key(&self, index: u64) -> Option<SequenceKey> {
        self.item(index)
    }
}

impl TableProvider<Cell> for ProcessTable {
    fn column_count(&self) -> usize {
        self.columns.len()
    }
    fn column_key(&self, column: usize) -> Option<ColumnKey> {
        self.columns.get(column).map(|c| ColumnKey::new(c.key))
    }
    fn cell(&self, row: u64, column: usize) -> Option<Cell> {
        let p = self.rows.get(row as usize)?;
        let column = self.columns.get(column)?;
        let cell = process_cell(p, column.key);
        Some(match (self.shape(row as usize)?, cell) {
            (
                RowShape::Group {
                    members, expanded, ..
                },
                Cell::Name { label, .. },
            ) => Cell::Group {
                label,
                count: members.len(),
                expanded: *expanded,
            },
            (RowShape::Group { .. }, Cell::Numeric(_)) if column.key == "pid" => {
                Cell::Numeric(String::new())
            }
            (
                RowShape::Process { member },
                Cell::Name {
                    label,
                    protected,
                    app,
                    ..
                },
            ) if self.has_groups
                && self
                    .process_start
                    .is_none_or(|start| (row as usize) < start) =>
            {
                Cell::Name {
                    label,
                    protected,
                    app,
                    indent: if *member { 2 } else { 1 },
                }
            }
            (_, cell) => cell,
        })
    }
}

/// Project a process column, using an empty text cell for unknown keys.
pub(crate) fn process_cell(p: &ProcessData, key: &str) -> Cell {
    match key {
        "name" => Cell::Name {
            label: p.name.clone(),
            protected: !p.action_allowed,
            app: p.kind == "App",
            indent: 0,
        },
        "cpu" => Cell::Numeric(p.cpu_display()),
        "rss" => Cell::Numeric(non_empty(&p.rss)),
        "mem_percent" => Cell::Numeric(format!("{}%", p.memory.trim_end_matches('%'))),
        "pid" => Cell::Numeric(non_empty(&p.pid)),
        "user" => Cell::Text(non_empty(&p.user)),
        "state" => Cell::Text(crate::i18n::display(non_empty(&p.state))),
        _ => Cell::Text(String::new()),
    }
}

/// A [`TableProvider`] over a filtered, sorted systemd unit list.
#[derive(Clone, Debug, Default)]
pub(crate) struct ServiceTable {
    rows: Vec<ServiceData>,
    columns: Vec<&'static Column>,
}

impl ServiceTable {
    pub(crate) fn new(rows: Vec<ServiceData>, columns: Vec<&'static Column>) -> Self {
        Self { rows, columns }
    }

    pub(crate) fn rows(&self) -> &[ServiceData] {
        &self.rows
    }

    pub(crate) fn row(&self, index: usize) -> Option<&ServiceData> {
        self.rows.get(index)
    }

    /// Update only the `selected` flag on each existing row without
    /// re-filtering or re-sorting.
    pub(crate) fn refresh_selection(&mut self, selected_service: &str) {
        for row in &mut self.rows {
            row.selected = !selected_service.is_empty() && row.name == selected_service;
        }
    }
}

impl ItemProvider<SequenceKey> for ServiceTable {
    fn len(&self) -> u64 {
        self.rows.len() as u64
    }
    fn item(&self, index: u64) -> Option<SequenceKey> {
        self.rows
            .get(index as usize)
            .map(|s| SequenceKey::new(s.name.clone()))
    }
}

impl KeyedProvider<SequenceKey> for ServiceTable {
    fn key(&self, index: u64) -> Option<SequenceKey> {
        self.item(index)
    }
}

impl TableProvider<Cell> for ServiceTable {
    fn column_count(&self) -> usize {
        self.columns.len()
    }
    fn column_key(&self, column: usize) -> Option<ColumnKey> {
        self.columns.get(column).map(|c| ColumnKey::new(c.key))
    }
    fn cell(&self, row: u64, column: usize) -> Option<Cell> {
        let s = self.rows.get(row as usize)?;
        let column = self.columns.get(column)?;
        Some(service_cell(s, column.key))
    }
}

/// The cell a service shows for a column key.
///
/// The name column carries the status dot so the dot aligns with the service
/// name rather than with a column header.
pub(crate) fn service_cell(s: &ServiceData, key: &str) -> Cell {
    match key {
        "name" => Cell::Status {
            label: s.name.clone(),
            tone: status_tone(s),
        },
        "state" => Cell::Text(crate::i18n::display(non_empty(&s.state))),
        "sub_state" => Cell::Text(crate::i18n::display(non_empty(&s.sub_state))),
        "load" => Cell::Text(crate::i18n::display(non_empty(&s.load))),
        "description" => Cell::Text(non_empty(&s.description)),
        _ => Cell::Text(String::new()),
    }
}

/// Which tone a unit's state paints as. `sub_state` wins where it is
/// informative, because "active (exited)" is not the same as "active
/// (running)" and painting both green would tell the user the wrong thing.
pub(crate) fn status_tone(s: &ServiceData) -> StatusTone {
    match s.sub_state.as_str() {
        "running" => StatusTone::Running,
        "failed" => StatusTone::Failed,
        "exited" | "dead" => StatusTone::Stopped,
        _ => match s.state.as_str() {
            "active" => StatusTone::Running,
            "failed" => StatusTone::Failed,
            _ => StatusTone::Stopped,
        },
    }
}

fn non_empty(value: &str) -> String {
    if value.trim().is_empty() {
        "—".to_owned()
    } else {
        value.to_owned()
    }
}
