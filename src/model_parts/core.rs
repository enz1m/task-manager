//! Model struct and history constants.

use super::*;

/// The complete, render-free application state.
#[derive(Debug)]
pub(crate) struct Model {
    // Cancel journal transport before collection workers are joined during field drop.
    pub(crate) logs_request: Option<LogsRequest>,
    pub(crate) collections: Option<crate::collection::Collections>,
    pub(crate) collection_health: crate::collection::Health,
    pub(crate) process_details: crate::process_details::Details,
    /// Active route from which navigation and row highlights are derived.
    pub(crate) active: Page,
    pub(crate) pending_page: Option<Page>,
    /// Resource displayed on the Performance page.
    pub(crate) selected_resource_id: String,
    pub(crate) selected_pid: String,
    pub(crate) capture_process_selection: Option<String>,
    /// Kernel start time distinguishing reuse of `selected_pid`.
    pub(crate) selected_process_start_time_ticks: u64,
    pub(crate) selected_service: String,
    pub(crate) inspector_open: bool,
    pub(crate) query: String,
    pub(crate) last_event: String,
    /// Transient acknowledgements coalesced into one visible toast.
    pub(crate) toasts: ToastHost,
    /// One-shot expiry schedule; an empty host installs no toast timer.
    pub(crate) toast_expiry: ToastExpirySchedule,
    /// Dismissal keys of status notices the user closed; a notice stays hidden
    /// until the state it reports (and so its key) changes.
    pub(crate) dismissed_notices: std::collections::BTreeSet<String>,
    pub(crate) process_sort: SortState,
    pub(crate) service_sort: SortState,
    pub(crate) process_filter: ProcessFilter,
    pub(crate) service_filter: ServiceFilter,
    pub(crate) process_filter_menu: MenuModel,
    pub(crate) service_filter_menu: MenuModel,
    pub(crate) titlebar_menu_open: bool,
    pub(crate) process_filter_menu_state: MenuState,
    pub(crate) service_filter_menu_state: MenuState,
    pub(crate) logs_unit: String,
    pub(crate) logs_text: String,
    /// A journal request is pending; an empty result alone does not imply loading.
    pub(crate) logs_loading: bool,
    pub(crate) logs_generation: u64,
    /// Last reported resize; starts at 1400 × 900 until the window reports its size.
    pub(crate) window: Size,
    /// Enables collection requests and cadence wakes while the window is available.
    pub(crate) window_visible: bool,
    pub(crate) menu:
        enzui_widgets::context_menu_host::ContextMenu<MenuTarget, crate::message::Message>,
    pub(crate) pending_action: Option<PendingAction>,
    pub(crate) actions: crate::actions::Actions,
    pub(crate) dialogs: enzui_widgets::modal_host::DialogStack<DialogKey>,
    pub(crate) confirm_state: enzui_core::confirm::ConfirmState,
    /// Vertical scroll offset of the active table, in pixels.
    pub(crate) table_scroll: f32,
    // Keep table allocations separate so page switches cannot clamp against another table.
    pub(crate) process_table_viewport: TableViewportState,
    pub(crate) service_table_viewport: TableViewportState,
    // Static device walks must not become steady-state sampling work.
    pub(crate) device_facts: DeviceFacts,
    pub(crate) device_facts_loading: bool,
    pub(crate) device_facts_loaded: bool,
    pub(crate) process_table: ProcessTable,
    pub(crate) expanded_app_groups: std::collections::BTreeSet<String>,
    pub(crate) service_table: ServiceTable,
    pub(crate) snapshot: Snapshot,
    // History timestamps are monotonic offsets from this epoch.
    pub(crate) epoch: Instant,
    pub(crate) cpu_history: Vec<GraphPointData>,
    pub(crate) memory_history: Vec<GraphPointData>,
    pub(crate) disk_history: Vec<GraphPointData>,
    pub(crate) network_history: HashMap<String, Vec<GraphPointData>>,
    pub(crate) gpu_history: Vec<GraphPointData>,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

/// History-window length in seconds; also scales the sample cap.
pub(crate) const HISTORY_LEN: usize = 72;

/// Inclusive time span retained by each history ring.
pub(crate) const HISTORY_WINDOW: Duration = Duration::from_secs(HISTORY_LEN as u64);

/// Maximum samples per ring; excess samples are removed from the oldest end.
pub(crate) const HISTORY_CAP: usize = HISTORY_LEN * 10;

/// A larger gap clears history instead of joining samples across missing observations.
pub(crate) const MAX_SAMPLE_GAP: Duration = Duration::from_secs(5);

pub(crate) fn max_table_scroll(rows: usize, body_height: f32) -> f32 {
    ((rows as f32 * layout::TABLE_ROW_HEIGHT) - body_height.max(0.0)).max(0.0)
}
