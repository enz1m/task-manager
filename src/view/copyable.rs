//! Copyable readings with shared tooltip hints and confirmation messages.
//!
//! Values are the action targets; surrounding field labels remain inert.

use enzui_widgets::tokens::ButtonSizeTokens;
use enzui_widgets::tooltip::enz_tooltip;
use enzui_widgets::{ButtonKind, ButtonSize, enz_button};
use iced::Element;
use iced::widget::tooltip::Position;

use crate::message::Message;

pub(crate) const COPY_HINT: &str = "Click to copy";

pub(crate) fn copyable_value_flush<'a>(
    value: &str,
    confirmation: &'static str,
    id: iced::advanced::widget::Id,
    ellipsis: iced::widget::text::Ellipsis,
    text_size: f32,
) -> Element<'a, Message> {
    copyable_value_flush_with_hint(value, confirmation, id, ellipsis, text_size, COPY_HINT)
}

pub(crate) fn copyable_value_flush_with_hint<'a>(
    value: &str,
    confirmation: &'static str,
    id: iced::advanced::widget::Id,
    ellipsis: iced::widget::text::Ellipsis,
    text_size: f32,
    hint: impl Into<String>,
) -> Element<'a, Message> {
    copyable_value_boxed_with_hint(
        value,
        confirmation,
        id,
        ellipsis,
        text_size,
        ButtonSizeTokens::SM.height,
        (hint, Some(value.to_owned())),
    )
}

pub(crate) fn copyable_value_boxed<'a>(
    value: &str,
    label: String,
    confirmation: &'static str,
    id: iced::advanced::widget::Id,
    ellipsis: iced::widget::text::Ellipsis,
    text_size: f32,
    height: f32,
) -> Element<'a, Message> {
    copyable_value_boxed_with_hint(
        value,
        confirmation,
        id,
        ellipsis,
        text_size,
        height,
        (COPY_HINT, Some(label)),
    )
}

fn copyable_value_boxed_with_hint<'a>(
    value: &str,
    confirmation: &'static str,
    id: iced::advanced::widget::Id,
    ellipsis: iced::widget::text::Ellipsis,
    text_size: f32,
    height: f32,
    presentation: (impl Into<String>, Option<String>),
) -> Element<'a, Message> {
    let (hint, label) = presentation;
    build(
        value,
        confirmation,
        id,
        ellipsis,
        // Match neighboring text without losing the minimum vertical pointer target.
        ButtonSizeTokens {
            padding_x: 0.0,
            text_size,
            height: height.max(MIN_COPY_TARGET_HEIGHT),
            ..ButtonSizeTokens::SM
        },
        hint,
        label,
    )
}

pub(crate) const MIN_COPY_TARGET_HEIGHT: f32 = 24.0;

fn build<'a>(
    value: &str,
    confirmation: &'static str,
    id: iced::advanced::widget::Id,
    ellipsis: iced::widget::text::Ellipsis,
    size_tokens: ButtonSizeTokens,
    hint: impl Into<String>,
    label: Option<String>,
) -> Element<'a, Message> {
    let hint = crate::i18n::display(hint.into());
    let button = enz_button(
        label.unwrap_or_else(|| crate::i18n::reading(value)),
        Message::CopyText {
            value: value.to_owned(),
            confirmation,
        },
    )
    .kind(ButtonKind::Plain)
    .size(ButtonSize::Sm)
    .ellipsis(ellipsis)
    // Keep the value as the accessible name and put instructions in its description.
    .aria_description(crate::i18n::display(hint.clone()))
    .id(id)
    .size_tokens(size_tokens);
    // Top placement resolves from the value's bounds and flips below when the
    // viewport has no room above it.
    enz_tooltip(button.into_element(), hint, Position::Top)
}
