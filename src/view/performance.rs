//! Per-resource Performance page: a rolling-history area chart plus a real
//! stats column, and (for CPU) a wrapping per-core
//! utilization grid.

use enzui_core::chart::{DomainPolicy, Series, SeriesId, nice_extent, nice_ticks};
use enzui_core::semantics::HeadingLevel;
use enzui_theme::EnzTheme;
use enzui_widgets::area_chart::enz_area_chart;
use enzui_widgets::chart_geometry::{Y_LABEL_GAP, Y_TICK_TARGET, tick_step};
use enzui_widgets::chart_legend::{ChartLegendEntry, enz_chart_legend};
use enzui_widgets::chart_palette::ChartPalette;
use enzui_widgets::enz_text::{TextSize, TextTone, enz_text};
use enzui_widgets::grid::enz_grid;
use enzui_widgets::icon::{ICON_MD, enz_icon};
use enzui_widgets::progress_bar::{ProgressBarTokens, enz_progress_bar_sampled};
use enzui_widgets::scroll_view::enz_scroll_view;
use enzui_widgets::text_pair::{TextPairTokens, enz_text_pair};
use iced::widget::{column, container, row};
use iced::{Alignment, Color, Element, Length};

use crate::message::Message;
use crate::model::Model;
use crate::resource_ext::{ResourceView, Stat, device_stats};
use crate::services::{GraphPointData, ResourceData};

pub(crate) fn view<'a>(model: &Model, theme: &EnzTheme, width: f32) -> Element<'a, Message> {
    let palette = &theme.palette;
    let sp = &theme.spacing;
    let res = model.active_resource();
    let accent = res.accent();
    let typo = &theme.typography;

    let header = row![]
        .mirrored(crate::i18n::rtl())
        .align_y(Alignment::Center)
        .spacing(sp.sm())
        .extend([
            Element::from(
                column![
                    enz_text(if res.id.starts_with("network") {
                        res.name.clone()
                    } else {
                        crate::i18n::display(&res.name)
                    })
                    .size(TextSize::Title2)
                    .heading(HeadingLevel::Section),
                    enz_text(crate::i18n::display(res.subtitle.clone()))
                        .size(TextSize::Body)
                        .tone(TextTone::Muted),
                ]
                .spacing(sp.space1)
                .width(Length::Fill),
            ),
            // Not a heading despite its size: the live value changes every refresh
            // and would churn the heading outline.
            Element::from(
                enz_text(crate::i18n::display(res.value.clone()))
                    .size(TextSize::Title1)
                    .color(enzui_theme::Palette::brand_text_on(
                        palette.background,
                        accent,
                    )),
            ),
        ]);

    // `device_stats` yields a `Stat` only for a resolved field, so an
    // unavailable sensor produces no row rather than `n/a` or `0 °C`.
    let facts = device_stats(model.device_facts(), &res);
    // The fact rows feed the chart-height reflow input as their own
    // full-width rows, not as stats grid cells: a fact row is 45 px and
    // reflows through no column ladder.
    let (identity_rows, compact_stats) = stats_split(&res);
    let ncores = if res.id == "cpu" {
        model.snapshot().cpu_breakdown.cores.len()
    } else {
        0
    };
    let chart_height = crate::layout::chart_height(
        iced::Size::new(width, model.window.height),
        sp,
        crate::layout::PerfContent {
            identity_stats: identity_rows,
            compact_stats,
            facts: facts
                .iter()
                .map(|fact| 1 + fact.additional_values.len())
                .sum(),
            cores: ncores,
            copyable_rows: copyable_fact_rows(&res, &facts),
        },
    );
    let chart = resource_chart(model, theme, chart_height);

    let stats = resource_stats(&res, width, accent, palette, typo, sp);

    let mut body = column![header, chart, stats].spacing(sp.lg());

    if !facts.is_empty() {
        let mut details = column![].spacing(sp.sm()).width(Length::Fill);
        for stat in &facts {
            details = details.push(long_stat(stat, accent, palette, typo, sp));
        }
        body = body.push(details);
    }

    if res.id == "cpu" {
        let cores = &model.snapshot().cpu_breakdown.cores;
        if !cores.is_empty() {
            body = body.push(core_grid(model, width, accent, palette, typo, sp));
        }
    }

    container(enz_scroll_view(
        body.padding(iced::Padding::from(sp.space9).top(0.0)),
    ))
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

pub(crate) fn resource_chart<'a>(
    model: &Model,
    theme: &EnzTheme,
    chart_height: f32,
) -> Element<'a, Message> {
    let res = model.active_resource();
    let history = model.active_history();
    let live = model.window_visible;
    let chart_content = if res.id == "disk" {
        charts::live_disk_history_chart(history, chart_height, theme, model.epoch, live)
    } else if res.id.starts_with("network") {
        live_chart::live_chart(
            throughput_area_chart(&res.name, history, chart_height, theme),
            history,
            model.epoch,
            live,
        )
    } else {
        live_chart::live_chart(
            history_area_chart(&res.name, history, chart_height, theme),
            history,
            model.epoch,
            live,
        )
    };
    if live {
        chart_content
    } else {
        iced::widget::themer(
            Some(crate::theme::instant_sample_theme(theme)),
            chart_content,
        )
        .into()
    }
}

pub(crate) use charts::{history_area_chart, throughput_area_chart};
pub(crate) use stats::{copyable_fact_rows, core_grid, long_stat, resource_stats, stats_split};
pub(crate) mod charts;
mod live_chart;
mod stats;
