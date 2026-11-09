//! View and subscription.

use super::*;

impl App {
    pub(crate) fn view(&self) -> Element<'_, Message> {
        static READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if std::env::var("ENZUI_CAPTURE_READY").as_deref() == Ok("1")
            && self.session.startup_settled()
            && self
                .model
                .collections
                .as_ref()
                .is_some_and(super::collection::Collections::is_idle)
            && !READY.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            eprintln!("[CAPTURE_READY] task-manager-app");
        }

        let model = &self.model;
        let theme = self.themes.theme();

        let base_shell: Element<'_, Message> = view::shell::view(model, theme, self.nav);
        if model.collection_health.close_notice.is_some()
            || model.actions.is_closing()
            || self.session.is_closing()
        {
            // Close progress and vetoes stay reachable while closing.
            return enz_toast_layer(
                base_shell,
                self.notification_stack(false),
                ToastOverlayPlacement::default().bottom_inset(self.toast_bottom_inset(theme)),
            );
        }

        let with_menu = view::menu::present(model, theme, base_shell);

        let with_dialogs = model.dialogs.view(
            with_menu,
            |key| match key {
                model::DialogKey::Logs => view::logs::surface(model, theme),
                model::DialogKey::Shortcuts => {
                    enzui_widgets::shortcuts_dialog::enz_shortcuts_dialog::<Message>(
                        true,
                        keybindings::shortcut_dialog_entries(),
                    )
                    .title(crate::i18n::display("Keyboard Shortcuts"))
                    .into_surface(Some(Box::new(|| Message::CloseShortcuts)))
                }
            },
            Message::Dialog,
        );

        let process_confirmation_generation = model
            .actions
            .confirmation
            .as_ref()
            .map(|intent| intent.generation);
        let presentation: Element<'_, Message> = match &model.pending_action {
            Some(action) => enz_confirm_dialog(
                action.title(),
                services::bounded_message(action.message()),
                &model.confirm_state,
                move |event| match process_confirmation_generation {
                    Some(generation) => Message::ActionConfirmEvent(generation, event),
                    None => Message::ConfirmEvent(event),
                },
            )
            .confirm_label(crate::i18n::display(action.confirm_label()))
            .cancel_label(crate::i18n::display("Cancel"))
            .destructive(true)
            .id_prefix("tm-destructive")
            .into_host(with_dialogs),
            None => enzui_widgets::modal_host::enz_modal_host(with_dialogs, None, |event| {
                Message::ConfirmEvent(ConfirmDialogEvent::from_host(event))
            })
            .into_element(),
        };

        // Toast semantics must stay outside inert modal backgrounds.
        enz_toast_layer(
            presentation,
            self.notification_stack(true),
            ToastOverlayPlacement::default().bottom_inset(self.toast_bottom_inset(theme)),
        )
    }

    /// Status notices from the action and session owners, each with its
    /// dismissal key, minus the ones the user dismissed.
    pub(crate) fn status_notices(
        &self,
    ) -> Vec<(String, enzui_widgets::notice_stack::EnzNotice<Message>)> {
        let mut notices = crate::actions::notices(&self.model.actions);
        notices.extend(session::notices(&self.session));
        notices.retain(|(key, _)| !self.model.dismissed_notices.contains(key));
        notices
    }

    /// Forgets dismissals whose notice is gone, keeping the set bounded. Runs
    /// on dismissal only: a page-save failure clears while each retry is in
    /// flight, and pruning then would bring the same failure straight back.
    pub(crate) fn prune_dismissed_notices(&mut self) {
        if self.model.dismissed_notices.is_empty() {
            return;
        }
        let mut live = crate::actions::notices(&self.model.actions);
        live.extend(session::notices(&self.session));
        self.model
            .dismissed_notices
            .retain(|key| live.iter().any(|(live, _)| live == key));
    }

    /// The bottom-right stack: app-owned status notices above the transient toasts.
    fn notification_stack(&self, with_toasts: bool) -> Option<Element<'_, Message>> {
        let notices: Vec<_> = self
            .status_notices()
            .into_iter()
            .map(|(key, notice)| notice.on_dismiss(Message::DismissNotice(key)))
            .collect();
        let toasts = (with_toasts && !self.model.toasts.is_empty())
            .then(|| self.model.toasts.view().map(Message::Toast));
        if notices.is_empty() {
            return toasts;
        }
        let spacing = self.themes.theme().spacing.xs();
        let stack: Element<'_, Message> = enzui_widgets::notice_stack::enz_notice_stack(notices)
            .spacing(spacing)
            .into();
        Some(
            iced::widget::column(std::iter::once(stack).chain(toasts))
                .spacing(spacing)
                .align_x(iced::Alignment::End)
                .into(),
        )
    }

    pub(crate) fn toast_bottom_inset(&self, theme: &EnzTheme) -> f32 {
        if !matches!(self.model.active, Page::Apps | Page::Processes) {
            return 0.0;
        }

        // The overlay supplies the gutter above the summary band's top edge.
        view::shell::resolved_shell_tokens(&theme.spacing).effective_edge_inset()
            + theme.spacing.space9
            + view::page::summary_band_height(theme)
    }

    pub(crate) fn theme(&self) -> iced::Theme {
        self.themes.iced_theme()
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        let started = self
            .model
            .collections
            .as_ref()
            .is_none_or(|owner| owner.painted && !owner.is_closing());
        let samples = sampler::subscription(
            self.model.active.sample_scope(),
            self.model.window_visible && started && !self.session.is_closing(),
        )
        .map(|()| Message::CollectionTick);
        let events = iced::event::listen_with(classify_window_event);
        let themes = self
            .themes
            .subscription()
            .map(|event| Message::Theme(Box::new(event)));
        let mut subscriptions = vec![
            samples,
            events,
            themes,
            self.desktop.subscription(|_| Message::DesktopActivate),
        ];

        #[cfg(feature = "center")]
        subscriptions.push(self.center_subscription());
        let mut needs_collection_wake = self.model.actions.needs_wake()
            || (self.model.collections.is_some() && self.model.process_details.needs_wake());
        if let Some(owner) = &self.model.collections
            && !owner.is_close_committed()
        {
            if !owner.painted {
                subscriptions.push(iced::window::frames().map(|_| Message::CollectionFirstFrame));
            }
            needs_collection_wake |= owner.needs_wake();
        }
        if self.session.needs_wake() {
            subscriptions
                .push(iced::time::every(session::WAKE_INTERVAL).map(|_| Message::SessionWake));
        }
        if needs_collection_wake {
            subscriptions.push(
                iced::time::every(collection::WAKE_INTERVAL).map(|_| Message::CollectionWake),
            );
        }
        // A deadline key preserves timer identity across unrelated updates.
        if let Some((deadline, wait)) = self.model.toast_expiry.armed() {
            subscriptions.push(
                iced::time::every(wait)
                    .with(deadline)
                    .map(|(_deadline, now)| {
                        Message::Toast(enzui_widgets::toast_host::ToastEvent::Tick(now))
                    }),
            );
        }
        subscriptions.push(self.shell_activity.subscription());
        Subscription::batch(subscriptions)
    }
}
