//! Process and service details with copyable fields and action controls.
//!
//! Process termination and service mutations request confirmation.

use enzui_theme::{EnzTheme, SpaceToken};
use enzui_widgets::enz_text::{TextSize, TextTone, enz_text};
use enzui_widgets::form_layout::enz_form_layout;
use enzui_widgets::grid::enz_grid;
use enzui_widgets::labeled_content::{LabeledContentLayout, enz_labeled_content};
use enzui_widgets::status_page::{StatusPageVariant, enz_status_page};
use enzui_widgets::surface::{BorderStyle, SurfaceTone, enz_surface};
use enzui_widgets::view_that_fits::enz_view_that_fits;
use enzui_widgets::{ButtonKind, enz_button};
use iced::widget::{column, container, row};
use iced::{Alignment, Element, Length};

use crate::message::Message;
use crate::model::{Model, Page, PendingAction};

/// Show the active inspector, or a selection prompt when no item is selected.
pub(crate) fn view<'a>(model: &Model, theme: &EnzTheme) -> Element<'a, Message> {
    match model.active {
        Page::Services => service_inspector(model, theme),
        _ => process_inspector(model, theme),
    }
}

fn process_inspector<'a>(model: &Model, theme: &EnzTheme) -> Element<'a, Message> {
    let p = model.selected_process();
    if p.pid.is_empty() {
        return empty("Select a process to inspect it", model, theme);
    }

    let typo = theme.typography;
    let sizing = theme.sizing;
    let sp = &theme.spacing;
    // Values are focusable copy actions, so fields the compact table folds away
    // stay reachable in the accessibility tree.
    let memory_share = format!("{}%", p.memory.trim_end_matches('%'));
    let [threads, open_files, cwd] = model.process_details.labels(&p.target());
    let fields = enz_form_layout(vec![
        labelled_action_value("PID", Field::Pid, &p.pid, typo, sizing, *sp),
        labelled_action_value("CPU", Field::Cpu, &p.cpu_display(), typo, sizing, *sp),
        memory_fields(p.rss.clone(), memory_share, typo, sizing, *sp),
        labelled_action_value("User", Field::User, &p.user, typo, sizing, *sp),
        labelled_action_value("State", Field::State, &p.state, typo, sizing, *sp),
        labelled_action_value("Type", Field::Kind, kind_label(&p.kind), typo, sizing, *sp),
        labelled_action_value(
            "Command line",
            Field::CommandLine,
            &p.command_line,
            typo,
            sizing,
            *sp,
        ),
        labelled_action_value("Threads", Field::Threads, &threads, typo, sizing, *sp),
        labelled_action_value(
            "Open files",
            Field::OpenFiles,
            &open_files,
            typo,
            sizing,
            *sp,
        ),
        labelled_action_value(
            "Working directory",
            Field::WorkingDirectory,
            &cwd,
            typo,
            sizing,
            *sp,
        ),
    ])
    .spacing_scale(theme.spacing)
    .row_gap(FIELD_GAP)
    .inset(SpaceToken::None);

    let heading = if p.kind == "App" {
        let icon = super::app_icon::app_icon_element(
            super::app_icon::app_icon(&p.name),
            super::app_icon::HEADING_ICON_SIZE,
        );
        row![icon, title(&p.name)]
            .mirrored(crate::i18n::rtl())
            .spacing(sp.space4)
            .align_y(Alignment::Center)
            .into()
    } else {
        title(&p.name)
    };
    let body = column![Element::from(fields)];

    // The note is pinned beside the actions, outside both the scrolling body
    // and the footer band. An other-user row names its owner; other refusals
    // keep the generic note.
    let note = (!p.action_allowed).then(|| {
        p.action_block_note(&crate::services::current_user())
            .unwrap_or_else(|| "End/force kill unavailable for this process".to_owned())
    });

    let actions = action_band(
        vec![
            enz_button(
                crate::i18n::display("End"),
                Message::RequestAction(PendingAction::EndProcess {
                    pid: p.pid.clone(),
                    start_time_ticks: p.start_time_ticks,
                    kernel_uid: p.kernel_uid,
                    name: p.name.clone(),
                }),
            )
            .kind(ButtonKind::Secondary)
            .id(iced::advanced::widget::Id::new("task-manager-process-end"))
            .enabled(p.action_allowed),
            enz_button(
                crate::i18n::display("Force kill"),
                Message::RequestAction(PendingAction::KillProcess {
                    pid: p.pid.clone(),
                    start_time_ticks: p.start_time_ticks,
                    kernel_uid: p.kernel_uid,
                    name: p.name.clone(),
                }),
            )
            .kind(ButtonKind::Destructive)
            .aria_label(crate::i18n::display("Force kill process"))
            .id(iced::advanced::widget::Id::new("task-manager-process-kill"))
            .enabled(p.action_allowed),
        ],
        sp,
    );

    inspector_panel(heading, body, Some(actions), note, model, theme)
}

fn service_inspector<'a>(model: &Model, theme: &EnzTheme) -> Element<'a, Message> {
    let s = model.selected_service_data();
    if s.name.is_empty() || s.name == "No service selected" {
        return empty("Select a service to inspect it", model, theme);
    }

    let typo = theme.typography;
    let sizing = theme.sizing;
    let sp = &theme.spacing;
    let status = s.status_text();
    let usage = usage_line(&s.cpu, &s.memory);
    let mut rows: Vec<Element<'a, Message>> = vec![
        labelled_action_value("Status", Field::Status, &status, typo, sizing, *sp),
        // Compact Services keeps State but drops the separate Sub-state
        // column, so the exact table value (for example `running`) stays
        // reachable as its own folded field.
        labelled_action_value(
            "Sub-state",
            Field::SubState,
            &s.sub_state,
            typo,
            sizing,
            *sp,
        ),
        labelled_action_value("Load", Field::Load, &s.load, typo, sizing, *sp),
        labelled_action_value("Usage", Field::Usage, &usage, typo, sizing, *sp),
    ];

    if !s.description.is_empty() {
        rows.push(labelled_action_value(
            "Description",
            Field::Description,
            &s.description,
            typo,
            sizing,
            *sp,
        ));
    }

    if !s.fragment_path.is_empty() {
        rows.push(labelled_action_value(
            "Unit file",
            Field::UnitFile,
            &s.fragment_path,
            typo,
            sizing,
            *sp,
        ));
    }

    let fields = enz_form_layout(rows)
        .spacing_scale(theme.spacing)
        .row_gap(FIELD_GAP)
        .inset(SpaceToken::None);

    let body = column![Element::from(fields)];

    let note = (!s.action_allowed).then(|| "Protected service: actions disabled".to_owned());

    // Offer what the unit's state allows: Start for a stopped or failed unit,
    // Stop and Restart for a running one (an active red Stop on a dead unit
    // was the only enabled-looking control on the row).
    let running = s.is_running();
    let actions = service_action_grid(
        vec![
            enz_button(
                crate::i18n::display("Start"),
                Message::RunServiceAction(s.name.clone(), "start"),
            )
            .kind(ButtonKind::Secondary)
            .aria_label(crate::i18n::display("Start service"))
            .id(iced::advanced::widget::Id::new(
                "task-manager-service-start",
            ))
            .enabled(s.action_allowed && !running),
            enz_button(
                crate::i18n::display("Restart"),
                Message::RunServiceAction(s.name.clone(), "restart"),
            )
            .kind(ButtonKind::Secondary)
            .aria_label(crate::i18n::display("Restart service"))
            .id(iced::advanced::widget::Id::new(
                "task-manager-service-restart",
            ))
            .enabled(s.action_allowed && running),
            enz_button(
                crate::i18n::display("Stop"),
                Message::RequestAction(PendingAction::StopService {
                    unit: s.name.clone(),
                }),
            )
            .kind(ButtonKind::Destructive)
            .aria_label(crate::i18n::display("Stop service"))
            .id(iced::advanced::widget::Id::new("task-manager-service-stop"))
            .enabled(s.action_allowed && running),
            enz_button(
                crate::i18n::display("Logs…"),
                Message::OpenLogs(s.name.clone()),
            )
            .kind(ButtonKind::Secondary)
            .aria_label(crate::i18n::display("Show recent journal entries"))
            .id(crate::logs_button_id()),
        ],
        sp,
    );

    inspector_panel(title(&s.name), body, Some(actions), note, model, theme)
}

fn action_band<'a>(
    buttons: Vec<enzui_widgets::EnzButton<'a, Message>>,
    sp: &enzui_theme::SpacingScale,
) -> Element<'a, Message> {
    let mut r = row![]
        .mirrored(crate::i18n::rtl())
        .spacing(sp.sm())
        .width(Length::Fill);
    for b in buttons {
        r = r.push(b.width(Length::Fill).into_element());
    }
    r.into()
}

fn service_action_grid<'a>(
    buttons: Vec<enzui_widgets::EnzButton<'a, Message>>,
    sp: &enzui_theme::SpacingScale,
) -> Element<'a, Message> {
    let cells: Vec<Element<'a, Message>> = buttons
        .into_iter()
        .map(|b| b.width(Length::Fill).into_element())
        .collect();
    enz_grid(2, cells).gap(sp.sm()).stretch_cells(true).into()
}

fn usage_line(cpu: &str, memory: &str) -> String {
    if cpu.trim().is_empty() && memory.trim().is_empty() {
        return "unavailable".to_owned();
    }
    let cpu = if cpu.trim().is_empty() { "—" } else { cpu };
    let memory = if memory.trim().is_empty() {
        "—"
    } else {
        memory
    };
    crate::i18n::message(
        "tm-cpu-memory-df719d",
        &[
            ("arg0", crate::i18n::argument(&crate::i18n::reading(cpu))),
            ("arg1", crate::i18n::argument(&crate::i18n::reading(memory))),
        ],
    )
}

fn inspector_panel<'a>(
    heading: Element<'a, Message>,
    body: iced::widget::Column<'a, Message>,
    actions: Option<Element<'a, Message>>,
    note: Option<String>,
    model: &Model,
    theme: &EnzTheme,
) -> Element<'a, Message> {
    use enzui_core::semantics::{Role, Semantics};
    use enzui_widgets::scroll_column::enz_scroll_column;
    use enzui_widgets::semantics_container::SemanticsContainer;
    let width = if model.inspector_beside() {
        crate::layout::INSPECTOR_WIDTH
    } else {
        crate::layout::stacked_inspector_width(model.window.width)
    };
    let inset_body = container(body.width(Length::Fill))
        .padding(iced::Padding {
            top: 8.0,
            right: 8.0,
            bottom: 0.0,
            left: 8.0,
        })
        .width(Length::Fill);
    let scroll: Element<'a, Message> = enz_scroll_column(vec![inset_body.into()])
        .spacing(0.0)
        .complete_rows(TextSize::Body.line_height_px())
        .into();
    let close = enzui_widgets::toolbar_icon::enz_toolbar_icon("x", Message::CloseInspector)
        .label(crate::i18n::display("Close details"))
        .id(iced::advanced::widget::Id::new(
            "task-manager-inspector-close",
        ))
        .size(28.0, 16.0);
    let header = container(
        row![container(heading).width(Length::Fill), close]
            .mirrored(crate::i18n::rtl())
            .spacing(8.0)
            .align_y(Alignment::Center),
    )
    .padding([8.0, 8.0])
    .width(Length::Fill);
    let mut content = column![header, scroll].spacing(0.0);
    if let Some(note) = note {
        content = content.push(
            container(
                enz_text(crate::i18n::display(note))
                    .size(TextSize::Caption)
                    .tone(TextTone::Warning),
            )
            .padding(iced::Padding {
                top: 0.0,
                right: 8.0,
                bottom: 10.0,
                left: 8.0,
            })
            .width(Length::Fill),
        );
    }
    let radius = theme.radius.step(theme.shape.card);
    if let Some(actions) = actions {
        let footer = enz_surface(actions)
            .tone(SurfaceTone::Elevated)
            .border_style(BorderStyle::None)
            .border_width(0.0)
            .radius_corners([0.0, 0.0, radius, radius])
            .padding([10.0, 8.0])
            .width(Length::Fill);
        content = content.push(footer);
    }
    let panel: Element<'a, Message> = enz_surface(content.height(Length::Fill))
        .tone(SurfaceTone::SurfaceAlt)
        .border_style(BorderStyle::Thin)
        .border_width(1.0)
        .radius(radius)
        .padding(0)
        .width(Length::Fixed(width))
        .height(Length::Fill)
        .clip(true)
        .into();
    SemanticsContainer::new(
        panel,
        Semantics::new(Role::Group).label(crate::i18n::display("Inspector")),
    )
    .into()
}

pub(crate) fn toggle_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("task-manager.inspector.toggle")
}

fn empty<'a>(prompt: &str, model: &Model, theme: &EnzTheme) -> Element<'a, Message> {
    let hero = enz_status_page(crate::i18n::display(prompt))
        .icon("mouse-pointer-click")
        .variant(StatusPageVariant::Inline)
        .compact(true);
    inspector_panel(
        title(&crate::i18n::display("Details")),
        column![hero],
        None,
        None,
        model,
        theme,
    )
}

fn title<'a>(value: &str) -> Element<'a, Message> {
    enz_text(value.to_owned())
        .size(TextSize::Title3)
        .single_line_elided()
        .width(Length::Fill)
        .tone(TextTone::Default)
        .into()
}

fn inspector_value_id(field: Field) -> iced::advanced::widget::Id {
    // Sampled values change; focus identity must stay tied to the field.
    iced::advanced::widget::Id::new(field.key())
}

pub(crate) fn memory_fields<'a>(
    memory: String,
    memory_share: String,
    typography: enzui_theme::TypographyScale,
    sizing: enzui_theme::SizingTokens,
    spacing: enzui_theme::SpacingScale,
) -> Element<'a, Message> {
    let row_memory = memory.clone();
    let row_share = memory_share.clone();
    enz_view_that_fits(vec![
        Box::new(move || {
            row![
                container(labelled_action_value(
                    "Memory",
                    Field::Memory,
                    &row_memory,
                    typography,
                    sizing,
                    spacing,
                ))
                .width(MEMORY_FIELD_WIDTH),
                container(labelled_action_value(
                    "Of total RAM",
                    Field::MemoryShare,
                    &row_share,
                    typography,
                    sizing,
                    spacing,
                ))
                .width(MEMORY_FIELD_WIDTH),
            ]
            .mirrored(crate::i18n::rtl())
            .spacing(spacing.sm())
            .width(Length::Fill)
            .into()
        }),
        Box::new(move || {
            column![
                labelled_action_value(
                    "Memory",
                    Field::Memory,
                    &memory,
                    typography,
                    sizing,
                    spacing,
                ),
                labelled_action_value(
                    "Of total RAM",
                    Field::MemoryShare,
                    &memory_share,
                    typography,
                    sizing,
                    spacing,
                ),
            ]
            .spacing(FIELD_GAP.resolve(&spacing))
            .width(Length::Fill)
            .into()
        }),
    ])
    .height(Length::Shrink)
    .into()
}

fn kind_label(kind: &str) -> &str {
    match kind {
        "App" => "Application",
        "Service" => "Background service",
        "System" => "System process",
        other => other,
    }
}

// The labeled-content group keeps each name associated with its copy action in
// the accessibility tree while the value remains a stable keyboard target.
fn labelled_action_value<'a>(
    label: &str,
    field: Field,
    value: &str,
    typography: enzui_theme::TypographyScale,
    sizing: enzui_theme::SizingTokens,
    spacing: enzui_theme::SpacingScale,
) -> Element<'a, Message> {
    let value = if value.trim().is_empty() {
        "\u{2014}".to_owned()
    } else {
        value.to_owned()
    };
    // Preserve the disambiguating filename or argv suffix when paths elide.
    let ellipsis = if matches!(
        field,
        Field::CommandLine | Field::UnitFile | Field::WorkingDirectory
    ) {
        iced::widget::text::Ellipsis::Middle
    } else {
        iced::widget::text::Ellipsis::End
    };
    let rhythm = FieldRhythm::from(typography);
    let control = container(crate::view::copyable::copyable_value_boxed(
        &value,
        match field {
            Field::State | Field::Kind | Field::SubState | Field::Load => {
                crate::i18n::display(&value)
            }
            Field::Status => value
                .strip_suffix(')')
                .and_then(|value| value.split_once(" ("))
                .map_or_else(
                    || crate::i18n::display(&value),
                    |(state, sub_state)| {
                        crate::i18n::message(
                            "tm-state-sub-state",
                            &[
                                ("state", crate::i18n::argument(&crate::i18n::display(state))),
                                (
                                    "substate",
                                    crate::i18n::argument(&crate::i18n::display(sub_state)),
                                ),
                            ],
                        )
                    },
                ),
            Field::Threads | Field::OpenFiles => {
                crate::i18n::display(crate::i18n::number_text(value.clone()))
            }
            Field::Cpu | Field::Memory | Field::MemoryShare | Field::Usage => {
                crate::i18n::reading(&value)
            }
            _ => value.clone(),
        },
        copy_confirmation(field),
        inspector_value_id(field),
        ellipsis,
        rhythm.value_size,
        rhythm.value_box,
    ))
    .padding(if crate::i18n::rtl() {
        iced::Padding::ZERO.right(spacing.xs())
    } else {
        iced::Padding::ZERO.left(spacing.xs())
    })
    .width(Length::Fill);
    enz_labeled_content(crate::i18n::display(label))
        .control(control)
        .label_size(rhythm.label_size)
        .row_height(rhythm.value_box)
        .row_spacing(spacing.space1)
        .padding_x(0.0)
        .layout(LabeledContentLayout::Compact)
        .typography(typography)
        .sizing(sizing)
        .into_element()
}

// Field separation must remain larger than the compact label-to-value gap.
const FIELD_GAP: SpaceToken = SpaceToken::Lg;
const MEMORY_FIELD_WIDTH: f32 = 112.0;

#[derive(Clone, Copy)]
struct FieldRhythm {
    label_size: f32,
    value_size: f32,
    value_box: f32,
}

impl FieldRhythm {
    fn from(typography: enzui_theme::TypographyScale) -> Self {
        let value_size = typography.section_size;
        Self {
            label_size: typography.footnote_size,
            value_size,
            value_box: (value_size * 1.35)
                .ceil()
                .max(crate::view::copyable::MIN_COPY_TARGET_HEIGHT),
        }
    }
}

#[derive(Clone, Copy)]
enum Field {
    Pid,
    Cpu,
    Memory,
    MemoryShare,
    User,
    State,
    Kind,
    CommandLine,
    Threads,
    OpenFiles,
    WorkingDirectory,
    Status,
    SubState,
    Load,
    Usage,
    Description,
    UnitFile,
}

impl Field {
    fn key(self) -> &'static str {
        match self {
            Self::Pid => "task-manager.inspector.value.pid",
            Self::Cpu => "task-manager.inspector.value.cpu",
            Self::Memory => "task-manager.inspector.value.memory",
            Self::MemoryShare => "task-manager.inspector.value.memory-share",
            Self::User => "task-manager.inspector.value.user",
            Self::State => "task-manager.inspector.value.state",
            Self::Kind => "task-manager.inspector.value.kind",
            Self::CommandLine => "task-manager.inspector.value.command-line",
            Self::Threads => "task-manager.inspector.value.threads",
            Self::OpenFiles => "task-manager.inspector.value.open-files",
            Self::WorkingDirectory => "task-manager.inspector.value.working-directory",
            Self::Status => "task-manager.inspector.value.status",
            Self::SubState => "task-manager.inspector.value.sub-state",
            Self::Load => "task-manager.inspector.value.load",
            Self::Usage => "task-manager.inspector.value.usage",
            Self::Description => "task-manager.inspector.value.description",
            Self::UnitFile => "task-manager.inspector.value.unit-file",
        }
    }
}

fn copy_confirmation(field: Field) -> &'static str {
    match field {
        Field::Pid => "Copied PID",
        Field::Cpu => "Copied CPU usage",
        Field::Memory => "Copied memory usage",
        Field::MemoryShare => "Copied memory share",
        Field::User => "Copied process owner",
        Field::State => "Copied process state",
        Field::Kind => "Copied process kind",
        Field::CommandLine => "Copied command line",
        Field::Threads => "Copied thread census",
        Field::OpenFiles => "Copied open file census",
        Field::WorkingDirectory => "Copied working directory",
        Field::Status => "Copied service status",
        Field::SubState => "Copied service sub-state",
        Field::Load => "Copied service load state",
        Field::Usage => "Copied service usage",
        Field::Description => "Copied service description",
        Field::UnitFile => "Copied unit file path",
    }
}
