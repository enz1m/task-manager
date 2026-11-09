//! Responsive row counts and geometry budgets for tables and Performance charts.
//!
//! Window-width rungs use the default sizing scale. Search collapse is measured
//! independently by its titlebar widget. Performance budgets combine row-box
//! estimates with the supplied spacing scale; tables publish their vertical viewport.

use iced::Size;

/// Window-width threshold for compact columns and sidebar drawers, in logical px.
pub(crate) fn mobile_breakpoint() -> f32 {
    enzui_theme::SizingTokens::default().layout_switch_stack
}

/// Window-width threshold below which the table and inspector stack, in logical px.
pub(crate) fn tablet_breakpoint() -> f32 {
    enzui_theme::SizingTokens::default().layout_switch_compact
}

/// Width of an inspector beside the table, in logical px.
pub(crate) const INSPECTOR_WIDTH: f32 = 280.0;

/// Whether the width is below the compact threshold.
pub(crate) fn is_compact(window_width: f32) -> bool {
    window_width < mobile_breakpoint()
}

/// Whether the width is below the table/inspector stacking threshold.
pub(crate) fn should_stack(window_width: f32) -> bool {
    window_width < tablet_breakpoint()
}

/// Stat-grid column count: one at compact widths, two below the stacking threshold, otherwise three.
pub(crate) fn stats_grid_columns(window_width: f32) -> usize {
    if is_compact(window_width) {
        1
    } else if should_stack(window_width) {
        2
    } else {
        3
    }
}

/// Core-meter column count: two at compact widths, four below the stacking threshold, otherwise eight.
pub(crate) fn core_columns(window_width: f32) -> usize {
    if is_compact(window_width) {
        2
    } else if should_stack(window_width) {
        4
    } else {
        8
    }
}

/// Minimum history-chart height, in logical px; short panes scroll rather than shrinking it further.
pub(crate) const MIN_CHART_HEIGHT: f32 = 180.0;

/// Maximum history-chart height, in logical px; taller panes retain unused slack.
pub(crate) const MAX_CHART_HEIGHT: f32 = 640.0;

// TOKEN-AUDIT-EXCEPTION(dim): tallest measured flat stat-row box, in logical px.
const PERF_STAT_ROW: f32 = 28.0;

// TOKEN-AUDIT-EXCEPTION(dim): measured caption/value stack, in logical px.
const PERF_FACT_ROW: f32 = 45.0;

// TOKEN-AUDIT-EXCEPTION(dim): extra height of a copy control over a text value, in logical px.
const PERF_COPYABLE_FACT_EXTRA: f32 = 6.0;

// TOKEN-AUDIT-EXCEPTION(dim): measured core meter/label row, in logical px.
const PERF_CORE_ROW: f32 = 24.0;

// TOKEN-AUDIT-EXCEPTION(dim): matches core_grid spacing, in logical px.
const PERF_CORE_GRID_GAP: f32 = 6.0;

// TOKEN-AUDIT-EXCEPTION(dim): measured resource title/subtitle stack, in logical px.
const PERF_HEADER_ROW: f32 = 52.0;

fn rows_for(n: usize, cols: usize) -> usize {
    if n == 0 || cols == 0 {
        0
    } else {
        n.div_ceil(cols)
    }
}

/// Estimate Performance pane height after shell chrome, floored at zero, in logical px.
pub(crate) fn perf_pane_height(window: Size, spacing: &enzui_theme::SpacingScale) -> f32 {
    let shell = crate::view::shell::shell_tokens(spacing);
    (window.height - (shell.effective_edge_inset() + shell.titlebar_height + shell.page_inset))
        .max(0.0)
}

/// Estimate non-chart Performance height from row-box sizes and the supplied spacing scale.
/// Identity rows, compact-grid rows, and device facts have different heights.
pub(crate) fn perf_non_chart_height(spacing: &enzui_theme::SpacingScale, rows: PerfRows) -> f32 {
    let PerfRows {
        identity_rows,
        stat_rows,
        fact_rows,
        core_rows,
        copyable_rows,
    } = rows;
    // The shell owns the top inset; the page owns the bottom inset.
    let mut total = spacing.space9;
    let mut children = 3;
    total += PERF_HEADER_ROW;
    // Identity rows and compact-grid rows share one spacing domain.
    let stats_block_rows = identity_rows + stat_rows;
    total += identity_rows as f32 * PERF_FACT_ROW
        + stat_rows as f32 * PERF_STAT_ROW
        + stats_block_rows.saturating_sub(1) as f32 * spacing.sm();
    if fact_rows > 0 {
        children += 1;
        total += block_height(fact_rows, PERF_FACT_ROW, spacing.sm());
    }
    if core_rows > 0 {
        children += 1;
        total += enzui_widgets::text::TextSize::Body.line_height_px()
            + spacing.sm()
            + block_height(core_rows, PERF_CORE_ROW, PERF_CORE_GRID_GAP);
    }
    total += copyable_rows as f32 * PERF_COPYABLE_FACT_EXTRA;
    total + (children - 1) as f32 * spacing.lg()
}

fn block_height(rows: usize, row: f32, gap: f32) -> f32 {
    if rows == 0 {
        0.0
    } else {
        rows as f32 * row + (rows - 1) as f32 * gap
    }
}

/// Performance content counts before responsive grids convert items to rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PerfContent {
    /// Stats drawn as full-width identity rows (`Interface`, `Mount`, …).
    pub(crate) identity_stats: usize,
    /// Stats drawn as cells in the reflowing compact grid.
    pub(crate) compact_stats: usize,
    /// Device-fact rows below the stats.
    pub(crate) facts: usize,
    /// Per-logical-processor meters (0 for a resource with no core grid).
    pub(crate) cores: usize,
    /// Identity and fact rows whose copy control adds height to the text box.
    pub(crate) copyable_rows: usize,
}

/// Performance row counts after responsive grids reflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PerfRows {
    pub(crate) identity_rows: usize,
    pub(crate) stat_rows: usize,
    pub(crate) fact_rows: usize,
    pub(crate) core_rows: usize,
    pub(crate) copyable_rows: usize,
}

/// Convert content counts to rows using the window-width rungs.
/// Identity and fact rows remain full-width; compact stats and core meters reflow.
pub(crate) fn perf_rows(window_width: f32, content: PerfContent) -> PerfRows {
    PerfRows {
        identity_rows: content.identity_stats,
        stat_rows: rows_for(content.compact_stats, stats_grid_columns(window_width)),
        fact_rows: content.facts,
        core_rows: rows_for(content.cores, core_columns(window_width)),
        copyable_rows: content.copyable_rows,
    }
}

/// Use pane slack for a history chart, clamped to the minimum and maximum chart heights.
/// Returns logical px; narrow grids reduce slack by adding rows.
pub(crate) fn chart_height(
    window: Size,
    spacing: &enzui_theme::SpacingScale,
    content: PerfContent,
) -> f32 {
    let non_chart = perf_non_chart_height(spacing, perf_rows(window.width, content));
    (perf_pane_height(window, spacing) - non_chart).clamp(MIN_CHART_HEIGHT, MAX_CHART_HEIGHT)
}

/// Process and service table row height, in logical px.
pub(crate) const TABLE_ROW_HEIGHT: f32 = 28.0;

/// Process and service table header height, in logical px.
pub(crate) const TABLE_HEADER_HEIGHT: f32 = 30.0;

/// Folded inspector height, in logical px; additional fields remain scrollable.
pub(crate) const STACKED_INSPECTOR_HEIGHT: f32 = 304.0;

fn page_chrome_width() -> f32 {
    2.0 * enzui_theme::SpacingScale::default().space9
}

/// Estimate stacked inspector width after stock page insets, floored at the inspector width.
pub(crate) fn stacked_inspector_width(window_width: f32) -> f32 {
    (window_width - page_chrome_width()).max(INSPECTOR_WIDTH)
}

/// Narrowest a value column (PID, memory, state…) gets before the name column yields.
const MIN_VALUE_COLUMN_WIDTH: f32 = 88.0;
/// Narrowest the name column gets when value columns claim their minimum.
const MIN_NAME_COLUMN_WIDTH: f32 = 140.0;

/// Estimate horizontal tracks from window width, stock page insets, and inspector placement.
/// A non-positive or non-finite total weight distributes width equally.
pub(crate) fn table_column_widths(
    window: Size,
    weights: &[f32],
    inspector_beside: bool,
) -> Vec<f32> {
    let inspector = if inspector_beside {
        INSPECTOR_WIDTH + 12.0
    } else {
        0.0
    };
    let width = (window.width - inspector - page_chrome_width()).max(200.0);
    let total: f32 = weights.iter().sum();
    if total > 0.0 && total.is_finite() {
        let mut widths: Vec<f32> = weights
            .iter()
            .map(|weight| weight / total * width)
            .collect();
        // Value columns keep room for a PID, "3,403.0 MiB" or "Sleeping"; the
        // widest (name) column gives up the difference. Pure proportions cut
        // them to "414…" at 900 px.
        let primary = weights
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map_or(0, |(i, _)| i);
        let mut deficit = 0.0;
        for (i, w) in widths.iter_mut().enumerate() {
            if i != primary && *w < MIN_VALUE_COLUMN_WIDTH {
                deficit += MIN_VALUE_COLUMN_WIDTH - *w;
                *w = MIN_VALUE_COLUMN_WIDTH;
            }
        }
        if let Some(w) = widths.get_mut(primary) {
            *w = (*w - deficit).max(MIN_NAME_COLUMN_WIDTH);
        }
        widths
    } else {
        let count = weights.len().max(1) as f32;
        weights.iter().map(|_| width / count).collect()
    }
}
