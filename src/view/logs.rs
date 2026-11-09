//! Modal service logs with selectable monospace text and whole-document copy.

use enzui_theme::{EnzTheme, FontRole};
use enzui_widgets::capped_dialog_body::CappedDialogBody;
use enzui_widgets::enz_text::{TextTone, enz_text};
use enzui_widgets::modal_surface::{ModalSurfaceTokens, enz_modal_surface};
use enzui_widgets::scroll_view::{Axis, enz_scroll_view};
use enzui_widgets::selection_scope::enz_selection_scope;
use enzui_widgets::surface::{BorderStyle, SurfaceTone, enz_surface};
use enzui_widgets::window_title::{WindowTitleTokens, enz_window_title};
use enzui_widgets::{ButtonKind, enz_button};
use iced::widget::{Space, container, row};
use iced::{Alignment, Element, Length};

pub(crate) const COPY_LOGS_LABEL: &str = "Copy all log text to clipboard";

use crate::message::Message;
use crate::model::Model;

const DIALOG_HEIGHT_FRACTION: f32 = 0.8;

/// Build the Logs surface; the shared stack owns modality and focus.
pub(crate) fn surface<'a>(model: &'a Model, theme: &EnzTheme) -> Element<'a, Message> {
    let sp = &theme.spacing;
    let typo = &theme.typography;

    let title_tokens = WindowTitleTokens {
        title_size: 17.0,
        subtitle_size: 11.0,
        ..WindowTitleTokens::defaults()
    };
    let title = row![
        container(
            enz_window_title(logs_heading(&model.logs_unit))
                .subtitle(crate::i18n::display(crate::services::journal_tail_caption()))
                .subtitle_tone(TextTone::Muted)
                .tokens(title_tokens)
        )
        .width(Length::Fill),
    ]
    .mirrored(crate::i18n::rtl())
    .align_y(Alignment::Center)
    .spacing(sp.sm());

    let body_text = logs_body(model.logs_loading, &model.logs_text);

    // Hard journal lines remain intact; the both-axis viewport owns overflow
    // while `CappedDialogBody` supplies the external height limit.
    let body_document = enz_text(if model.logs_loading || model.logs_text.is_empty() {
        crate::i18n::display(body_text)
    } else {
        body_text
    })
    .font_role(FontRole::Monospace)
    .font_from_theme(theme)
    .size_px(typo.monospace_size)
    .wrapping(iced::widget::text::Wrapping::None)
    .ellipsis(iced::widget::text::Ellipsis::None)
    .into_element();
    let selectable_body = enz_selection_scope(body_document)
        .id(iced::advanced::widget::Id::new(
            "task-manager-service-logs-selection",
        ))
        .label(crate::i18n::display("Service log text"))
        .document_key(model.logs_generation)
        .on_copy(|value| Message::CopyText {
            value,
            confirmation: "Copied selected log text",
        })
        .into_element();
    let body = enz_scroll_view(container(selectable_body).padding(sp.sm()))
        .axis(Axis::Both)
        .height(Length::Shrink)
        .width(Length::Fill);

    let framed_body = enz_surface(body)
        .tone(SurfaceTone::SurfaceAlt)
        .border_style(BorderStyle::Thin)
        .border_width(1.0)
        .radius(theme.radius.step(theme.shape.card))
        .fill_width()
        .into_element();

    let actions = row![
        Space::new().width(Length::Fill),
        // This remains a whole-document action beside passage selection.
        enz_button(
            crate::i18n::display("Copy All"),
            Message::CopyText {
                value: model.logs_text.clone(),
                confirmation: "Copied log text",
            },
        )
        .kind(ButtonKind::Secondary)
        .aria_label(crate::i18n::display(COPY_LOGS_LABEL))
        .id(iced::advanced::widget::Id::new("task-manager-logs-copy"))
        .enabled(logs_has_copyable_text(&model.logs_text)),
        enz_button(crate::i18n::display("Close"), Message::CloseLogs)
            .kind(ButtonKind::Primary)
            .id(iced::advanced::widget::Id::new("task-manager-logs-close")),
    ]
    .mirrored(crate::i18n::rtl())
    .spacing(sp.sm())
    .align_y(Alignment::Center);

    // The body cap subtracts measured title and action heights from the stack budget.
    let stack = CappedDialogBody::new(
        vec![title.into()],
        framed_body,
        vec![actions.into()],
        12.0,
        model.window.height * DIALOG_HEIGHT_FRACTION,
    );

    enz_modal_surface(stack.into())
        .padding(sp.lg())
        .width(Length::Fixed(dialog_width(model.window.width)))
        .tokens(ModalSurfaceTokens::from_palette(&theme.palette))
        .into_element()
}

/// Subtract a 48 px margin and clamp the requested width to 240–720 logical pixels.
pub(crate) fn dialog_width(window_width: f32) -> f32 {
    (window_width - 48.0).clamp(240.0, 720.0)
}

/// The reason a `service_logs` no-result sentence carries, if it carries one.
///
/// `services::no_logs_available` composes `"No logs available for <unit>."`
/// (empty) or `"No logs available for <unit>: <reason>"` (a fault). `": "` is
/// unambiguous because a systemd unit name cannot contain a space.
pub(crate) fn logs_no_result_reason(logs_text: &str) -> Option<&str> {
    let rest = logs_text.trim().strip_prefix("No logs available for ")?;
    let (_unit, reason) = rest.split_once(": ")?;
    let reason = reason.trim();
    (!reason.is_empty()).then_some(reason)
}

/// Whether the current logs payload is worth copying to the clipboard.
///
/// Empty text and a bare "No logs available…" sentinel are not copyable.
/// A no-result sentence that names a reason stays copyable.
pub(crate) fn logs_has_copyable_text(logs_text: &str) -> bool {
    let t = logs_text.trim();
    if t.is_empty() {
        return false;
    }
    !t.starts_with("No logs available") || logs_no_result_reason(t).is_some()
}

pub(crate) fn logs_heading(unit: &str) -> String {
    // The unit name is untrusted; a bidi override in it would rewrite the
    // heading, which cannot wrap away.
    crate::i18n::message(
        "tm-logs-f6134b",
        &[(
            "arg0",
            crate::i18n::argument(&(crate::services::bounded_subject(unit))),
        )],
    )
}

// App status messages are localised; journal content and external failure reasons stay verbatim.
pub(crate) fn logs_body(loading: bool, journal_text: &str) -> String {
    if loading {
        return crate::i18n::display("Loading…");
    }
    let t = journal_text.trim();
    if t.is_empty() {
        return crate::i18n::display("No logs available.");
    }
    if t.starts_with("No logs available") {
        return match logs_no_result_reason(t) {
            Some(reason) => crate::i18n::message(
                "tm-no-logs-reason",
                &[("reason", crate::i18n::argument(reason))],
            ),
            None => crate::i18n::display("No logs available."),
        };
    }
    journal_text.to_owned()
}
