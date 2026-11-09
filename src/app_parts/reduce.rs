//! Message reducer.

use super::*;

impl App {
    pub(crate) fn reduce(&mut self, message: Message) -> Task<Message> {
        if self
            .model
            .collections
            .as_ref()
            .is_some_and(collection::Collections::is_close_committed)
        {
            return Task::none();
        }
        match &message {
            Message::ActivityWindowOpened(window) => {
                return self.shell_activity.bind_window(*window);
            }
            Message::ActivityWindowTagged(window, tag, queued) => {
                self.shell_activity.confirm_tag(*window, tag, *queued);
                return Task::none();
            }
            _ => {}
        }
        if (self.session.is_closing() || self.model.actions.is_closing())
            && !matches!(
                &message,
                Message::SessionWake
                    | Message::SessionRetry(_)
                    | Message::SessionKeepOpen(_)
                    | Message::CollectionWake
                    | Message::CollectionFirstFrame
                    | Message::CollectionClose(_)
                    | Message::KeepActionsOpen(_)
                    | Message::RetryActionsClose(_)
                    | Message::RetryOwnedAction(_, _)
                    | Message::CancelOwnedAction(_, _)
                    | Message::AcknowledgeActionFailure(_, _)
                    | Message::Toast(_)
                    | Message::DismissNotice(_)
                    | Message::Window(enzui_widgets::title_bar_gesture::WindowCommand::Close)
            )
        {
            return Task::none();
        }
        match message {
            Message::ActivityWindowOpened(_) | Message::ActivityWindowTagged(..) => {}

            Message::DesktopActivate => return self.desktop.drain(None).1,
            Message::Theme(event) => {
                let before = self.themes.theme_generation();
                let task = self.themes.update(*event);
                debug_assert!(
                    self.themes.theme_generation() >= before,
                    "theme generations must be monotonic"
                );
                return task.map(|event| Message::Theme(Box::new(event)));
            }
            Message::CollectionFirstFrame => {
                if let Some(owner) = self.model.collections.as_mut() {
                    owner.frame_seen();
                }
                return self.poll_collections();
            }
            Message::CollectionWake => return self.poll_collections(),
            Message::CollectionTick => {
                // Settle the previous flight before cadence duplicate detection.
                let settled = self.poll_collections();
                self.request_paced_collection();
                return Task::batch([settled, self.poll_collections()]);
            }
            Message::CollectionClose(id) => return self.request_close(id),
            #[cfg(feature = "center")]
            Message::Center(event) => return self.center_event(event),
            Message::SelectPage(page) => {
                self.model.titlebar_menu_open = false;
                let changed = self.model.active != page || self.model.pending_page.is_some();
                if changed {
                    self.invalidate_collections_for_page_switch();
                }
                let retain_current = self.model.pending_page.is_some()
                    || self
                        .model
                        .collection_health
                        .phase(collection::page_source(self.model.active))
                        == collection::Phase::Current;
                let defer = changed
                    && self.model.collection_health.managed
                    && !matches!(
                        self.model
                            .collection_health
                            .phase(collection::page_source(page)),
                        collection::Phase::Current | collection::Phase::Empty
                    )
                    && retain_current;
                self.model.pending_page = defer.then_some(page);
                if !defer {
                    self.model.select_page(page);
                }
                if changed || self.model.collections.is_none() {
                    self.request_collection(collection::SourceKind::Cadence);
                }
                self.nav.dismiss_overlay();
                if defer {
                    return self.flight_wakes();
                }
                return Task::batch([self.prepare_device_facts_if_visible(), self.remember_page()]);
            }
            Message::SessionWake => return self.poll_collections(),
            Message::SessionRetry(generation) => {
                self.session.retry(generation, std::time::Instant::now());
                return self.poll_collections();
            }
            Message::SessionKeepOpen(generation) => {
                self.session.keep_open(generation);
                if !self.session.is_closing() {
                    self.request_collection(collection::SourceKind::Cadence);
                }
            }
            Message::SelectResource(id) => {
                let pending = self.model.pending_page.take().is_some();
                let changed = pending || self.model.active != Page::Performance;
                if changed {
                    self.invalidate_collections_for_page_switch();
                }
                self.model.select_resource(id);
                if changed || self.model.collections.is_none() {
                    self.request_collection(collection::SourceKind::Cadence);
                }
                self.nav.dismiss_overlay();
                return Task::batch([self.prepare_device_facts_if_visible(), self.remember_page()]);
            }
            Message::SelectProcess(target) => {
                self.model.select_process(target);
            }
            Message::ToggleAppGroup(name) => {
                self.model.toggle_app_group(name);
            }
            Message::SelectService(name) => {
                self.model.selected_service = name;
                self.model.menu.close();
                self.model.rebuild_tables();
            }
            Message::ToggleInspector => {
                self.model.inspector_open = !self.model.inspector_open;
            }
            Message::CloseInspector => {
                self.model.inspector_open = false;
                return iced::widget::operation::focus(crate::view::inspector::toggle_id());
            }
            Message::SearchChanged(q) => {
                self.model.set_query(q);
            }
            Message::TypeToSearch(text) => {
                if !self.model.active.has_searchable_list()
                    || self.model.a_modal_is_open()
                    || self.model.titlebar_menu_open
                    || self.model.menu.is_open()
                    || self.model.process_filter_menu_state.is_open()
                    || self.model.service_filter_menu_state.is_open()
                {
                    return Task::none();
                }
                if !self.model.inspector_beside() && self.model.window.height < 650.0 {
                    self.model.inspector_open = false;
                }
                let mut query = self.model.query.clone();
                query.push_str(&text);
                self.model.set_query(query);
                return Task::batch(
                    search_focus_operations::<Message>(self.model.active)
                        .into_iter()
                        .map(iced::advanced::widget::operate),
                );
            }
            Message::ProcessFilterMenu(event) => {
                if let Some(id) = enzui_widgets::menu_button::apply_event(
                    &mut self.model.process_filter_menu,
                    &mut self.model.process_filter_menu_state,
                    &event,
                ) {
                    self.model.set_process_filter(id.as_str());
                }
                return enzui_widgets::menu_button::focus_restore_operation(
                    &event,
                    Some(&view::page::filter_id(Page::Processes)),
                )
                .map_or_else(Task::none, iced::advanced::widget::operate);
            }
            Message::ServiceFilterMenu(event) => {
                if let Some(id) = enzui_widgets::menu_button::apply_event(
                    &mut self.model.service_filter_menu,
                    &mut self.model.service_filter_menu_state,
                    &event,
                ) {
                    self.model.set_service_filter(id.as_str());
                }
                return enzui_widgets::menu_button::focus_restore_operation(
                    &event,
                    Some(&view::page::filter_id(Page::Services)),
                )
                .map_or_else(Task::none, iced::advanced::widget::operate);
            }
            Message::SortProcesses(key) => {
                self.model.sort_processes_by(key);
            }
            Message::SortServices(key) => {
                self.model.sort_services_by(key);
            }
            Message::Refresh => {
                // Event classification cannot see the modal that owns the keyboard.
                if self.model.a_modal_is_open() {
                    return Task::none();
                }
                self.model.announce("Refreshing…");
                self.model.process_details.refresh();
                self.request_collection(collection::SourceKind::Refresh);
                return Task::batch([
                    self.prepare_device_facts_if_visible(),
                    Task::done(Message::CollectionWake),
                ]);
            }

            Message::TableWheelScrolled(dy) => self.model.scroll_table_by_wheel(dy),
            Message::TableScrolled(offset) => self.model.scroll_table_to(offset),
            Message::WindowResized(size) => {
                self.model.resize(size);
            }
            Message::WindowVisibilityChanged(visible) => {
                if self.model.window_visible != visible {
                    self.invalidate_collections();
                }
                self.model.window_visible = visible;
                if visible {
                    self.request_collection(collection::SourceKind::Cadence);
                    return self.prepare_device_facts_if_visible();
                }
            }
            Message::CheckWindowMinimized => {
                // An unfocused window may still be visible beside another window.
                return iced::window::latest().then(|window| match window {
                    Some(id) => iced::window::is_minimized(id).map(Message::WindowMinimized),
                    None => Task::none(),
                });
            }
            Message::WindowMinimized(Some(minimized)) => {
                if self.model.window_visible == minimized {
                    self.invalidate_collections();
                }
                self.model.window_visible = !minimized;
                if !minimized {
                    self.request_collection(collection::SourceKind::Cadence);
                    return self.prepare_device_facts_if_visible();
                }
            }
            Message::WindowMinimized(None) => {}
            Message::Sidebar(state) => self.nav = state,

            Message::OpenContextMenu(target, at) => {
                self.model.open_menu(target, at);
            }
            Message::ContextMenuEvent(event) => {
                let (chosen, focus) = self.model.menu.update(&event);
                if let Some((target, message)) = chosen {
                    if matches!(&target, model::MenuTarget::Process(target) if !self.model.process_target_is_current(target))
                    {
                        return focus;
                    }
                    if matches!(
                        &message,
                        Message::SelectProcess(_) | Message::SelectService(_)
                    ) {
                        self.model.inspector_open = true;
                    }
                    return Task::batch([focus, self.reduce(message)]);
                }
                return focus;
            }

            Message::RequestAction(action) => {
                self.model.request_action(action);
            }
            Message::ConfirmEvent(event) => {
                if self.model.actions.confirmation.is_some()
                    && matches!(event, ConfirmDialogEvent::Resolved(_))
                {
                    return Task::none();
                }
                return self.on_confirm(event);
            }
            Message::ActionConfirmEvent(generation, event) => {
                if self
                    .model
                    .actions
                    .confirmation
                    .as_ref()
                    .is_some_and(|intent| intent.generation == generation)
                {
                    return self.on_confirm(event);
                }
            }
            Message::CancelOwnedAction(generation, attempt) => {
                self.model.actions.cancel(generation, attempt);
            }
            Message::RetryOwnedAction(generation, attempt) => {
                self.model.actions.retry(generation, attempt);
            }
            Message::RunServiceAction(unit, action) => {
                if self.model.a_modal_is_open() || self.model.actions.is_closing() {
                    return Task::none();
                }
                if let Some(verb) = service_actions::Verb::parse(action) {
                    self.model
                        .request_action(PendingAction::Service { unit, verb });
                } else {
                    self.model
                        .announce_outcome(services::ActionOutcome::warning(
                            "Unsupported service action",
                        ));
                }
            }
            Message::AcknowledgeActionFailure(generation, attempt) => {
                let was_closing = self.model.actions.is_closing();
                self.model.actions.acknowledge(generation, attempt);
                if was_closing && !self.model.actions.is_closing() {
                    self.session.resume_after_action_veto();
                }
            }
            Message::KeepActionsOpen(generation) => {
                self.model.actions.keep_open(generation);
                if !self.model.actions.is_closing() {
                    self.session.resume_after_action_veto();
                }
            }
            Message::RetryActionsClose(generation) => self.model.actions.retry_close(generation),

            Message::OpenLogs(unit) => return self.arm_logs(unit),
            Message::LogsLoaded(generation, unit, text) => {
                self.apply_logs_text(generation, &unit, text);
            }
            Message::CloseLogs => {
                return self.dismiss_logs(enzui_core::modal::DismissCause::CloseButton);
            }
            Message::Dialog(event) => return self.reduce_dialog(event),
            Message::FocusNext => {
                // A modal owns its focus ring; global traversal would move focus twice.
                if !self.model.a_modal_is_open() {
                    return operate(traverse::<Message>(Traversal::Next));
                }
            }
            Message::FocusPrevious => {
                if !self.model.a_modal_is_open() {
                    return operate(traverse::<Message>(Traversal::Previous));
                }
            }

            Message::CopyText {
                value,
                confirmation,
            } => {
                return iced::clipboard::write(value)
                    .map(move |result| Message::CopyFinished(result, confirmation));
            }
            Message::CopyFinished(result, confirmation) => match result {
                Ok(()) => self.model.announce(confirmation),
                Err(_) => self
                    .model
                    .announce_outcome(services::ActionOutcome::warning(crate::i18n::message(
                        "tm-copy-failed",
                        &[],
                    ))),
            },

            Message::Window(command) => {
                if command == enzui_widgets::title_bar_gesture::WindowCommand::Close {
                    if let Some(id) = self.session.closing_window() {
                        return Task::done(Message::CollectionClose(id));
                    }
                    if self.model.actions.is_closing() {
                        return Task::none();
                    }
                    if self
                        .model
                        .collections
                        .as_ref()
                        .is_some_and(collection::Collections::is_close_committed)
                    {
                        return Task::none();
                    }
                    if let Some(id) = self
                        .model
                        .collections
                        .as_ref()
                        .and_then(collection::Collections::closing_window)
                    {
                        return Task::done(Message::CollectionClose(id));
                    }
                    return iced::window::latest().then(|id| match id {
                        Some(id) => Task::done(Message::CollectionClose(id)),
                        None => Task::none(),
                    });
                }
                if command == enzui_widgets::title_bar_gesture::WindowCommand::Minimize {
                    self.invalidate_collections();
                    self.model.window_visible = false;
                }
                return iced::window::latest().then(move |maybe_id| match maybe_id {
                    Some(id) => command.into_task(id),
                    None => Task::none(),
                });
            }

            Message::OpenShortcuts => return self.open_shortcuts(),
            Message::CloseShortcuts => {
                return self.dismiss_shortcuts();
            }

            Message::Toast(event) => {
                if matches!(event, enzui_widgets::toast_host::ToastEvent::Tick(_)) {
                    // Disarm the delivered wake before scheduling any remaining dwell time.
                    self.model.toast_expiry =
                        enzui_widgets::toast_host::ToastExpirySchedule::default();
                }
                self.model.toasts.update(event);
            }
            Message::DismissNotice(key) => {
                self.prune_dismissed_notices();
                self.model.dismissed_notices.insert(key);
            }

            Message::ToggleTitlebarMenu => {
                self.model.titlebar_menu_open = !self.model.titlebar_menu_open;
            }
            Message::CloseTitlebarMenu => self.model.titlebar_menu_open = false,
            Message::FocusSearch => {
                self.model.titlebar_menu_open = false;
                if self.model.a_modal_is_open() {
                    return Task::none();
                }
                if !self.model.active.has_searchable_list() {
                    return Task::none();
                }
                if !self.model.inspector_beside() && self.model.window.height < 650.0 {
                    self.model.inspector_open = false;
                }
                return Task::batch(
                    search_focus_operations::<Message>(self.model.active)
                        .into_iter()
                        .map(iced::advanced::widget::operate),
                );
            }

            Message::EndSelectedProcess => {
                // Uncaptured Delete must not replace a modal's pending action.
                if self.model.a_modal_is_open() {
                    return Task::none();
                }
                let pid = self.model.selected_pid.clone();
                if pid.is_empty() {
                    return Task::none();
                }
                let Some(process) = self.model.snapshot().processes.iter().find(|p| {
                    p.pid == pid
                        && p.start_time_ticks == self.model.selected_process_start_time_ticks
                }) else {
                    return Task::none();
                };
                let name = process.name.clone();
                let start_time_ticks = process.start_time_ticks;
                self.model.request_action(model::PendingAction::EndProcess {
                    pid,
                    start_time_ticks,
                    kernel_uid: process.kernel_uid,
                    name,
                });
            }
        }
        Task::none()
    }
}
