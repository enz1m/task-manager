//! Resource meters and history sparklines that open the selected Performance resource.

use enzui_theme::{EnzTheme, Palette, SpaceToken};
use enzui_widgets::enz_text::{TextSize, enz_text};
use enzui_widgets::pressable::{PressableStyle, enz_pressable};
use enzui_widgets::progress_bar::{ProgressBarTokens, enz_progress_bar_sampled};
use enzui_widgets::scroll_view::enz_scroll_view;
use enzui_widgets::sidebar_content::enz_sidebar_content;
use enzui_widgets::sparkline::enz_sparkline;
use enzui_widgets::text_pair::{TextPairTokens, enz_text_pair};
use iced::widget::text::Ellipsis;
use iced::widget::{column, responsive, row};
use iced::{Alignment, Color, Element, Length};

use crate::message::Message;
use crate::model::Model;
use crate::resource_ext::ResourceView;
use crate::services::ResourceData;

pub(crate) fn view<'a>(model: &Model, theme: &EnzTheme) -> Element<'a, Message> {
    let palette = &theme.palette;
    let sp = &theme.spacing;
    let typo = &theme.typography;
    let chart_theme = crate::theme::instant_sample_theme(theme);

    let mut resource_rows = column![].spacing(sp.space3);
    for res in model.sidebar_resources() {
        let history = model.history_for(&res.id);
        resource_rows = resource_rows.push(resource_tile(
            &res,
            history,
            if res.id == "cpu" {
                model
                    .snapshot()
                    .cpu_breakdown
                    .measurement
                    .map(crate::services::CpuUsage::busy_percent)
            } else if res.id == "memory" {
                model
                    .snapshot()
                    .memory_reading
                    .ram()
                    .map(crate::services::MemoryUsage::used_percent)
            } else if res.id == "disk" {
                model
                    .snapshot()
                    .disk_reading
                    .activity()
                    .map(crate::services::DiskActivity::utilization_percent)
            } else if res.id == "gpu" {
                model
                    .snapshot()
                    .gpu_reading
                    .busy()
                    .map(crate::services::GpuBusy::percent)
            } else if res.id.starts_with("network") {
                model
                    .snapshot()
                    .network_readings
                    .get(&res.id)
                    .and_then(crate::services::NetworkReading::capacity_percent)
            } else {
                Some(res.percent)
            },
            palette,
            typo,
            sp,
            &chart_theme,
        ));
    }

    // The header band abuts the body under `SidebarHeader::Inside`, so the top
    // inset is `Lg`; an `Md` inset clips the selected first tile's border.
    let content = enz_sidebar_content()
        .padding_left(SpaceToken::Md)
        .padding_right(SpaceToken::Md)
        .padding_top(SpaceToken::Lg)
        .padding_bottom(SpaceToken::Md)
        .spacing(SpaceToken::Sm)
        .width(Length::Fill)
        .height(Length::Shrink)
        .push(resource_rows)
        .into_element();
    let body = enz_scroll_view(content).height(Length::Fill);

    body.into()
}

fn resource_tile<'a>(
    res: &ResourceData,
    history: &[crate::services::GraphPointData],
    measured_percent: Option<i32>,
    palette: &enzui_theme::Palette,
    typo: &enzui_theme::TypographyScale,
    sp: &enzui_theme::SpacingScale,
    chart_theme: &iced::Theme,
) -> Element<'a, Message> {
    let resource = res.clone();
    let history = history.to_vec();
    let style = TileStyle {
        palette: palette.clone(),
        typography: *typo,
        spacing: *sp,
        chart_theme: chart_theme.clone(),
    };
    let content = responsive(move |size| {
        resource_tile_content(&resource, &history, measured_percent, &style, size.width)
    })
    .height(Length::Shrink);

    // Horizontal padding must clear the rounded clip so trailing value glyphs remain intact.
    enz_pressable(content.into(), Message::SelectResource(res.id.clone()))
        .id(format!("task-manager-resource-{}", res.id).into())
        .style(PressableStyle::Row)
        .selectable(true)
        .selected(res.selected)
        .aria_label(crate::i18n::display(crate::i18n::message(
            "tm-performance-a3407e",
            &[(
                "arg0",
                crate::i18n::argument(&if res.id.starts_with("network") {
                    res.name.clone()
                } else {
                    crate::i18n::display(&res.name)
                }),
            )],
        )))
        .padding(iced::Padding::from(sp.md()).left(sp.sm()).right(sp.sm()))
        .width(Length::Fill)
        .into()
}

struct TileStyle {
    palette: enzui_theme::Palette,
    typography: enzui_theme::TypographyScale,
    spacing: enzui_theme::SpacingScale,
    chart_theme: iced::Theme,
}

fn resource_tile_content<'a>(
    res: &ResourceData,
    history: &[crate::services::GraphPointData],
    measured_percent: Option<i32>,
    style: &TileStyle,
    width: f32,
) -> Element<'a, Message> {
    let palette = &style.palette;
    let typo = &style.typography;
    let sp = &style.spacing;
    let chart_theme = &style.chart_theme;
    let accent = res.accent();
    // Value ink must clear 4.5:1 against the row's idle or selected backdrop;
    // meters and charts retain the raw accent.
    let tile_bg = if res.selected {
        palette.surface_selected
    } else {
        palette.surface_alt
    };
    let value_color = Palette::brand_text_on(tile_bg, accent);
    let name = if res.id.starts_with("network") {
        res.name.clone()
    } else {
        crate::i18n::display(&res.name)
    };
    let subtitle = res.subtitle.clone();
    let value = res.value.clone();
    let percent = measured_percent.map(|value| value.clamp(0, 100) as f32 / 100.0);

    // Live sample changes must not change tile height.
    let mut pair_tokens = TextPairTokens::from_palette(palette);
    pair_tokens.trailing_color = value_color;
    // Middle elision retains device suffixes.
    // Tooltips would obscure neighboring live meters.
    let subtitle_id: iced::advanced::widget::Id =
        format!("task-manager.sidebar.subtitle.{}", res.id).into();
    let subtitle_display: Element<'a, Message> = enz_text(crate::i18n::display(subtitle))
        .size(TextSize::Caption)
        .muted_on(tile_bg)
        .single_line_elided()
        .ellipsis(Ellipsis::Middle)
        .audit_elision_id(subtitle_id)
        .into();
    let label_stack = column![
        enz_text_pair(name, crate::i18n::display(value))
            .typography(*typo)
            .spacing_scale(*sp)
            .tokens(pair_tokens),
        subtitle_display,
    ]
    .spacing(enzui_theme::SpaceToken::Xxs.resolve(sp))
    .width(Length::Fill);

    let meter = meter_bar(percent, accent, palette);

    let text_half: Element<'a, Message> = column![label_stack, meter].spacing(sp.space3).into();

    if width < MINI_CHART_MIN_TILE_WIDTH {
        text_half
    } else {
        row![mini_chart(res, history, accent, chart_theme), text_half]
            .mirrored(crate::i18n::rtl())
            .align_y(Alignment::Center)
            .spacing(sp.sm())
            .into()
    }
}

const MINI_CHART_MIN_TILE_WIDTH: f32 = 180.0;
const MINI_CHART_W: f32 = 44.0;
const MINI_CHART_H: f32 = 40.0;

fn mini_chart<'a>(
    res: &ResourceData,
    history: &[crate::services::GraphPointData],
    accent: Color,
    chart_theme: &iced::Theme,
) -> Element<'a, Message> {
    let samples: Vec<f32> = history
        .iter()
        .map(|point| (point.user + point.system) as f32)
        .collect();
    // Network and disk histories are throughput with no universal ceiling;
    // CPU, memory and GPU remain literal 0..100 percentages.
    let normalize_to_peak = res.id.starts_with("network") || res.id == "disk";
    let chart_label = if res.id == "disk" {
        "Disk activity".to_owned()
    } else {
        crate::i18n::message(
            "tm-usage-bb7a58",
            &[(
                "arg0",
                crate::i18n::argument(&if res.id.starts_with("network") {
                    res.name.clone()
                } else {
                    crate::i18n::display(&res.name)
                }),
            )],
        )
    };

    let mut spark = enz_sparkline(samples)
        .style(enzui_widgets::sparkline::SparkStyle::Area)
        .label(crate::i18n::display(chart_label))
        .value_text(crate::i18n::reading(&res.value))
        .color(accent)
        .width(MINI_CHART_W)
        .height(MINI_CHART_H);
    if !normalize_to_peak {
        spark = spark.range(0.0, 100.0);
    }
    iced::widget::themer(Some(chart_theme.clone()), spark.into_element()).into()
}

fn meter_bar<'a>(
    fraction: Option<f32>,
    accent: Color,
    palette: &enzui_theme::Palette,
) -> Element<'a, Message> {
    let mut tokens = ProgressBarTokens::from_palette(palette);
    tokens.track = palette.surface_selected;
    enz_progress_bar_sampled(fraction.into())
        .fill_color(accent)
        .bar_height(4.0)
        .bar_radius(2.0)
        .tokens(tokens)
        .into_element()
}
