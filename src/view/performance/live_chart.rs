use super::*;
use enzui_widgets::area_chart::{ChartCurve, EnzAreaChart};
use std::time::Instant;

pub(super) fn live_chart<'a>(
    chart: EnzAreaChart,
    history: &[GraphPointData],
    epoch: Instant,
    live: bool,
) -> Element<'a, Message> {
    let sampled_at = history.last().map_or(epoch, |point| epoch + point.at);
    let chart = chart.curve(ChartCurve::Smooth);
    if live {
        chart
            .live(crate::model::HISTORY_WINDOW, sampled_at)
            .into_element()
    } else {
        chart
            .x_domain(DomainPolicy::Fixed(
                -crate::model::HISTORY_WINDOW.as_secs_f64(),
                0.0,
            ))
            .into_element()
    }
}
