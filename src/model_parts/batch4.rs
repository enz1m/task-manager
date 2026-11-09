//! Model tables and details.

use super::*;

impl Model {
    /// Publish advisory text to `last_event` and the global toast host.
    /// Use [`Model::announce_outcome`] with warning urgency for refusals and failures.
    pub(crate) fn announce(&mut self, message: impl Into<String>) {
        self.announce_outcome(crate::services::ActionOutcome::info(message));
    }

    /// Publish an outcome to `last_event` and the coalescing global toast.
    ///
    /// Nonblank text replaces the current acknowledgement with the shared default dwell.
    /// Empty or whitespace-only text leaves the toast intact; `last_event` still stores
    /// the supplied text.
    pub(crate) fn announce_outcome(&mut self, outcome: crate::services::ActionOutcome) {
        let crate::services::ActionOutcome { text, urgency } = outcome;
        let text = crate::i18n::display(text);
        if !text.trim().is_empty() {
            self.toasts.push(
                ToastSpec::new(text.clone())
                    .tone(toast_tone_for(urgency))
                    .coalesce_key(TOAST_COALESCE_KEY),
            );
        }
        self.last_event = text;
    }

    /// Requires the captured row's policy permission and current collection health.
    pub(crate) fn action_is_allowed(&self, action: &PendingAction) -> bool {
        if self.actions.is_closing()
            || self
                .collections
                .as_ref()
                .is_some_and(crate::collection::Collections::is_closing)
        {
            return false;
        }
        if self.collection_health.managed
            && self
                .collection_health
                .phase(if crate::process_action_of(action).is_some() {
                    crate::collection::SourceKind::Processes
                } else {
                    crate::collection::SourceKind::Services
                })
                != crate::collection::Phase::Current
        {
            return false;
        }
        match action {
            PendingAction::EndProcess {
                pid,
                start_time_ticks,
                kernel_uid,
                ..
            }
            | PendingAction::KillProcess {
                pid,
                start_time_ticks,
                kernel_uid,
                ..
            } => self
                .snapshot
                .processes
                .iter()
                .find(|p| {
                    &p.pid == pid
                        && p.start_time_ticks == *start_time_ticks
                        && p.kernel_uid == *kernel_uid
                })
                .is_some_and(|p| p.action_allowed),
            PendingAction::StopService { unit } | PendingAction::Service { unit, .. } => self
                .snapshot
                .services
                .iter()
                .find(|s| &s.name == unit)
                .is_some_and(|s| s.action_allowed),
        }
    }

    /// Clear the pending action, returning it if the user confirmed.
    pub(crate) fn take_pending_action(&mut self, confirmed: bool) -> Option<PendingAction> {
        let action = self.pending_action.take();
        if confirmed {
            action
        } else {
            self.actions.confirmation = None;
            None
        }
    }

    /// Toggle/set the process sort key. Re-selecting the active key flips the
    /// direction; a new key adopts its sensible default direction.
    pub(crate) fn sort_processes_by(&mut self, key: impl Into<String>) {
        let key = key.into();
        if self.process_sort.key == key {
            self.process_sort.ascending = !self.process_sort.ascending;
        } else {
            self.process_sort.ascending = projection::default_process_sort_ascending(&key);
            self.process_sort.key = key;
        }
        self.table_scroll = 0.0;
        self.rebuild_process_table();
        self.clamp_table_scroll();
    }

    pub(crate) fn sort_services_by(&mut self, key: impl Into<String>) {
        let key = key.into();
        if self.service_sort.key == key {
            self.service_sort.ascending = !self.service_sort.ascending;
        } else {
            self.service_sort.ascending = projection::default_service_sort_ascending(&key);
            self.service_sort.key = key;
        }
        self.table_scroll = 0.0;
        self.rebuild_service_table();
        self.clamp_table_scroll();
    }

    /// Scroll the active table, clamped into the legal range.
    pub(crate) fn scroll_table_to(&mut self, offset: f32) {
        self.table_scroll = if offset.is_finite() { offset } else { 0.0 };
        self.clamp_table_scroll();
    }

    pub(crate) fn push_graph_samples_at(&mut self, at: Duration) {
        match self.snapshot.cpu_breakdown.measurement {
            Some(cpu) => push_limited(
                &mut self.cpu_history,
                GraphPointData {
                    user: cpu.busy_percent(),
                    system: 0,
                    at,
                },
            ),
            // Unknown ends the live series; otherwise its last point would be
            // announced as current or joined across an unobserved interval.
            None => self.cpu_history.clear(),
        }
        match self.snapshot.memory_reading.ram() {
            Some(memory) => push_limited(
                &mut self.memory_history,
                GraphPointData {
                    user: memory.used_percent(),
                    system: 0,
                    at,
                },
            ),
            // Unknown must not leave the previous reading labelled as current.
            None => self.memory_history.clear(),
        }
        match self.snapshot.disk_reading.activity() {
            Some(activity) => {
                let (read, write) = activity.history_values();
                push_limited(
                    &mut self.disk_history,
                    GraphPointData {
                        user: read,
                        system: write,
                        at,
                    },
                );
            }
            // No interval may be announced as current or bridged after a failed read.
            None => self.disk_history.clear(),
        }
        match self.snapshot.gpu_reading.busy() {
            Some(busy) => push_limited(
                &mut self.gpu_history,
                GraphPointData {
                    user: busy.percent(),
                    system: 0,
                    at,
                },
            ),
            None => self.gpu_history.clear(),
        }
        let live_network_ids: HashSet<&str> = self
            .snapshot
            .resources
            .iter()
            .filter(|resource| resource.id.starts_with("network"))
            .map(|resource| resource.id.as_str())
            .collect();
        // A departed interface has no history retention grace period. Keeping
        // its ring would attach old measurements to a later reuse of its name.
        self.network_history
            .retain(|id, _| live_network_ids.contains(id.as_str()));
        for resource in self
            .snapshot
            .resources
            .iter()
            .filter(|resource| resource.id.starts_with("network"))
        {
            let history = self.network_history.entry(resource.id.clone()).or_default();
            match self
                .snapshot
                .network_readings
                .get(&resource.id)
                .and_then(crate::services::NetworkReading::combined_kbps)
            {
                Some(throughput) => push_limited(
                    history,
                    GraphPointData {
                        user: throughput,
                        system: 0,
                        at,
                    },
                ),
                None => history.clear(),
            }
        }
    }
}
