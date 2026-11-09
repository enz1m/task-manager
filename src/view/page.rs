//! Process and service pages with filters, grouped applications, inspectors and summary stats.

use enzui_core::semantics::HeadingLevel;
use enzui_theme::EnzTheme;
use enzui_widgets::banner::BannerTone;
use enzui_widgets::enz_banner;
use enzui_widgets::enz_pane_header;
use enzui_widgets::enz_text::{TextSize, line_box_px};
use enzui_widgets::menu_button::{MenuButtonKind, enz_menu_button};
use enzui_widgets::status_bar::{Distribution, StatusBarItem, enz_status_bar_strip};
use enzui_widgets::surface::RadiusSize;
use iced::widget::{column, container, row};
use iced::{Alignment, Element, Length};

use crate::layout;
use crate::message::Message;
use crate::model::{Model, Page};
use crate::view::{inspector, table};

pub(crate) fn view<'a>(model: &'a Model, theme: &'a EnzTheme) -> Element<'a, Message> {
    let sp = &theme.spacing;
    let title = match model.active {
        Page::Apps => "Applications",
        Page::Processes => "Processes",
        Page::Services => "Services",
        Page::Performance => "Processes",
    };

    let mut titlebar = enz_pane_header(crate::i18n::display(title))
        .title_size(TextSize::Title2.px())
        .weight(iced::font::Weight::Bold)
        .heading(HeadingLevel::Page)
        .spacing(sp.xs());

    let filter = filter_button(model);

    let details = enzui_widgets::toolbar_icon::enz_toolbar_icon("info", Message::ToggleInspector)
        .label(crate::i18n::display(if model.inspector_open {
            "Hide details"
        } else {
            "Show details"
        }))
        .id(inspector::toggle_id());
    titlebar = titlebar.trailing(
        row![filter, details]
            .mirrored(crate::i18n::rtl())
            .spacing(sp.sm())
            .align_y(Alignment::Center)
            .into(),
    );

    let table_el = table::view(model, theme);
    use enzui_widgets::motion::{MotionEffect, SlideDirection, motion};
    let overlay_inspector = !model.inspector_beside() && model.window.height < 650.0;
    let body: Element<'a, Message> = if model.inspector_beside() {
        let panel = container(inspector::view(model, theme))
            .padding(if crate::i18n::rtl() {
                iced::Padding::ZERO.right(sp.md())
            } else {
                iced::Padding::ZERO.left(sp.md())
            })
            .width(Length::Fixed(layout::INSPECTOR_WIDTH + sp.md()))
            .height(Length::Fill);
        let panel = super::inspector_reveal::reveal(
            panel,
            model.inspector_open,
            false,
            layout::INSPECTOR_WIDTH + sp.md(),
            theme,
        );
        row![
            container(table_el).width(Length::Fill).height(Length::Fill),
            panel
        ]
        .mirrored(crate::i18n::rtl())
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    } else if overlay_inspector {
        container(table_el)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else {
        let height = (model.window.height * 0.40).clamp(120.0, layout::STACKED_INSPECTOR_HEIGHT);
        let panel = container(inspector::view(model, theme))
            .padding(iced::Padding::ZERO.top(sp.md()))
            .width(Length::Fill)
            .height(height + sp.md());
        let panel = super::inspector_reveal::reveal(
            panel,
            model.inspector_open,
            true,
            height + sp.md(),
            theme,
        );
        column![
            container(table_el).width(Length::Fill).height(Length::Fill),
            panel
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    };

    let mut page_column = column![titlebar];
    if model.active_list_fabricated() {
        page_column = page_column.push(fallback_banner(model.active));
    }
    page_column = page_column.push(body);
    if matches!(model.active, Page::Apps | Page::Processes) {
        page_column = page_column.push(summary_bar(model, theme));
    }

    let page: Element<'a, Message> = container(
        page_column
            .spacing(sp.md())
            .padding(iced::Padding::from(sp.space9).top(0.0))
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into();
    if overlay_inspector {
        let base: Element<'a, Message> =
            enzui_widgets::inert::Inert::new(page, model.inspector_open).into();
        let panel = container(inspector::view(model, theme))
            .padding(iced::Padding::from(sp.space9).top(0.0))
            .width(Length::Fill)
            .height(Length::Fill);
        let panel = motion(
            motion(panel, model.inspector_open, MotionEffect::Fade),
            model.inspector_open,
            MotionEffect::Slide(SlideDirection::Down),
        );
        iced::widget::stack![base, panel]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else {
        page
    }
}

pub(crate) fn filter_id(page: Page) -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new(if page == Page::Services {
        "task-manager-service-filter"
    } else {
        "task-manager-process-filter"
    })
}

fn filter_button<'a>(model: &'a Model) -> Element<'a, Message> {
    if model.active == Page::Services {
        enz_menu_button(
            crate::i18n::display("Filter"),
            &model.service_filter_menu,
            &model.service_filter_menu_state,
            Message::ServiceFilterMenu,
        )
        .kind(MenuButtonKind::Quiet(enzui_widgets::ButtonSize::Sm))
        .aria_label(crate::i18n::display("Filter rows"))
        .menu_id(filter_id(model.active))
        .into_element()
    } else {
        enz_menu_button(
            crate::i18n::display("Filter"),
            &model.process_filter_menu,
            &model.process_filter_menu_state,
            Message::ProcessFilterMenu,
        )
        .kind(MenuButtonKind::Quiet(enzui_widgets::ButtonSize::Sm))
        .aria_label(crate::i18n::display("Filter rows"))
        .menu_id(filter_id(model.active))
        .into_element()
    }
}

fn fallback_banner<'a>(page: Page) -> Element<'a, Message> {
    let (noun, plural) = match page {
        Page::Services => ("service", "services"),
        _ => ("process", "processes"),
    };
    let title = match page {
        Page::Services => "Services unavailable",
        _ => "Processes unavailable",
    };
    enz_banner(crate::i18n::display(title))
        .tone(BannerTone::Warning)
        .body(crate::i18n::display(crate::i18n::message(
            "tm-the-noun-list-could-not-be-read-on-this-system-so-no-f22599",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(noun))),
                ("arg1", crate::i18n::argument(&crate::i18n::display(plural))),
            ],
        )))
        .into_element()
}

/// Return CPU percentages, total threads and the filtered process count in display order.
///
/// The process count includes folded group members. Unavailable measurements use an em dash.
pub(crate) fn summary_stats(model: &Model) -> Vec<(&'static str, String)> {
    let measurement = model.snapshot().cpu_breakdown.measurement;
    let percent = |component: fn(crate::services::CpuUsage) -> i32| {
        measurement.map_or_else(|| "—".to_owned(), |cpu| format!("{}%", component(cpu)))
    };
    let (threads, processes) = if model.snapshot().processes_fabricated {
        ("—".to_owned(), "—".to_owned())
    } else {
        (
            model.snapshot().thread_total.to_string(),
            model.process_table().process_count().to_string(),
        )
    };
    vec![
        ("System", percent(crate::services::CpuUsage::system_percent)),
        ("User", percent(crate::services::CpuUsage::user_percent)),
        ("Idle", percent(crate::services::CpuUsage::idle_percent)),
        ("Threads", threads),
        ("Processes", processes),
    ]
}

// Steal time prevents reconstructing omitted Idle from User and System percentages.
pub(crate) fn summary_cells(model: &Model, compact: bool) -> Vec<(&'static str, String)> {
    summary_stats(model)
        .into_iter()
        .filter(|(label, _)| !(compact && *label == "Idle"))
        .collect()
}

fn summary_bar<'a>(model: &Model, theme: &EnzTheme) -> Element<'a, Message> {
    let compact = layout::is_compact(model.window.width);
    build_summary_bar(summary_cells(model, compact), theme)
}

pub(crate) fn build_summary_bar<'a>(
    cells: Vec<(&'static str, String)>,
    theme: &EnzTheme,
) -> Element<'a, Message> {
    let sp = &theme.spacing;

    let mut strip = enz_status_bar_strip::<Message>()
        .bar_height(summary_band_height(theme))
        .text_size(theme.typography.body_size)
        .horizontal_padding(sp.md())
        .radius(RadiusSize::Lg)
        // Equal shares keep value widths from changing the stat pitch.
        .distribute(Distribution::EqualColumns);

    for (label, value) in cells {
        strip = strip.push_leading(StatusBarItem::stat(
            crate::i18n::display(label),
            crate::i18n::display(crate::i18n::number_text(value)),
        ));
    }

    strip.into_element()
}

pub(crate) fn summary_band_height(theme: &EnzTheme) -> f32 {
    let ty = &theme.typography;
    line_box_px(ty.body_size) + line_box_px(ty.caption_size) + 2.0 * theme.spacing.sm()
}
