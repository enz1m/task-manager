//! Collection polling.

use super::*;

impl App {
    pub(crate) fn request_collection(&mut self, kind: collection::SourceKind) {
        self.request_collection_with(kind, false);
    }

    pub(crate) fn request_paced_collection(&mut self) {
        self.request_collection_with(collection::SourceKind::Cadence, true);
    }

    fn request_collection_with(&mut self, kind: collection::SourceKind, paced: bool) {
        if !self.model.window_visible
            || self.model.actions.is_closing()
            || self.session.is_closing()
        {
            return;
        }
        let scope = if kind == collection::SourceKind::Refresh {
            services::SampleScope::ALL
        } else {
            self.model
                .pending_page
                .unwrap_or(self.model.active)
                .sample_scope()
        };
        let owner = self
            .model
            .collections
            .get_or_insert_with(collection::Collections::system);
        let requested = if paced {
            owner.request_paced(scope)
        } else {
            owner.request(kind, scope)
        };
        if requested.is_some() {
            for source in collection::domains(scope) {
                self.model
                    .collection_health
                    .begin(source, kind == collection::SourceKind::Cadence);
            }
        } else if let Some(cause) = owner.refusal() {
            self.model
                .announce_outcome(services::ActionOutcome::warning(cause));
        }
    }

    pub(crate) fn prepare_device_facts_if_visible(&mut self) -> Task<Message> {
        if self.model.actions.is_closing()
            || self.session.is_closing()
            || !self.model.window_visible
            || !self.model.should_load_device_facts()
        {
            return Task::none();
        }
        let owner = self
            .model
            .collections
            .get_or_insert_with(collection::Collections::system);
        if owner
            .request_facts(self.device_facts_source.prepare())
            .is_some()
        {
            self.model.mark_device_facts_loading();
            self.model
                .collection_health
                .begin(collection::SourceKind::DeviceFacts, false);
        } else if let Some(cause) = owner.refusal() {
            self.model
                .announce_outcome(services::ActionOutcome::warning(cause));
        }
        Task::none()
    }

    pub(crate) fn invalidate_collections(&mut self) {
        self.invalidate_collections_with(false);
    }

    pub(crate) fn invalidate_collections_for_page_switch(&mut self) {
        // Page changes retire workers without hiding a still-current cached sample.
        self.invalidate_collections_with(true);
    }

    fn invalidate_collections_with(&mut self, keep_current: bool) {
        if let Some(owner) = self.model.collections.as_mut() {
            owner.invalidate();
        }
        for source in [
            collection::SourceKind::Processes,
            collection::SourceKind::Services,
            collection::SourceKind::Telemetry,
        ] {
            self.model.collection_health.begin(source, keep_current);
        }
        self.model.abandon_device_facts_load();
    }

    pub(crate) fn poll_session(&mut self) {
        if let Some(page) = self.session.poll(std::time::Instant::now()) {
            self.invalidate_collections();
            self.model.select_page(page);
            self.nav.dismiss_overlay();
            self.request_collection(collection::SourceKind::Boot);
            let _ = self.prepare_device_facts_if_visible();
        }
        if let Some(id) = self.session.ready_window()
            && !self.model.actions.is_closing()
            && !self.model.actions.start_close(id)
        {
            self.session.resume_after_action_veto();
        }
    }

    pub(crate) fn poll_collections(&mut self) -> Task<Message> {
        self.poll_session();
        if let Some(outcome) = self.model.actions.poll() {
            self.model.announce_outcome(outcome);
        }
        if let Some(id) = self.model.actions.advance_close()
            && !self
                .model
                .collections
                .as_ref()
                .is_some_and(collection::Collections::is_closing)
        {
            self.begin_collection_close(id);
        }
        let Some(owner) = self.model.collections.as_mut() else {
            return Task::none();
        };
        let events = owner.poll(std::time::Instant::now());
        let session_live = usize::from(self.session.ready_window().is_none());
        let (close, notice) = owner.owned_close_state(std::time::Instant::now(), session_live);
        self.model.collection_health.close_notice = notice;
        let wakes = owner.take_flight_wakes();
        let destination = self.model.pending_page;
        for event in events {
            self.apply_collection_event(event);
        }
        let navigation = if destination
            .is_some_and(|page| self.model.active == page && self.model.pending_page.is_none())
        {
            self.remember_page()
        } else {
            Task::none()
        };
        let wakes = Task::batch(
            wakes
                .into_iter()
                .map(|wake| Task::perform(wake, |()| Message::CollectionWake)),
        );
        if let Some(captured_id) = close
            && self.model.actions.commit_close(captured_id)
        {
            #[cfg(feature = "center")]
            self.shutdown_center();
            return Task::batch([wakes, iced::window::close(captured_id)]);
        }
        Task::batch([wakes, navigation])
    }

    pub(crate) fn apply_collection_event(&mut self, event: collection::Event) {
        use collection::{Event, Stamp, Terminal};
        if self.model.actions.is_closing() || self.session.is_closing() {
            return;
        }
        match event {
            Event::Snapshot(intent, stamp, result)
                if self
                    .model
                    .collections
                    .as_ref()
                    .is_some_and(|owner| owner.accepts_snapshot(intent)) =>
            {
                self.model.collection_health.accept(
                    stamp.clone(),
                    result.as_ref().err().map_or("", String::as_str),
                );
                match result {
                    Ok(mut collected) => {
                        if !intent.scope.processes {
                            collected
                                .snapshot
                                .processes
                                .clone_from(&self.model.snapshot().processes);
                            collected.snapshot.processes_fabricated =
                                self.model.snapshot().processes_fabricated;
                        }
                        if !intent.scope.services {
                            collected
                                .snapshot
                                .services
                                .clone_from(&self.model.snapshot().services);
                            collected.snapshot.services_fabricated =
                                self.model.snapshot().services_fabricated;
                        }
                        for source in collected.stamps {
                            self.model.collection_health.accept(source, "");
                        }
                        self.model
                            .apply_captured_snapshot(collected.snapshot, stamp.captured_at);
                        // Capture selection waits for process data and applies only once.
                        if intent.scope.processes
                            && !self.model.snapshot().processes.is_empty()
                            && let Some(selection) = self.model.capture_process_selection.take()
                        {
                            let target = self
                                .model
                                .process_table()
                                .processes()
                                .find(|process| selection == "first" || process.pid == selection)
                                .map(crate::services::ProcessData::target);
                            if let Some(target) = target {
                                self.model.select_process(target);
                            }
                        }
                    }
                    Err(cause) => {
                        for source in collection::domains(intent.scope) {
                            self.model.collection_health.accept(
                                Stamp {
                                    source,
                                    generation: intent.generation,
                                    captured_at: stamp.captured_at,
                                    terminal: if stamp.terminal == Terminal::Cancelled {
                                        Terminal::Cancelled
                                    } else {
                                        Terminal::Failed
                                    },
                                },
                                &cause,
                            );
                        }
                    }
                }
                // Only an accepted result for the requested domain may commit deferred navigation.
                if let Some(page) = self.model.pending_page
                    && collection::domains(intent.scope).contains(&collection::page_source(page))
                {
                    self.model.select_page(page);
                }
            }
            Event::Facts(generation, stamp, result)
                if self
                    .model
                    .collections
                    .as_ref()
                    .is_some_and(|owner| owner.accepts_facts(generation)) =>
            {
                self.model
                    .collection_health
                    .accept(stamp, result.as_ref().err().map_or("", String::as_str));
                match result {
                    Ok(facts) => self.model.apply_device_facts(facts),
                    Err(_) => self.model.abandon_device_facts_load(),
                }
            }
            _ => {}
        }
    }

    pub(crate) fn poll_process_details(&mut self) {
        let closing = self.session.is_closing()
            || self.model.actions.is_closing()
            || self
                .model
                .collections
                .as_ref()
                .is_some_and(collection::Collections::is_closing);
        if closing {
            self.model.process_details.invalidate();
        } else {
            self.model.sync_process_details();
        }
        if let Some(owner) = &self.model.collections {
            self.model.process_details.poll(
                &owner.detail_dispatcher(),
                std::time::Instant::now(),
                owner.painted && !closing,
            );
        }
    }
}
