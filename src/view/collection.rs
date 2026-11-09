use crate::collection::{Phase, SourceKind, page_source};
use crate::message::Message;
use crate::model::Model;
use enzui_theme::EnzTheme;
use enzui_widgets::status_page::enz_status_page;
use enzui_widgets::{banner::BannerTone, enz_banner};
use iced::{Element, Length};

/// Replace unavailable content, preserving the loaded page during pending navigation.
pub(crate) fn blocking_page<'a>(model: &Model) -> Option<Element<'a, Message>> {
    if let Some(notice) = &model.collection_health.close_notice {
        return Some(
            enz_status_page(crate::i18n::display("Collection stopped"))
                .description(crate::i18n::display(notice.clone()))
                .into(),
        );
    }
    if !model.collection_health.managed || model.pending_page.is_some() {
        return None;
    }
    let source = page_source(model.active);
    let phase = model.collection_health.phase(source);
    let notice = model.collection_health.notice(source);
    let title = match phase {
        Phase::Current => return None,
        Phase::Pending => {
            let line = notice.unwrap_or_else(|| "Collecting\u{2026}".to_owned());
            return Some(enz_status_page(crate::i18n::display(line)).into());
        }
        Phase::Stale => "Waiting for fresh data",
        Phase::Failed => "Unavailable",
        Phase::Empty => "Nothing to show",
    };
    let mut page = enz_status_page(crate::i18n::display(title));
    if let Some(notice) = notice {
        page = page.description(crate::i18n::display(notice));
    }
    Some(page.into())
}

fn notice_slot<'a>(notice: Option<String>) -> Element<'a, Message> {
    match notice {
        Some(text) => enz_banner(crate::i18n::display(text))
            .tone(BannerTone::Warning)
            .into(),
        None => iced::widget::Space::new().height(0).into(),
    }
}

pub(crate) fn page<'a>(model: &'a Model, theme: &'a EnzTheme, width: f32) -> Element<'a, Message> {
    if let Some(page) = blocking_page(model) {
        return page;
    }
    let page = match model.active {
        crate::model::Page::Performance => super::performance::view(model, theme, width),
        _ => super::page::view(model, theme),
    };
    if !model.collection_health.managed {
        return page;
    }
    let health = &model.collection_health;
    let notice = (model.active == crate::model::Page::Performance)
        .then(|| health.notice(SourceKind::DeviceFacts))
        .flatten()
        .filter(|_| health.phase(SourceKind::DeviceFacts) != Phase::Pending);
    iced::widget::column![notice_slot(notice), page]
        .height(Length::Fill)
        .into()
}

pub(crate) fn sidebar<'a>(model: &'a Model, theme: &'a EnzTheme) -> Element<'a, Message> {
    let health = &model.collection_health;
    if health.managed && health.phase(SourceKind::Telemetry) != Phase::Current {
        let mut page = enz_status_page(crate::i18n::display("System activity"));
        if let Some(notice) = health.notice(SourceKind::Telemetry) {
            page = page.description(crate::i18n::display(notice));
        }
        return page.into();
    }
    super::sidebar::view(model, theme)
}
