//! Theme-derived helpers. Shared source selection lives in `ThemeIntegration`.

use enzui_theme::EnzTheme;

pub(crate) fn instant_sample_theme(theme: &EnzTheme) -> iced::Theme {
    let mut chart_theme = theme.clone();
    chart_theme.motion.normal_ms = 0.0;
    chart_theme.iced_theme()
}
