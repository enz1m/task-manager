//! Model derived views.

use super::*;

impl Model {
    /// Services filtered by query and sorted by the active key.
    pub(crate) fn visible_services(&self) -> Vec<ServiceData> {
        let query = enzui_core::palette::normalize(self.query.trim());
        let mut items: Vec<ServiceData> = self
            .snapshot
            .services
            .iter()
            .filter(|s| projection::service_matches(s, &query))
            .filter(|s| match self.service_filter {
                ServiceFilter::All => true,
                ServiceFilter::Running => s.sub_state.eq_ignore_ascii_case("running"),
                ServiceFilter::Stopped => {
                    s.state.eq_ignore_ascii_case("inactive")
                        || s.sub_state.eq_ignore_ascii_case("dead")
                }
                ServiceFilter::Failed => s.state.eq_ignore_ascii_case("failed"),
            })
            .cloned()
            .collect();
        projection::sort_services(
            &mut items,
            &self.service_sort.key,
            self.service_sort.ascending,
        );
        for item in &mut items {
            item.selected = !self.selected_service.is_empty() && item.name == self.selected_service;
        }
        items
    }

    /// The process currently shown in the inspector, or an empty placeholder.
    pub(crate) fn selected_process(&self) -> ProcessData {
        self.snapshot
            .processes
            .iter()
            .find(|p| {
                p.pid == self.selected_pid
                    && p.start_time_ticks == self.selected_process_start_time_ticks
            })
            .cloned()
            .unwrap_or_else(ProcessData::empty)
    }

    pub(crate) fn sync_process_details(&mut self) {
        let process = self.selected_process();
        let target = (matches!(self.active, Page::Apps | Page::Processes)
            && !process.pid.is_empty())
        .then(|| process.target());
        self.process_details.select(target);
    }

    /// The service currently shown in the inspector, or an empty placeholder.
    pub(crate) fn selected_service_data(&self) -> ServiceData {
        self.snapshot
            .services
            .iter()
            .find(|s| s.name == self.selected_service)
            .cloned()
            .unwrap_or_else(ServiceData::empty)
    }

    pub(crate) fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Static hardware facts currently available to the Performance view.
    pub(crate) fn device_facts(&self) -> &DeviceFacts {
        &self.device_facts
    }

    /// Whether the one-shot collector should be prepared now.
    ///
    /// Device facts render only on Performance pages, so process/service list
    /// and inspector views do no `/sys` work. A completed empty result counts
    /// as loaded and is not polled again.
    pub(crate) fn should_load_device_facts(&self) -> bool {
        self.active == Page::Performance && !self.device_facts_loading && !self.device_facts_loaded
    }

    pub(crate) fn mark_device_facts_loading(&mut self) {
        self.device_facts_loading = true;
    }

    pub(crate) fn abandon_device_facts_load(&mut self) {
        self.device_facts_loading = false;
    }

    pub(crate) fn apply_device_facts(&mut self, facts: DeviceFacts) {
        self.device_facts = facts;
        self.device_facts_loading = false;
        self.device_facts_loaded = true;
    }

    /// Owned collections use explicit Pending status; unowned fixtures retain
    /// their raw-empty loading convention.
    pub(crate) fn processes_warming(&self) -> bool {
        if self.collection_health.managed {
            self.collection_health
                .phase(crate::collection::SourceKind::Processes)
                == crate::collection::Phase::Pending
        } else {
            self.snapshot.processes.is_empty()
        }
    }

    /// Whether the service collection is pending, not successfully empty.
    pub(crate) fn services_warming(&self) -> bool {
        if self.collection_health.managed {
            self.collection_health
                .phase(crate::collection::SourceKind::Services)
                == crate::collection::Phase::Pending
        } else {
            self.snapshot.services.is_empty()
        }
    }

    /// Whether the active page uses fabricated list data.
    /// Services reads the service flag; every other page reads the process flag.
    pub(crate) fn active_list_fabricated(&self) -> bool {
        match self.active {
            Page::Services => self.snapshot.services_fabricated,
            _ => self.snapshot.processes_fabricated,
        }
    }

    /// The realised process table for the current page.
    pub(crate) fn process_table(&self) -> &ProcessTable {
        &self.process_table
    }

    /// The realised service table.
    pub(crate) fn service_table(&self) -> &ServiceTable {
        &self.service_table
    }

    /// The columns currently laid out for the process table.
    pub(crate) fn process_columns(&self) -> Vec<&'static Column> {
        visible_columns(TableKind::Processes, layout::is_compact(self.window.width))
    }

    /// The columns currently laid out for the service table.
    pub(crate) fn service_columns(&self) -> Vec<&'static Column> {
        visible_columns(TableKind::Services, layout::is_compact(self.window.width))
    }

    /// Rebuild both table projections and clamp their shared scroll offset.
    /// Renderers borrow these cached rows without filtering or sorting.
    pub(crate) fn rebuild_tables(&mut self) {
        self.rebuild_process_table();
        self.rebuild_service_table();
        self.clamp_table_scroll();
    }

    pub(crate) fn rebuild_process_table(&mut self) {
        let apps = self.active == Page::Apps;
        self.process_table = if apps {
            let rows = self.visible_processes(true);
            let (rows, shapes) = crate::projection::group_applications(
                rows,
                &self.expanded_app_groups,
                !self.query.trim().is_empty(),
                &self.process_sort.key,
                self.process_sort.ascending,
            );
            ProcessTable::grouped(rows, shapes, self.process_columns())
        } else {
            let processes = self.all_visible_processes();
            let applications = processes
                .iter()
                .filter(|p| p.kind == "App" || p.application_name.is_some())
                .cloned()
                .collect();
            let (applications, shapes) = crate::projection::group_applications(
                applications,
                &self.expanded_app_groups,
                false,
                &self.process_sort.key,
                self.process_sort.ascending,
            );
            ProcessTable::unified(applications, shapes, processes, self.process_columns())
        };
    }

    /// Fold or unfold an application overview group.
    pub(crate) fn toggle_app_group(&mut self, name: String) {
        if !self.expanded_app_groups.remove(&name) {
            self.expanded_app_groups.insert(name);
        }
        self.rebuild_process_table();
        self.clamp_table_scroll();
    }

    pub(crate) fn rebuild_service_table(&mut self) {
        self.service_table = ServiceTable::new(self.visible_services(), self.service_columns());
    }

    // Before layout publishes body height, no trustworthy upper scroll bound exists.
    pub(crate) fn clamp_table_scroll(&mut self) {
        let rows = match self.active {
            Page::Services => self.service_table.rows().len(),
            _ => self.process_table.rows().len(),
        };
        let body_height = self
            .active_table_viewport()
            .resolved()
            .map(|v| v.body_height);
        self.table_scroll = match body_height {
            Some(body_height) => self
                .table_scroll
                .clamp(0.0, max_table_scroll(rows, body_height)),
            None => self.table_scroll.max(0.0),
        };
    }

    /// Horizontal tracks for the active table. Vertical geometry is not
    /// estimated here: `EnzTable` receives `Length::Fill` and publishes its
    /// actual body height through the corresponding viewport state.
    pub(crate) fn table_column_widths(&self) -> Vec<f32> {
        let columns = match self.active {
            Page::Services => self.service_columns(),
            _ => self.process_columns(),
        };
        let weights: Vec<f32> = columns.iter().map(|column| column.weight).collect();
        layout::table_column_widths(
            self.window,
            &weights,
            self.inspector_beside() && self.inspector_open,
        )
    }

    pub(crate) fn process_table_viewport(&self) -> &TableViewportState {
        &self.process_table_viewport
    }

    pub(crate) fn service_table_viewport(&self) -> &TableViewportState {
        &self.service_table_viewport
    }

    pub(crate) fn active_table_viewport(&self) -> &TableViewportState {
        match self.active {
            Page::Services => self.service_table_viewport(),
            _ => self.process_table_viewport(),
        }
    }

    /// Whether the inspector sits beside the table at the tracked window width.
    pub(crate) fn inspector_beside(&self) -> bool {
        !layout::should_stack(self.window.width)
    }
}
