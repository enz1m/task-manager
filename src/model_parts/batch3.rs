//! Model mutations.

use super::*;

impl Model {
    pub(crate) fn select_page(&mut self, page: Page) {
        self.pending_page = None;
        let prev = self.active;
        self.active = page;
        // A scroll offset and row anchor cannot transfer between page partitions.
        self.table_scroll = 0.0;
        self.menu.close();
        self.titlebar_menu_open = false;
        self.process_filter_menu_state.close(CloseCause::Host);
        self.service_filter_menu_state.close(CloseCause::Host);

        if (prev == Page::Apps) != (page == Page::Apps) {
            self.rebuild_process_table();
        }
        self.clamp_table_scroll();

        // Navigation selects a destination row; refresh never retargets a lost selection.
        let selection_changed = match page {
            Page::Apps | Page::Processes => {
                let selected_is_visible = self.process_table.processes().any(|process| {
                    process.pid == self.selected_pid
                        && process.start_time_ticks == self.selected_process_start_time_ticks
                });
                if selected_is_visible {
                    false
                } else if let Some(first) = self.process_table.processes().next() {
                    self.selected_pid = first.pid.clone();
                    self.selected_process_start_time_ticks = first.start_time_ticks;
                    true
                } else {
                    let changed = !self.selected_pid.is_empty()
                        || self.selected_process_start_time_ticks != 0;
                    self.selected_pid.clear();
                    self.selected_process_start_time_ticks = 0;
                    changed
                }
            }
            Page::Services => {
                let selected_is_visible = self
                    .service_table
                    .rows()
                    .iter()
                    .any(|service| service.name == self.selected_service);
                if selected_is_visible {
                    false
                } else if let Some(first) = self.service_table.rows().first() {
                    self.selected_service = first.name.clone();
                    true
                } else {
                    let changed = !self.selected_service.is_empty();
                    self.selected_service.clear();
                    changed
                }
            }
            Page::Performance => false,
        };
        if selection_changed {
            match page {
                Page::Apps | Page::Processes => {
                    self.process_table.refresh_selection(
                        &self.selected_pid,
                        self.selected_process_start_time_ticks,
                    );
                }
                Page::Services => {
                    self.service_table.refresh_selection(&self.selected_service);
                }
                Page::Performance => {}
            }
        }
        self.sync_process_details();
    }

    pub(crate) fn select_resource(&mut self, id: impl Into<String>) {
        self.selected_resource_id = id.into();
        self.active = Page::Performance;
        self.menu.close();
        self.sync_process_details();
    }

    /// Set the tracked window size and rebuild table projections.
    pub(crate) fn resize(&mut self, window: Size) {
        self.window = window;
        self.rebuild_tables();
    }

    /// Update the search query and re-derive the visible rows.
    pub(crate) fn set_query(&mut self, query: String) {
        self.query = query;
        self.rebuild_tables();
    }

    /// Apply one of the process Filter popup's stable command ids.
    pub(crate) fn set_process_filter(&mut self, id: &str) {
        self.process_filter = match id {
            "running" => ProcessFilter::Running,
            "sleeping" => ProcessFilter::Sleeping,
            _ => ProcessFilter::All,
        };
        self.process_filter_menu = process_filter_menu(self.process_filter);
        self.table_scroll = 0.0;
        self.rebuild_process_table();
        self.clamp_table_scroll();
    }

    /// Apply one of the service Filter popup's stable command ids.
    pub(crate) fn set_service_filter(&mut self, id: &str) {
        self.service_filter = match id {
            "running" => ServiceFilter::Running,
            "stopped" => ServiceFilter::Stopped,
            "failed" => ServiceFilter::Failed,
            _ => ServiceFilter::All,
        };
        self.service_filter_menu = service_filter_menu(self.service_filter);
        self.table_scroll = 0.0;
        self.rebuild_service_table();
        self.clamp_table_scroll();
    }

    pub(crate) fn process_target_is_current(&self, target: &ProcessTarget) -> bool {
        self.snapshot
            .processes
            .iter()
            .any(|process| process.matches_target(target))
    }

    /// Select a process only while the event's captured identity is current.
    pub(crate) fn select_process(&mut self, target: ProcessTarget) {
        if self.process_target_is_current(&target) {
            self.selected_pid = target.pid;
            self.selected_process_start_time_ticks = target.start_time_ticks;
        } else {
            self.selected_pid.clear();
            self.selected_process_start_time_ticks = 0;
        }
        self.menu.close();
        self.process_table
            .refresh_selection(&self.selected_pid, self.selected_process_start_time_ticks);
        self.sync_process_details();
    }

    pub(crate) fn open_menu(&mut self, target: MenuTarget, at: Point) {
        // A delayed context event must not open on a replacement process.
        let target_is_current = match &target {
            MenuTarget::Process(process_target) => {
                let current = self.process_target_is_current(process_target);
                if current {
                    self.selected_pid.clone_from(&process_target.pid);
                    self.selected_process_start_time_ticks = process_target.start_time_ticks;
                } else {
                    self.selected_pid.clear();
                    self.selected_process_start_time_ticks = 0;
                }
                current
            }
            MenuTarget::Service(unit) => {
                self.selected_service.clone_from(unit);
                true
            }
        };
        if target_is_current {
            let items = crate::view::menu::items(self, &target);
            self.menu.open(
                target.clone(),
                enzui_core::context_menu::InvocationKind::Pointer(
                    enzui_core::context_menu::Point::new(at.x, at.y),
                ),
                items,
            );
        } else {
            self.menu.close();
        }
        match &target {
            MenuTarget::Process(_) => {
                self.process_table
                    .refresh_selection(&self.selected_pid, self.selected_process_start_time_ticks);
            }
            MenuTarget::Service(_) => {
                self.service_table.refresh_selection(&self.selected_service);
            }
        }
        self.sync_process_details();
    }

    /// Parks a confirmation without signalling; refuses stale or protected
    /// targets, or a full pending slot.
    pub(crate) fn request_action(&mut self, action: PendingAction) -> bool {
        if !self.action_is_allowed(&action) {
            let message = self.refusal_message(&action);
            // Refusals require warning urgency even when the sentence changes.
            self.announce_outcome(crate::services::ActionOutcome::warning(message));
            self.menu.close();
            return false;
        }
        if self.pending_action.is_some() || !self.actions.capture(&action, &self.snapshot) {
            self.announce_outcome(crate::services::ActionOutcome::warning(
                "Action retained or identity unavailable; finish or cancel it before requesting another",
            ));
            return false;
        }
        self.pending_action = Some(action);
        self.menu.close();
        true
    }

    pub(crate) fn refusal_message(&self, action: &PendingAction) -> String {
        match action {
            PendingAction::EndProcess {
                pid,
                start_time_ticks,
                kernel_uid,
                name,
            }
            | PendingAction::KillProcess {
                pid,
                start_time_ticks,
                kernel_uid,
                name,
            } => {
                let current = self.snapshot.processes.iter().find(|p| &p.pid == pid);
                let subject = || crate::services::process_subject(pid, name);
                let identity = match current {
                    None => Some(crate::i18n::message(
                        "tm-vanished-f6fa66",
                        &[("arg0", crate::i18n::argument(&(subject())))],
                    )),
                    Some(row) if row.start_time_ticks != *start_time_ticks => {
                        Some(crate::i18n::message(
                            "tm-target-pid-was-reused-2eaab3",
                            &[("arg0", crate::i18n::argument(&(subject())))],
                        ))
                    }
                    Some(row) if row.kernel_uid != *kernel_uid => Some(crate::i18n::message(
                        "tm-captured-target-uid-changed-719500",
                        &[("arg0", crate::i18n::argument(&(subject())))],
                    )),
                    _ => None,
                };
                if let Some(reason) = identity {
                    return crate::services::bounded_message(crate::i18n::message(
                        "tm-blocked-reason-nothing-signalled-348479",
                        &[("arg0", crate::i18n::argument(&(reason)))],
                    ));
                }
                let preflight = match current {
                    _ if self.collection_health.managed
                        && self
                            .collection_health
                            .phase(crate::collection::SourceKind::Processes)
                            != crate::collection::Phase::Current =>
                    {
                        Some("Blocked: process sample is stale; refresh before acting")
                    }
                    _ if self
                        .collections
                        .as_ref()
                        .is_some_and(crate::collection::Collections::is_closing) =>
                    {
                        Some("Blocked: application is closing; nothing signalled")
                    }
                    _ => None,
                };
                if let Some(message) = preflight {
                    return message.to_owned();
                }
                let caller = crate::services::current_user();
                current.and_then(|p| p.action_refusal(&caller)).map_or_else(
                    || {
                        let subject = crate::services::process_subject(pid, name);
                        crate::services::bounded_message(crate::i18n::message(
                            "tm-blocked-subject-is-protected-b057ad",
                            &[("arg0", crate::i18n::argument(&(subject)))],
                        ))
                    },
                    |reason| {
                        crate::services::bounded_message(crate::i18n::message(
                            "tm-blocked-ea594d",
                            &[("arg0", crate::i18n::argument(&(reason.reason())))],
                        ))
                    },
                )
            }
            PendingAction::StopService { unit } | PendingAction::Service { unit, .. } => {
                crate::services::bounded_message(crate::i18n::message(
                    "tm-blocked-ea594d",
                    &[(
                        "arg0",
                        crate::i18n::argument(
                            &(crate::services::ActionRefusal::app_policy(unit).reason()),
                        ),
                    )],
                ))
            }
        }
    }
}
