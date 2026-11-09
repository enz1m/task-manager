//! Resource stats and grids.

use super::*;
use iced::widget::{Space, column, container, row};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatsShape {
    Cpu,
    Memory,
    Disk,
    Gpu,
    Network,
}

pub(crate) fn stats_shape(resource_id: &str) -> StatsShape {
    match resource_id {
        "cpu" => StatsShape::Cpu,
        "memory" => StatsShape::Memory,
        "disk" => StatsShape::Disk,
        "gpu" => StatsShape::Gpu,
        id if id.starts_with("network") => StatsShape::Network,
        _ => StatsShape::Memory,
    }
}

pub(crate) fn identity_labels(resource_id: &str) -> &'static [&'static str] {
    match stats_shape(resource_id) {
        StatsShape::Cpu | StatsShape::Memory => &[],
        StatsShape::Disk => &["Mount"],
        StatsShape::Gpu => &["Adapter", "Source", "Device"],
        StatsShape::Network => &["Interface"],
    }
}

// Share copy eligibility with rendering so the chart budget charges each control row.
pub(crate) fn copyable_fact_rows(resource: &ResourceData, facts: &[Stat]) -> usize {
    let identity = identity_labels(&resource.id);
    resource
        .stats()
        .iter()
        .filter(|stat| identity.contains(&stat.label.as_str()))
        .chain(facts.iter())
        .filter(|stat| copy_confirmation_for_fact(&stat.label).is_some())
        .map(|stat| 1 + stat.additional_values.len())
        .sum()
}

pub(crate) fn stats_split(resource: &ResourceData) -> (usize, usize) {
    let identity = identity_labels(&resource.id);
    let stats = resource.stats();
    let identity_rows = stats
        .iter()
        .filter(|stat| identity.contains(&stat.label.as_str()))
        .count();
    (identity_rows, stats.len() - identity_rows)
}

pub(crate) fn resource_stats<'a>(
    resource: &ResourceData,
    window_width: f32,
    accent: Color,
    palette: &enzui_theme::Palette,
    typo: &enzui_theme::TypographyScale,
    sp: &enzui_theme::SpacingScale,
) -> Element<'a, Message> {
    let stats = resource.stats();
    // Rendering and chart-height budgeting share the same responsive track count.
    let columns = crate::layout::stats_grid_columns(window_width);
    match stats_shape(&resource.id) {
        StatsShape::Cpu => compact_stats(&stats, columns, accent, palette, typo, sp),
        StatsShape::Memory => compact_stats(&stats, columns, accent, palette, typo, sp),
        StatsShape::Disk => identifiers_then_compact(
            &stats,
            identity_labels(&resource.id),
            columns,
            accent,
            palette,
            typo,
            sp,
        ),
        StatsShape::Gpu => identifiers_then_compact(
            &stats,
            identity_labels(&resource.id),
            columns,
            accent,
            palette,
            typo,
            sp,
        ),
        StatsShape::Network => identifiers_then_compact(
            &stats,
            identity_labels(&resource.id),
            columns,
            accent,
            palette,
            typo,
            sp,
        ),
    }
}

fn identifiers_then_compact<'a>(
    stats: &[Stat],
    identity_labels: &[&str],
    columns: usize,
    accent: Color,
    palette: &enzui_theme::Palette,
    typo: &enzui_theme::TypographyScale,
    sp: &enzui_theme::SpacingScale,
) -> Element<'a, Message> {
    let mut stack = column![].spacing(sp.sm()).width(Length::Fill);
    for stat in stats
        .iter()
        .filter(|stat| identity_labels.contains(&stat.label.as_str()))
    {
        stack = stack.push(long_stat(stat, accent, palette, typo, sp));
    }
    let compact: Vec<Stat> = stats
        .iter()
        .filter(|stat| !identity_labels.contains(&stat.label.as_str()))
        .cloned()
        .collect();
    if !compact.is_empty() {
        stack = stack.push(compact_stats(&compact, columns, accent, palette, typo, sp));
    }
    stack.into()
}

fn compact_stats<'a>(
    stats: &[Stat],
    columns: usize,
    accent: Color,
    palette: &enzui_theme::Palette,
    typo: &enzui_theme::TypographyScale,
    sp: &enzui_theme::SpacingScale,
) -> Element<'a, Message> {
    let cells = stats
        .iter()
        .map(|stat| compact_stat(stat, accent, palette, typo, sp))
        .collect();
    enz_grid(columns.max(1), cells)
        .gap(sp.sm())
        .stretch_cells(true)
        .into()
}

fn compact_stat<'a>(
    stat: &Stat,
    accent: Color,
    palette: &enzui_theme::Palette,
    typo: &enzui_theme::TypographyScale,
    sp: &enzui_theme::SpacingScale,
) -> Element<'a, Message> {
    let mut tokens = TextPairTokens::from_palette(palette);
    tokens.leading_color = palette.text_muted;
    let pair = enz_text_pair(
        crate::i18n::display(stat.label.clone()),
        if stat.label == "Load average" {
            stat.value
                .split_whitespace()
                .map(|value| crate::i18n::number_text(value.to_owned()))
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            crate::i18n::display(stat.value.clone())
        },
    )
    .typography(*typo)
    .spacing_scale(*sp)
    .tokens(tokens);
    icon_led(stat, pair.into(), accent, sp)
}

pub(crate) fn long_stat<'a>(
    stat: &Stat,
    accent: Color,
    _palette: &enzui_theme::Palette,
    typo: &enzui_theme::TypographyScale,
    sp: &enzui_theme::SpacingScale,
) -> Element<'a, Message> {
    let values = std::iter::once(&stat.value).chain(&stat.additional_values);
    let mut value_column = column![].spacing(sp.space1).width(Length::Fill);
    for value in values {
        let value_element: Element<'a, Message> = match copy_confirmation_for_fact(&stat.label) {
            Some(confirmation) if stat.label == "IP address" => {
                crate::view::copyable::copyable_value_flush_with_hint(
                    value,
                    confirmation,
                    fact_value_id(&stat.label, value),
                    iced::widget::text::Ellipsis::End,
                    typo.section_size,
                    crate::i18n::message(
                        "tm-copy-value-to-clipboard-1c959c",
                        &[("arg0", crate::i18n::argument(&(value)))],
                    ),
                )
            }
            Some(confirmation) => crate::view::copyable::copyable_value_flush(
                value,
                confirmation,
                fact_value_id(&stat.label, value),
                iced::widget::text::Ellipsis::End,
                typo.section_size,
            ),
            None => enz_text(crate::i18n::display(value.clone()))
                .size_px(typo.section_size)
                .single_line_elided()
                .into(),
        };
        value_column = value_column.push(value_element);
    }
    let text: Element<'a, Message> = column![
        enz_text(crate::i18n::display(stat.label.clone()))
            .size(TextSize::Caption)
            .tone(TextTone::Muted)
            .no_wrap(),
        value_column,
    ]
    .spacing(sp.space1)
    .width(Length::Fill)
    .into();
    icon_led(stat, text, accent, sp)
}

pub(crate) fn copy_confirmation_for_fact(label: &str) -> Option<&'static str> {
    match label {
        "IP address" => Some("Copied IP address"),
        // Device facts and resource identities use different labels for the same copyable field.
        "Interface" | "Network interface" => Some("Copied interface name"),
        "Mount" | "Mount point" => Some("Copied mount point"),
        _ => None,
    }
}

pub(crate) fn fact_value_id(label: &str, value: &str) -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::from(format!(
        "task-manager.performance.fact.{}.{}",
        label.to_ascii_lowercase().replace(' ', "-"),
        value
    ))
}

fn icon_led<'a>(
    stat: &Stat,
    content: Element<'a, Message>,
    accent: Color,
    sp: &enzui_theme::SpacingScale,
) -> Element<'a, Message> {
    let row: Element<'a, Message> = match stat.icon.as_deref() {
        Some(name) => row![enz_icon(name).size(ICON_MD).tint(accent), content]
            .mirrored(crate::i18n::rtl())
            .spacing(sp.sm())
            .align_y(if stat.additional_values.is_empty() {
                Alignment::Center
            } else {
                Alignment::Start
            })
            .width(Length::Fill)
            .into(),
        None => content,
    };
    container(row)
        .padding(iced::Padding::from(sp.xs()).left(sp.sm()).right(sp.sm()))
        .width(Length::Fill)
        .into()
}

pub(crate) fn core_grid<'a>(
    model: &Model,
    width: f32,
    accent: Color,
    palette: &enzui_theme::Palette,
    typo: &enzui_theme::TypographyScale,
    sp: &enzui_theme::SpacingScale,
) -> Element<'a, Message> {
    let cores = &model.snapshot().cpu_breakdown.cores;
    let columns = crate::layout::core_columns(width);

    let cells: Vec<Element<'a, Message>> = cores
        .iter()
        .map(|core| core_cell(&core.label, core.percent, accent, palette, typo))
        .collect();
    let grid: Element<'a, Message> = enz_grid(columns, cells).gap(6.0).stretch_cells(true).into();

    column![
        enz_text(crate::i18n::display("Utilization per logical processor"))
            .size(TextSize::Body)
            .tone(TextTone::Muted),
        grid,
    ]
    .spacing(sp.sm())
    .into()
}

fn core_cell<'a>(
    label: &str,
    percent: Option<i32>,
    accent: Color,
    palette: &enzui_theme::Palette,
    _typo: &enzui_theme::TypographyScale,
) -> Element<'a, Message> {
    let sampled: enzui_widgets::Sampled<f32> =
        percent.map(|p| p.clamp(0, 100) as f32 / 100.0).into();
    let mut bar_tokens = ProgressBarTokens::from_palette(palette);
    bar_tokens.track = palette.surface_selected;
    let bar = enz_progress_bar_sampled(sampled)
        .fill_color(accent)
        .bar_height(6.0)
        .bar_radius(3.0)
        .tokens(bar_tokens)
        .into_element();

    let reading: Element<'a, Message> = match percent {
        Some(p) => enz_text(crate::i18n::display(format!("{p}%")))
            .size(TextSize::Caption)
            .into(),
        None => enz_text(crate::i18n::display("—"))
            .size(TextSize::Caption2)
            .tone(TextTone::Subtle)
            .into(),
    };

    container(
        column![
            row![
                enz_text(crate::i18n::number_text(label.to_owned()))
                    .size(TextSize::Caption2)
                    .tone(TextTone::Muted),
                Space::new().width(Length::Fill),
                reading,
            ]
            .mirrored(crate::i18n::rtl()),
            bar,
        ]
        // The 3 px label-to-meter gap lies between spacing rungs.
        .spacing(3),
    )
    .width(Length::Fill)
    .into()
}
