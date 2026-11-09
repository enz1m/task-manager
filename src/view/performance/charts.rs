//! History charts and tick formatters.

use super::*;
use iced::widget::column;

pub(crate) fn history_points(history: &[GraphPointData]) -> Vec<(f64, f64)> {
    history_component_points(history, |point| point.user + point.system)
}

fn history_component_points(
    history: &[GraphPointData],
    value: impl Fn(&GraphPointData) -> i32,
) -> Vec<(f64, f64)> {
    let Some(newest) = history.last().map(|point| point.at) else {
        return Vec::new();
    };
    history
        .iter()
        .map(|point| {
            // Clock anomalies must not place samples after the newest timestamp.
            let age = newest.saturating_sub(point.at).as_secs_f64();
            (-age, f64::from(value(point)))
        })
        .collect()
}

pub(crate) fn history_span_secs(history: &[GraphPointData]) -> usize {
    match (history.first(), history.last()) {
        (Some(oldest), Some(newest)) => {
            let span = newest.at.saturating_sub(oldest.at).as_secs_f64().round() as usize;
            span.max(1)
        }
        _ => 1,
    }
}

pub(crate) fn chart_summary(name: &str, span_secs: usize, latest: Option<i32>) -> String {
    let Some(latest) = latest else {
        return crate::i18n::message(
            "tm-name-utilization-unavailable-no-data-db0c69",
            &[("arg0", crate::i18n::argument(&crate::i18n::display(name)))],
        );
    };
    crate::i18n::message(
        "tm-chart-summary",
        &[
            ("name", crate::i18n::argument(&crate::i18n::display(name))),
            ("seconds", crate::i18n::argument(&span_secs)),
            ("percent", crate::i18n::argument(&latest)),
        ],
    )
}

pub(crate) fn format_seconds_tick(value: f64, step: f64) -> String {
    if value.abs() < f64::EPSILON {
        crate::i18n::display("now")
    } else {
        // Fractional startup ticks need enough precision to stay distinct.
        let decimals =
            if value.fract().abs() > f64::EPSILON && step.is_finite() && step > 0.0 && step < 1.0 {
                (-step.log10()).ceil().clamp(1.0, 3.0) as usize
            } else {
                0
            };
        format!(
            "{}{}",
            crate::i18n::number_text(format!("{value:.decimals$}")),
            crate::i18n::display("s")
        )
    }
}

pub(crate) fn format_percent_tick(value: f64, step: f64) -> String {
    let decimals = usize::from(step.is_finite() && step > 0.0 && step < 1.0);
    crate::i18n::reading(&format!("{value:.decimals$}%"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThroughputScale {
    Kbps,
    Mbps,
    Gbps,
}

impl ThroughputScale {
    fn for_domain_max(domain_max_kbps: f64) -> Self {
        if domain_max_kbps >= 1_000_000.0 {
            Self::Gbps
        } else if domain_max_kbps >= 1_000.0 {
            Self::Mbps
        } else {
            Self::Kbps
        }
    }

    fn divisor(self) -> f64 {
        match self {
            Self::Kbps => 1.0,
            Self::Mbps => 1_000.0,
            Self::Gbps => 1_000_000.0,
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Self::Kbps => "Kbps",
            Self::Mbps => "Mbps",
            Self::Gbps => "Gbps",
        }
    }

    fn formatter(self) -> fn(f64, f64) -> String {
        match self {
            Self::Kbps => format_throughput_tick_kbps,
            Self::Mbps => format_throughput_tick_mbps,
            Self::Gbps => format_throughput_tick_gbps,
        }
    }
}

fn format_throughput_tick(value: f64, step: f64, scale: ThroughputScale) -> String {
    let divisor = scale.divisor();
    let scaled_step = if step.is_finite() && step > 0.0 {
        step / divisor
    } else {
        1.0
    };
    let decimals = if scaled_step < 1.0 {
        (-scaled_step.log10()).ceil().clamp(0.0, 3.0) as usize
    } else {
        0
    };
    crate::i18n::number_text(format!("{:.decimals$}", value / divisor))
}

fn format_throughput_tick_kbps(value: f64, step: f64) -> String {
    format_throughput_tick(value, step, ThroughputScale::Kbps)
}

fn format_throughput_tick_mbps(value: f64, step: f64) -> String {
    format_throughput_tick(value, step, ThroughputScale::Mbps)
}

fn format_throughput_tick_gbps(value: f64, step: f64) -> String {
    format_throughput_tick(value, step, ThroughputScale::Gbps)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiskRateScale {
    KiB,
    MiB,
    GiB,
}

impl DiskRateScale {
    fn for_domain_max(domain_max_kib: f64) -> Self {
        if domain_max_kib >= 1024.0 * 1024.0 {
            Self::GiB
        } else if domain_max_kib >= 1024.0 {
            Self::MiB
        } else {
            Self::KiB
        }
    }

    fn divisor(self) -> f64 {
        match self {
            Self::KiB => 1.0,
            Self::MiB => 1024.0,
            Self::GiB => 1024.0 * 1024.0,
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Self::KiB => "KiB/s",
            Self::MiB => "MiB/s",
            Self::GiB => "GiB/s",
        }
    }

    fn formatter(self) -> fn(f64, f64) -> String {
        match self {
            Self::KiB => format_disk_tick_kib,
            Self::MiB => format_disk_tick_mib,
            Self::GiB => format_disk_tick_gib,
        }
    }
}

fn format_disk_tick(value: f64, step: f64, scale: DiskRateScale) -> String {
    let divisor = scale.divisor();
    let scaled_step = if step.is_finite() && step > 0.0 {
        step / divisor
    } else {
        1.0
    };
    let decimals = if scaled_step < 1.0 {
        (-scaled_step.log10()).ceil().clamp(0.0, 3.0) as usize
    } else {
        0
    };
    crate::i18n::number_text(format!("{:.decimals$}", value / divisor))
}

fn format_disk_tick_kib(value: f64, step: f64) -> String {
    format_disk_tick(value, step, DiskRateScale::KiB)
}

fn format_disk_tick_mib(value: f64, step: f64) -> String {
    format_disk_tick(value, step, DiskRateScale::MiB)
}

fn format_disk_tick_gib(value: f64, step: f64) -> String {
    format_disk_tick(value, step, DiskRateScale::GiB)
}

fn y_gutter_for(
    domain: (f64, f64),
    formatter: fn(f64, f64) -> String,
    unit: &str,
    theme: &EnzTheme,
) -> f32 {
    use enzui_widgets::chart_geometry::{chart_label_font, label_width};
    let ticks = nice_ticks(domain.0, domain.1, Y_TICK_TARGET);
    let step = tick_step(&ticks);
    let font = chart_label_font(&theme.iced_theme());
    let size = TextSize::Caption.px();
    let widest = ticks
        .iter()
        .map(|&value| label_width(&formatter(value, step), size, font))
        .fold(label_width(unit, size, font), f32::max);
    widest.ceil() + Y_LABEL_GAP
}

pub(crate) fn disk_throughput_area_chart(
    history: &[GraphPointData],
    height: f32,
    theme: &EnzTheme,
) -> enzui_widgets::area_chart::EnzAreaChart {
    let read_points = history_component_points(history, |point| point.user);
    let write_points = history_component_points(history, |point| point.system);
    let max = f64::from(
        history
            .iter()
            .flat_map(|point| [point.user, point.system])
            .max()
            .unwrap_or(0)
            .max(1),
    );
    let (_, domain_max) = nice_extent((0.0, max * 1.1), Y_TICK_TARGET);
    let scale = DiskRateScale::for_domain_max(domain_max);
    let span_secs = history_span_secs(history);
    let summary = match history.last() {
        Some(latest) => crate::i18n::message(
            "tm-disk-summary",
            &[
                ("seconds", crate::i18n::argument(&span_secs)),
                (
                    "read",
                    crate::i18n::argument(&crate::i18n::reading(
                        &crate::services::format_disk_rate(latest.user as u64),
                    )),
                ),
                (
                    "write",
                    crate::i18n::argument(&crate::i18n::reading(
                        &crate::services::format_disk_rate(latest.system as u64),
                    )),
                ),
            ],
        ),
        None => crate::i18n::display("Disk read/write throughput unavailable; no data"),
    };
    enz_area_chart(crate::i18n::display("Disk throughput history"))
        .summary(summary)
        .y_domain(DomainPolicy::Fixed(0.0, domain_max))
        // Choose the axis unit and tick divisor from the same domain scale.
        .y_axis_label(scale.unit())
        .y_tick_formatter(scale.formatter())
        .y_gutter(y_gutter_for(
            (0.0, domain_max),
            scale.formatter(),
            scale.unit(),
            theme,
        ))
        .x_tick_formatter(format_seconds_tick)
        .width(Length::Fill)
        .height(height)
        .push(Series::from_points(SeriesId(0), read_points))
        .push(Series::from_points(SeriesId(1), write_points))
}

pub(crate) fn throughput_area_chart(
    name: &str,
    history: &[GraphPointData],
    height: f32,
    theme: &EnzTheme,
) -> enzui_widgets::area_chart::EnzAreaChart {
    let points = history_points(history);
    let latest = history.last().map(|point| point.user);
    let max = f64::from(
        history
            .iter()
            .map(|point| point.user + point.system)
            .max()
            .unwrap_or(0)
            .max(1),
    );
    let span_secs = history_span_secs(history);
    let (_, y_domain_max) = nice_extent((0.0, max * 1.1), Y_TICK_TARGET);
    let scale = ThroughputScale::for_domain_max(y_domain_max);
    enz_area_chart(crate::i18n::message(
        "tm-name-throughput-history-3348d3",
        &[("arg0", crate::i18n::argument(name))],
    ))
    .summary(match latest {
        Some(latest) => crate::i18n::message(
            "tm-throughput-summary",
            &[
                ("name", crate::i18n::argument(name)),
                ("seconds", crate::i18n::argument(&span_secs)),
                (
                    "rate",
                    crate::i18n::argument(&crate::i18n::reading(&crate::services::format_rate(
                        latest as u64,
                    ))),
                ),
            ],
        ),
        None => crate::i18n::message(
            "tm-name-throughput-unavailable-no-data-fe1e6f",
            &[("arg0", crate::i18n::argument(name))],
        ),
    })
    .y_domain(DomainPolicy::Fixed(0.0, y_domain_max))
    // Choose the axis unit and tick divisor from the same domain scale.
    .y_axis_label(scale.unit())
    .y_tick_formatter(scale.formatter())
    .y_gutter(y_gutter_for(
        (0.0, y_domain_max),
        scale.formatter(),
        scale.unit(),
        theme,
    ))
    .x_tick_formatter(format_seconds_tick)
    .width(Length::Fill)
    .height(height)
    .push(Series::from_points(SeriesId(0), points))
}

pub(crate) fn history_area_chart(
    name: &str,
    history: &[GraphPointData],
    height: f32,
    theme: &EnzTheme,
) -> enzui_widgets::area_chart::EnzAreaChart {
    let points = history_points(history);
    let series = Series::from_points(SeriesId(0), points);
    let latest = history.last().map(|p| p.user + p.system);

    enz_area_chart(crate::i18n::message(
        "tm-name-history-e2095d",
        &[("arg0", crate::i18n::argument(&crate::i18n::display(name)))],
    ))
    .summary(chart_summary(name, history_span_secs(history), latest))
    .y_domain(DomainPolicy::Fixed(0.0, 100.0))
    .y_tick_formatter(format_percent_tick)
    .y_gutter(y_gutter_for((0.0, 100.0), format_percent_tick, "", theme))
    .x_tick_formatter(format_seconds_tick)
    .width(Length::Fill)
    .height(height)
    .push(series)
}

pub(super) fn live_disk_history_chart<'a>(
    history: &[GraphPointData],
    height: f32,
    theme: &EnzTheme,
    epoch: std::time::Instant,
    live: bool,
) -> Element<'a, Message> {
    let colors = ChartPalette::from_palette(&theme.palette);
    let legend = enz_chart_legend([
        ChartLegendEntry::new(crate::i18n::display("Read"), colors.series_color(0)),
        ChartLegendEntry::new(crate::i18n::display("Write"), colors.series_color(1)),
    ])
    .wrap(false);
    column![
        legend,
        super::live_chart::live_chart(
            disk_throughput_area_chart(history, (height - 24.0).max(120.0), theme),
            history,
            epoch,
            live
        )
    ]
    .spacing(8.0)
    .into()
}
