//! Virtualized process and service tables with row actions and sortable headers.

use enzui_core::Action;
use enzui_core::table::ColumnKey;
use enzui_core::virtual_sequence::{HeightModel, VariableHeightModel};
use enzui_theme::{ContrastFloor, EnzTheme};
use enzui_widgets::context_target::enz_context_target;
use enzui_widgets::enz_table::enz_table;
use enzui_widgets::enz_text::{TextSize, TextTone, enz_text};
use enzui_widgets::status_dot::{StatusDotTokens, StatusKind, enz_status_dot};
use enzui_widgets::status_page::{StatusPageVariant, enz_status_page};
use enzui_widgets::table_header_cell::{EnzTableHeaderCellAlign, enz_table_header_cell};
use iced::widget::{column, container, row};
use iced::{Alignment, Color, Element, Length};

use crate::layout::{TABLE_HEADER_HEIGHT, TABLE_ROW_HEIGHT};
use crate::message::Message;
use crate::model::{MenuTarget, Model, Page, SortState};
use crate::table::{Cell, Column, RowShape, StatusTone};

/// Build the table for the active page.
pub(crate) fn view<'a>(model: &'a Model, theme: &'a EnzTheme) -> Element<'a, Message> {
    enzui_widgets::responsive::enz_responsive(move |size| {
        let mut columns = model.table_column_widths();
        let total = columns.iter().sum::<f32>().max(1.0);
        for width in &mut columns {
            *width *= size.width.max(200.0) / total;
        }
        match model.active {
            Page::Services => services(model, theme, columns),
            _ => processes(model, theme, columns),
        }
    })
    .into()
}

fn processes<'a>(model: &'a Model, theme: &EnzTheme, columns: Vec<f32>) -> Element<'a, Message> {
    // Gated on the raw snapshot, so a search that filters everything out is an
    // empty result rather than loading.
    if model.processes_warming() {
        return loading_state(
            "Loading processes",
            "The process list is warming in the background.",
            theme,
        );
    }
    let provider = model.process_table();
    let rows = provider.rows().len();
    if rows == 0 && !model.query.trim().is_empty() {
        return no_matches_state(&model.query, theme);
    }
    let sort = model.process_sort.clone();
    let palette = theme.palette.clone();
    let sizing = theme.sizing;
    let spacing = theme.spacing;
    let typo = theme.typography;
    let selected = model.selected_pid.clone();
    let selected_start_time_ticks = model.selected_process_start_time_ticks;
    let row_selected = selected.clone();

    let row_geometry = HeightModel::new(rows, TABLE_ROW_HEIGHT);
    let column_geometry = VariableHeightModel::new(columns);

    let table = enz_table(
        provider,
        row_geometry,
        column_geometry,
        move |row, _column, cell: &Cell| {
            let Some(process) = provider.process(row as usize) else {
                // An application group is not a process: it has no context
                // menu, and its row activation folds it.
                return container(cell_element(
                    cell, &palette, &sizing, &spacing, &typo, false,
                ))
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
            };
            let target = process.target();
            let is_selected = !selected.is_empty()
                && target.pid == selected
                && target.start_time_ticks == selected_start_time_ticks;
            let content = cell_element(cell, &palette, &sizing, &spacing, &typo, is_selected);
            let content = if provider.is_process_boundary(row as usize) {
                column![
                    enzui_widgets::enz_divider(),
                    container(content).height(Length::Fill)
                ]
                .height(Length::Fill)
                .into()
            } else {
                content
            };
            interactive_cell(content, MenuTarget::Process(target))
        },
    )
    // A collapsed group carries its selected member's band; expanded groups
    // leave it on the member.
    .row_selection(move |row| {
        let is_selected = |process: &crate::services::ProcessData| {
            !row_selected.is_empty()
                && process.pid == row_selected
                && process.start_time_ticks == selected_start_time_ticks
        };
        match provider.shape(row as usize) {
            Some(RowShape::Group {
                members,
                expanded: false,
                ..
            }) => members.iter().any(is_selected),
            Some(RowShape::Group { .. }) | None => false,
            Some(RowShape::Process { .. }) => provider.row(row as usize).is_some_and(is_selected),
        }
    })
    .on_row_activate(move |row| {
        if let Some(RowShape::Group {
            name,
            members,
            expanded,
        }) = provider.shape(row as usize)
        {
            let verb = if *expanded { "Collapse" } else { "Expand" };
            return (
                crate::i18n::message(
                    "tm-group-process-count",
                    &[
                        ("arg0", crate::i18n::argument(&crate::i18n::display(verb))),
                        ("arg1", crate::i18n::argument(&(name))),
                        ("arg2", crate::i18n::argument(&(members.len()))),
                    ],
                ),
                Message::ToggleAppGroup(name.clone()),
            );
        }
        provider.process(row as usize).map_or_else(
            || {
                (
                    "Select process".to_owned(),
                    Message::SelectProcess(crate::services::ProcessTarget::default()),
                )
            },
            |process| {
                (
                    crate::i18n::message(
                        "tm-select-process-c11461",
                        &[("arg0", crate::i18n::argument(&(process.name)))],
                    ),
                    Message::SelectProcess(process.target()),
                )
            },
        )
    })
    .header({
        let page = model.active;
        move |index, key| header_cell(index, key, &sort, model.process_columns(), true, page)
    })
    .header_height(TABLE_HEADER_HEIGHT)
    .header_gap(theme.spacing.space2)
    .width(Length::Fill)
    .height(Length::Fill)
    .viewport_state(model.process_table_viewport())
    .scroll_y(model.table_scroll)
    .row_overscan(2)
    // Pass the raw delta so a burst through one unchanged view integrates
    // against the latest model offset in the reducer.
    .on_scroll(move |_dx, dy| Message::TableWheelScrolled(dy))
    .on_scroll_to(Message::TableScrolled);

    let table: Element<'a, Message> = table.into();

    table
}

fn no_matches_state<'a>(query: &str, theme: &EnzTheme) -> Element<'a, Message> {
    let hero = enz_status_page(crate::i18n::display(crate::i18n::message(
        "tm-no-matches-for-query-5c7251",
        &[("arg0", crate::i18n::argument(&(query)))],
    )))
    .icon("search")
    .status_text(crate::i18n::display(
        "Try a different query, or clear the search to see every row.",
    ))
    .variant(StatusPageVariant::Inline)
    .primary_action(
        Action::new("tm.search.clear", Message::SearchChanged(String::new()))
            .with_label(crate::i18n::display("Clear search")),
    )
    .compact(true);
    let _ = theme; // theme currently unused; status_page reads it from the tree
    container(hero)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}

fn loading_state<'a>(label: &str, caption: &str, theme: &EnzTheme) -> Element<'a, Message> {
    use enzui_widgets::spinner::enz_spinner;
    let hero = column![
        enz_spinner().into_element(),
        enz_text(crate::i18n::display(label))
            .size(TextSize::Title3)
            .tone(TextTone::Default),
        enz_text(crate::i18n::display(caption))
            .size(TextSize::Caption)
            .tone(TextTone::Muted),
    ]
    .spacing(theme.spacing.space5)
    .align_x(Alignment::Center);
    container(hero)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .into()
}

fn services<'a>(model: &'a Model, theme: &EnzTheme, columns: Vec<f32>) -> Element<'a, Message> {
    if model.services_warming() {
        return loading_state(
            "Loading services",
            "The service list is warming in the background.",
            theme,
        );
    }
    let provider = model.service_table();
    let rows = provider.rows().len();
    if rows == 0 && !model.query.trim().is_empty() {
        return no_matches_state(&model.query, theme);
    }
    let sort = model.service_sort.clone();
    let palette = theme.palette.clone();
    let sizing = theme.sizing;
    let spacing = theme.spacing;
    let typo = theme.typography;
    let selected = model.selected_service.clone();
    let row_selected = selected.clone();

    let row_geometry = HeightModel::new(rows, TABLE_ROW_HEIGHT);
    let column_geometry = VariableHeightModel::new(columns);

    let table = enz_table(
        provider,
        row_geometry,
        column_geometry,
        move |row, _column, cell: &Cell| {
            let unit = provider
                .row(row as usize)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            let is_selected = !selected.is_empty() && unit == selected;
            interactive_cell(
                cell_element(cell, &palette, &sizing, &spacing, &typo, is_selected),
                MenuTarget::Service(unit),
            )
        },
    )
    // Stable unit name, not a row index: service sorting cannot detach the
    // selected band from the selected service.
    .row_selection(move |row| {
        provider
            .row(row as usize)
            .is_some_and(|service| !row_selected.is_empty() && service.name == row_selected)
    })
    .on_row_activate(move |row| {
        provider.row(row as usize).map_or_else(
            || {
                (
                    "Select service".to_owned(),
                    Message::SelectService(String::new()),
                )
            },
            |service| {
                (
                    crate::i18n::message(
                        "tm-select-service-691f05",
                        &[("arg0", crate::i18n::argument(&(service.name)))],
                    ),
                    Message::SelectService(service.name.clone()),
                )
            },
        )
    })
    .header({
        let page = model.active;
        move |index, key| header_cell(index, key, &sort, model.service_columns(), false, page)
    })
    .header_height(TABLE_HEADER_HEIGHT)
    .header_gap(theme.spacing.space2)
    .width(Length::Fill)
    .height(Length::Fill)
    .viewport_state(model.service_table_viewport())
    .scroll_y(model.table_scroll)
    .row_overscan(2)
    // Wheel input is owned by `EnzTable`; its normalized delta is kept for
    // reducer-time accumulation. Drag remains an absolute offset.
    .on_scroll(move |_dx, dy| Message::TableWheelScrolled(dy))
    .on_scroll_to(Message::TableScrolled);

    let table: Element<'a, Message> = table.into();

    table
}

fn interactive_cell<'a>(content: Element<'a, Message>, target: MenuTarget) -> Element<'a, Message> {
    // Row surfaces own left-click and hover; cell wrappers must not duplicate them.
    enz_context_target(content, move |at| {
        Message::OpenContextMenu(target.clone(), at)
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn header_cell<'a>(
    _index: usize,
    key: &ColumnKey,
    sort: &SortState,
    columns: Vec<&'static Column>,
    processes: bool,
    page: Page,
) -> Element<'a, Message> {
    let key_str = key.as_str().to_owned();
    let label: String = if processes && key_str == "name" {
        name_header_label(page).to_owned()
    } else {
        columns
            .iter()
            .find(|c| c.key == key_str)
            .map_or("", |c| c.label)
            .to_owned()
    };
    let active = sort.key == key_str;
    let message = if processes {
        Message::SortProcesses(key_str.clone())
    } else {
        Message::SortServices(key_str.clone())
    };

    let align = if is_numeric(&key_str) {
        EnzTableHeaderCellAlign::Right
    } else {
        EnzTableHeaderCellAlign::Left
    };

    enz_table_header_cell(crate::i18n::display(label))
        .align(align)
        .sortable(true)
        .sort_active(active)
        .sort_ascending(sort.ascending)
        .header_height(TABLE_HEADER_HEIGHT)
        .cell_padding_x(8.0)
        // Both tables have a Name column; separate prefixes prevent sort-button ID collisions.
        .id_prefix(if processes {
            "tm-process-header"
        } else {
            "tm-service-header"
        })
        .on_sort(message)
        .into()
}

/// Return the Name column label for either process page.
pub(crate) fn name_header_label(_page: Page) -> &'static str {
    "Name"
}

const GROUP_CHEVRON_SIZE: f32 = 20.0;

fn is_numeric(key: &str) -> bool {
    matches!(key, "cpu" | "rss" | "mem_percent" | "pid")
}

const CELL_LEADING_INSET: f32 = 8.0;

fn status_gutter_width(
    sizing: &enzui_theme::SizingTokens,
    spacing: &enzui_theme::SpacingScale,
) -> f32 {
    sizing.status_dot + 2.0 * spacing.space1
}

fn cell_element<'a>(
    cell: &Cell,
    palette: &enzui_theme::Palette,
    sizing: &enzui_theme::SizingTokens,
    spacing: &enzui_theme::SpacingScale,
    _typo: &enzui_theme::TypographyScale,
    selected: bool,
) -> Element<'a, Message> {
    // Rows are a fixed `TABLE_ROW_HEIGHT` and clip, so every cell is
    // `.single_line_elided()`: one line, an end ellipsis, and an elision probe.
    let inner: Element<'a, Message> = match cell {
        Cell::Text(value) => enz_text(value.clone())
            .size(TextSize::Footnote)
            .single_line_elided()
            .width(Length::Fill)
            .into(),
        Cell::Numeric(value) => enz_text(crate::i18n::display(value.clone()))
            .size(TextSize::Footnote)
            .tabular_figures(true)
            .single_line_elided()
            .width(Length::Fill)
            .align(enzui_widgets::enz_text::TextAlign::Right)
            .into(),
        Cell::Name {
            label,
            protected,
            app,
            indent,
        } => {
            let tone = if *protected {
                TextTone::Muted
            } else {
                TextTone::Default
            };
            let name = enz_text(label.clone())
                .size(TextSize::Caption)
                .tone(tone)
                .single_line_elided()
                .width(Length::Fill);
            let named: Element<'a, Message> = if *app {
                let icon = super::app_icon::app_icon_element(
                    super::app_icon::app_icon(label),
                    super::app_icon::APP_ICON_SIZE,
                );
                row![icon, name]
                    .mirrored(crate::i18n::rtl())
                    .spacing(spacing.space4)
                    .align_y(Alignment::Center)
                    .into()
            } else {
                name.into()
            };
            if *indent > 0 {
                // Beside a group a name keeps the chevron's gutter, so every
                // top-level icon lines up; under one it steps in once more.
                let step = GROUP_CHEVRON_SIZE + spacing.space4;
                container(named)
                    .padding(iced::Padding::ZERO.left(step * f32::from(*indent)))
                    .width(Length::Fill)
                    .into()
            } else {
                named
            }
        }
        Cell::Group {
            label,
            count,
            expanded,
        } => {
            let chevron = enzui_widgets::tree_disclosure::enz_tree_disclosure(
                *expanded,
                Message::ToggleAppGroup(label.clone()),
            )
            .button_size(GROUP_CHEVRON_SIZE)
            .row_height(GROUP_CHEVRON_SIZE)
            // The row publishes the expand action and its name; a second
            // disclosure node would announce the group twice.
            .embedded(true);
            let icon = super::app_icon::app_icon_element(
                super::app_icon::app_icon(label),
                super::app_icon::APP_ICON_SIZE,
            );
            let name = enz_text(label.clone())
                .size(TextSize::Caption)
                .single_line_elided();
            let count = enz_text(crate::i18n::display(format!(
                "({})",
                crate::i18n::number_text(count.to_string())
            )))
            .size(TextSize::Caption)
            .tone(TextTone::Muted);
            row![
                chevron,
                icon,
                row![name, count]
                    .mirrored(crate::i18n::rtl())
                    .spacing(spacing.space2)
            ]
            .mirrored(crate::i18n::rtl())
            .spacing(spacing.space4)
            .align_y(Alignment::Center)
            .into()
        }
        Cell::Status { label, tone } => {
            let (kind, dot_tokens) = status_dot_for(*tone, palette, selected);
            let dot = enz_status_dot(kind)
                .geometry(enzui_widgets::status_dot::StatusDotGeometry::from_scales(
                    sizing,
                ))
                .tokens(dot_tokens)
                .into_element();
            row![
                container(dot)
                    .width(Length::Fixed(status_gutter_width(sizing, spacing)))
                    .align_x(Alignment::Center),
                enz_text(label.clone())
                    .size(TextSize::Footnote)
                    .single_line_elided()
                    .width(Length::Fill),
            ]
            .mirrored(crate::i18n::rtl())
            .align_y(Alignment::Center)
            .into()
        }
    };

    container(inner)
        .padding(
            iced::Padding::default()
                .left(CELL_LEADING_INSET)
                .right(CELL_LEADING_INSET),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(Alignment::Center)
        // Clip so a long value ends at the column edge instead of painting
        // over its neighbour.
        .clip(true)
        .into()
}

// Contrast resolution uses the resting row surface, excluding the hover wash.
fn row_backdrop(palette: &enzui_theme::Palette, selected: bool) -> Color {
    if selected {
        palette.surface_selected
    } else {
        palette.background
    }
}

fn status_dot_for(
    tone: StatusTone,
    palette: &enzui_theme::Palette,
    selected: bool,
) -> (StatusKind, StatusDotTokens) {
    let mut tokens = StatusDotTokens::from_palette(palette);
    // Selected-row contrast needs the graphics floor; the text floor over-promotes neutral ink.
    tokens.neutral_color =
        palette.subtle_on(row_backdrop(palette, selected), ContrastFloor::NonText);
    let kind = match tone {
        StatusTone::Running => StatusKind::Success,
        StatusTone::Failed => StatusKind::Error,
        StatusTone::Stopped => StatusKind::Neutral,
    };
    (kind, tokens)
}
