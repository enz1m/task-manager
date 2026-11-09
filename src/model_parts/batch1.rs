//! Model construction and snapshots.

use super::*;

impl Model {
    /// Whether confirmation, Logs, or Keyboard Shortcuts owns keyboard focus.
    /// The shell's focus ring stays suppressed while a modal owns Tab.
    #[must_use]
    pub(crate) fn a_modal_is_open(&self) -> bool {
        !self.dialogs.is_empty() || self.pending_action.is_some()
    }

    /// Construct an explicit pending model without running collectors.
    pub(crate) fn new() -> Self {
        let mut model = Self::from_snapshot(Snapshot::default());
        model.collection_health = crate::collection::Health::pending();
        model
    }

    /// Build a model from collected data without running collectors.
    pub(crate) fn from_snapshot(snapshot: Snapshot) -> Self {
        let snapshot = snapshot.with_network_resources();
        let selected_resource_id = snapshot
            .resources
            .first()
            .map_or_else(|| "cpu".to_owned(), |item| item.id.clone());
        let selected_pid = snapshot
            .processes
            .first()
            .map(|item| item.pid.clone())
            .unwrap_or_default();
        let selected_process_start_time_ticks = snapshot
            .processes
            .first()
            .map_or(0, |item| item.start_time_ticks);
        let selected_service = snapshot
            .services
            .first()
            .map(|item| item.name.clone())
            .unwrap_or_default();

        let mut model = Self {
            active: Page::Performance,
            pending_page: None,
            selected_resource_id,
            selected_pid,
            capture_process_selection: None,
            selected_process_start_time_ticks,
            selected_service,
            inspector_open: true,
            query: String::new(),
            last_event: String::new(),
            toasts: ToastHost::new()
                .id_prefix(TOAST_ID_PREFIX)
                // Superseded outcomes must not queue for later announcements.
                .max_visible(1)
                .waiting_budget(WaitingBudget::unbounded().max_waiting(Some(0))),
            toast_expiry: ToastExpirySchedule::default(),
            dismissed_notices: std::collections::BTreeSet::new(),
            process_sort: SortState {
                key: "cpu".to_owned(),
                ascending: false,
            },
            service_sort: SortState {
                key: "name".to_owned(),
                ascending: true,
            },
            process_filter: ProcessFilter::All,
            service_filter: ServiceFilter::All,
            process_filter_menu: process_filter_menu(ProcessFilter::All),
            service_filter_menu: service_filter_menu(ServiceFilter::All),
            titlebar_menu_open: false,
            process_filter_menu_state: MenuState::closed(),
            service_filter_menu_state: MenuState::closed(),
            logs_unit: String::new(),
            logs_text: String::new(),
            logs_loading: false,
            logs_generation: 0,
            logs_request: None,
            window: Size::new(1400.0, 900.0),
            window_visible: true,
            menu: enzui_widgets::context_menu_host::ContextMenu::new(),
            pending_action: None,
            actions: crate::actions::Actions::default(),
            dialogs: enzui_widgets::modal_host::DialogStack::new(),
            confirm_state: enzui_core::confirm::ConfirmState::new(None),
            table_scroll: 0.0,
            process_table_viewport: TableViewportState::new(),
            service_table_viewport: TableViewportState::new(),
            collections: None,
            collection_health: crate::collection::Health::default(),
            process_details: crate::process_details::Details::default(),
            device_facts: DeviceFacts::default(),
            device_facts_loading: false,
            device_facts_loaded: false,
            process_table: ProcessTable::default(),
            expanded_app_groups: std::collections::BTreeSet::new(),
            service_table: ServiceTable::default(),
            snapshot,
            epoch: Instant::now(),
            cpu_history: Vec::new(),
            memory_history: Vec::new(),
            disk_history: Vec::new(),
            network_history: HashMap::new(),
            gpu_history: Vec::new(),
        };
        // Boot contributes only available measurements, never a fabricated past.
        model.push_graph_samples_at(Duration::ZERO);
        model.rebuild_tables();
        model
    }

    pub(crate) fn apply_captured_snapshot(&mut self, snapshot: Snapshot, captured_at: Instant) {
        self.apply_snapshot_at(snapshot, captured_at.saturating_duration_since(self.epoch));
    }

    /// Apply a snapshot sampled at a monotonic offset from the model's epoch.
    pub(crate) fn apply_snapshot_at(&mut self, snapshot: Snapshot, at: Duration) {
        let snapshot = snapshot.with_network_resources();
        // A selected interface keeps its chart when a down link hides its sidebar tile.
        if !snapshot
            .resources
            .iter()
            .any(|r| r.id == self.selected_resource_id)
        {
            self.selected_resource_id = snapshot
                .resources
                .first()
                .map(|resource| resource.id.clone())
                .unwrap_or_default();
        }
        let selected = self.selected_process().target();
        if !snapshot
            .processes
            .iter()
            .any(|process| process.matches_target(&selected))
        {
            self.selected_pid.clear();
            self.selected_process_start_time_ticks = 0;
        }
        if self.menu.target().is_some_and(|target| match target {
            MenuTarget::Process(target) => !snapshot
                .processes
                .iter()
                .any(|process| process.matches_target(target)),
            MenuTarget::Service(_) => false,
        }) {
            self.menu.close();
        }
        if self
            .pending_action
            .as_ref()
            .and_then(PendingAction::process_target)
            .is_some_and(|target| {
                !snapshot
                    .processes
                    .iter()
                    .any(|process| process.matches_target(&target))
            })
        {
            let description = self
                .actions
                .confirmation
                .as_ref()
                .map(crate::actions::Intent::description)
                .unwrap_or_default();
            self.pending_action = None;
            self.actions.confirmation = None;
            self.announce_outcome(crate::services::ActionOutcome::warning(
                crate::i18n::message(
                    "tm-blocked-captured-target-uid-or-process-identity-chan-5d24f8",
                    &[("arg0", crate::i18n::argument(&(description)))],
                ),
            ));
        }
        if !snapshot
            .services
            .iter()
            .any(|s| s.name == self.selected_service)
        {
            self.selected_service.clear();
        }
        if self.snapshot.gpu_reading.identity_key() != snapshot.gpu_reading.identity_key() {
            self.gpu_history.clear();
        }
        #[allow(
            clippy::iter_over_hash_type,
            reason = "each identity clears only its own keyed history"
        )]
        for (id, reading) in &snapshot.network_readings {
            if self
                .snapshot
                .network_readings
                .get(id)
                .and_then(crate::services::NetworkReading::identity)
                != reading.identity()
                && let Some(history) = self.network_history.get_mut(id)
            {
                history.clear();
            }
        }
        self.snapshot = snapshot;
        self.push_graph_samples_at(at);
        self.rebuild_tables();
        self.sync_process_details();
    }

    /// Sidebar resource tiles. A tile is highlighted only on the Performance
    /// page when it is the selected resource; `selected` is derived here, never
    /// stored per row.
    pub(crate) fn resources(&self) -> Vec<ResourceData> {
        let on_performance = self.active == Page::Performance;
        self.snapshot
            .resources
            .iter()
            .cloned()
            .map(|mut item| {
                item.selected = on_performance && item.id == self.selected_resource_id;
                item
            })
            .collect()
    }

    /// Sidebar resources with virtual and down network interfaces omitted.
    /// If no physical link is up, retain the first physical interface, or the
    /// first virtual interface when no physical interface exists.
    pub(crate) fn sidebar_resources(&self) -> Vec<ResourceData> {
        let all = self.resources();
        let network = |r: &ResourceData| r.id.starts_with("network");
        let physical = |r: &ResourceData| network(r) && !is_virtual_interface(&r.name);
        let first_network = all
            .iter()
            .position(physical)
            .or_else(|| all.iter().position(network));
        let any_up = all
            .iter()
            .any(|r| physical(r) && network_is_up(&r.subtitle));
        all.into_iter()
            .enumerate()
            .filter(|(idx, r)| {
                if !network(r) {
                    return true;
                }
                (physical(r) && network_is_up(&r.subtitle))
                    || (!any_up && first_network == Some(*idx))
            })
            .map(|(_, r)| r)
            .collect()
    }

    /// The resource whose Performance page is shown.
    pub(crate) fn active_resource(&self) -> ResourceData {
        self.snapshot
            .resources
            .iter()
            .find(|r| r.id == self.selected_resource_id)
            .cloned()
            .unwrap_or_else(ResourceData::empty)
    }

    /// History series (user samples) for the currently-selected resource.
    pub(crate) fn active_history(&self) -> &[GraphPointData] {
        self.history_for(&self.selected_resource_id)
    }

    /// History series for an arbitrary resource id — the source for a sidebar
    /// tile's mini chart. Same id→ring mapping as `active_history`, so the
    /// sparkline and the Performance chart read the same samples.
    pub(crate) fn history_for(&self, id: &str) -> &[GraphPointData] {
        match id {
            "cpu" => &self.cpu_history,
            "memory" => &self.memory_history,
            "disk" => &self.disk_history,
            "gpu" => &self.gpu_history,
            id if id.starts_with("network") => self
                .network_history
                .get(id)
                .map_or(&[] as &[_], Vec::as_slice),
            _ => &self.cpu_history,
        }
    }

    /// Processes filtered by the search query and sorted by the active key.
    /// Applications include their attributed helper processes; the other partition is background work.
    pub(crate) fn visible_processes(&self, apps_only: bool) -> Vec<ProcessData> {
        self.matching_processes(Some(apps_only))
    }

    pub(crate) fn all_visible_processes(&self) -> Vec<ProcessData> {
        self.matching_processes(None)
    }

    fn matching_processes(&self, apps_only: Option<bool>) -> Vec<ProcessData> {
        let query = enzui_core::palette::normalize(self.query.trim());
        let mut items: Vec<ProcessData> = self
            .snapshot
            .processes
            .iter()
            .filter(|p| {
                let is_app = p.kind == "App" || p.application_name.is_some();
                apps_only.is_none_or(|apps| apps == is_app)
            })
            .filter(|p| projection::process_matches(p, &query))
            .filter(|p| match self.process_filter {
                ProcessFilter::All => true,
                ProcessFilter::Running => p.state.eq_ignore_ascii_case("running"),
                ProcessFilter::Sleeping => p.state.eq_ignore_ascii_case("sleeping"),
            })
            .cloned()
            .collect();
        projection::sort_processes(
            &mut items,
            &self.process_sort.key,
            self.process_sort.ascending,
        );
        for item in &mut items {
            item.selected = !self.selected_pid.is_empty()
                && item.pid == self.selected_pid
                && item.start_time_ticks == self.selected_process_start_time_ticks;
        }
        items
    }
}

#[cfg(test)]
impl Model {
    pub(crate) fn apply_snapshot(&mut self, snapshot: Snapshot) {
        self.apply_snapshot_at(snapshot, self.epoch.elapsed());
    }
}

pub(crate) fn is_virtual_interface(name: &str) -> bool {
    name == "lo"
        || ["veth", "br-", "docker", "virbr", "vnet", "cni", "flannel"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}
