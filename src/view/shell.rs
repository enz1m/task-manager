//! Responsive app chrome with a full-height resource sidebar on Performance.
//!
//! The shell owns the page-top inset; page views own side and bottom padding.
//! The shell owns titlebar geometry and window gestures. Sidebar slots remain
//! mounted during withdrawal so the shell can animate them out.

use enzui_theme::EnzTheme;
use enzui_theme::SpacingScale;
use enzui_widgets::action_bar::ActionItem;
use enzui_widgets::app_shell::{
    PAGE_INSET_TOKEN, ShellLayout, ShellTokens, SidebarHeader, enz_app_shell_with_geometry,
};
use enzui_widgets::nav_split::NavSplitState;
use enzui_widgets::search_field::{SearchPresentation, enz_titlebar_search};
use enzui_widgets::title_bar::enz_title_bar;
use enzui_widgets::view_switcher::{
    SwitcherPage, SwitcherPolicy, SwitcherTokens, enz_view_switcher,
};
use iced::widget::{Space, container, row};
use iced::{Element, Length};

use crate::message::Message;
use crate::model::{Model, Page};

pub(crate) const SIDEBAR_WIDTH: f32 = enzui_widgets::nav_split::DEFAULT_SIDEBAR_WIDTH;

pub(crate) fn shell_tokens(spacing: &SpacingScale) -> ShellTokens {
    ShellTokens {
        sidebar_width: SIDEBAR_WIDTH,
        sidebar_header: SidebarHeader::Inside,
        // The sole top-spacing authority; page columns apply no top padding.
        page_inset: PAGE_INSET_TOKEN.resolve(spacing),
        ..ShellTokens::default()
    }
}

pub(crate) fn resolved_shell_tokens(spacing: &SpacingScale) -> ShellTokens {
    shell_tokens(spacing).with_flush_defaults(ShellLayout::TitlebarSidebarContent)
}

pub(crate) const SWITCHER_PAGES: [(&str, &str); 3] = [
    ("performance", "Performance"),
    ("processes", "Processes"),
    ("services", "Services"),
];

pub(crate) fn switcher_index(active: Page) -> usize {
    match active {
        Page::Performance => 0,
        Page::Apps | Page::Processes => 1,
        Page::Services => 2,
    }
}

fn titlebar<'a>(model: &'a Model, theme: &'a EnzTheme) -> Element<'a, Message> {
    let actions = vec![
        ActionItem::icon(
            "task-manager.refresh",
            "refresh-cw",
            crate::i18n::display("Refresh"),
            Message::Refresh,
        ),
        ActionItem::icon(
            "task-manager.shortcuts",
            "keyboard",
            crate::i18n::display("Keyboard Shortcuts"),
            Message::OpenShortcuts,
        ),
    ];

    enz_title_bar()
        .push(titlebar_content(model, theme))
        .fluid_content(titlebar_min_width(model, theme))
        .actions(actions, Message::ToggleTitlebarMenu)
        .overflow_open(model.titlebar_menu_open)
        .action_dismiss(Message::CloseTitlebarMenu)
        .action_menu_id_prefix("task-manager-titlebar-actions")
        .action_overflow_trigger_id(iced::advanced::widget::Id::new(
            "task-manager-titlebar-overflow",
        ))
        .id("task-manager-app-window")
        .into_element()
}

fn titlebar_min_width(model: &Model, theme: &EnzTheme) -> f32 {
    let tokens = SwitcherTokens::default().compressed();
    let geo = tokens.tab_pill_geometry();
    let tabs = 3.0 * (2.0 * geo.pill_padding_x + geo.icon_size) + 2.0 * tokens.padding;
    tabs + if model.active.has_searchable_list() {
        theme.spacing.sm()
            + enzui_widgets::toolbar_icon::TitlebarAffordanceTokens::DEFAULT
                .effective_control_size()
    } else {
        0.0
    }
}

fn titlebar_content<'a>(model: &'a Model, theme: &'a EnzTheme) -> Element<'a, Message> {
    enzui_widgets::responsive::enz_responsive(move |size| {
        let selected = switcher_index(model.active);
        let icons = ["activity", "list-tree", "settings"];
        let labels: Vec<String> = SWITCHER_PAGES
            .iter()
            .map(|(_, title)| crate::i18n::display(*title))
            .collect();
        let wide = SwitcherTokens {
            tab_min_width: 0.0,
            auto_floor: 0.0,
            ..SwitcherTokens::default()
        };
        let compact = wide.compressed();
        let has_search = model.active.has_searchable_list();
        let search_reserve = if has_search {
            theme.spacing.sm()
                + enzui_widgets::toolbar_icon::TitlebarAffordanceTokens::DEFAULT
                    .effective_control_size()
        } else {
            0.0
        };
        let strip_width = |tokens: SwitcherTokens, active_only: bool, icons_only: bool| {
            let geo = tokens.tab_pill_geometry();
            labels
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    let width = if icons_only || (active_only && index != selected) {
                        2.0 * geo.pill_padding_x + geo.icon_size
                    } else {
                        tokens.tab_width(label, true)
                    };
                    width
                        + if model
                            .pending_page
                            .is_some_and(|page| page.id() == SWITCHER_PAGES[index].0)
                        {
                            geo.icon_size + geo.icon_gap
                        } else {
                            0.0
                        }
                })
                .sum::<f32>()
                + 2.0 * tokens.padding
                + 1.0
        };
        let full_width = strip_width(wide, false, false);
        let active_width = strip_width(compact, true, false);
        let all_labels = size.width >= full_width + search_reserve;
        let icons_only = !all_labels && size.width < active_width + search_reserve;
        let tokens = if all_labels { wide } else { compact };
        let tabs_width = strip_width(tokens, !all_labels, icons_only);
        let pages = SWITCHER_PAGES
            .iter()
            .enumerate()
            .map(|(index, (id, _))| {
                SwitcherPage::new(*id, labels[index].clone())
                    .with_icon(icons[index])
                    .with_busy(model.pending_page.is_some_and(|page| page.id() == *id))
            })
            .collect();
        let switcher = enz_view_switcher(pages, selected, |index| {
            let id = SWITCHER_PAGES
                .get(index)
                .map_or("performance", |(id, _)| *id);
            Message::SelectPage(Page::from_id(id).unwrap_or(Page::Performance))
        })
        .policy(SwitcherPolicy {
            compact_fallback: false,
            width_floors: false,
            ..SwitcherPolicy::PILL_STRIP
        })
        .tokens(tokens)
        .collapse_inactive_labels(!all_labels)
        .icons_only(icons_only)
        .tab_id_prefix("tm-view-switcher")
        .aria_label(crate::i18n::display("Views"))
        .into_element();
        let mut content = row![
            container(switcher).width(Length::Fixed(tabs_width)),
            Space::new().width(Length::Fill),
        ]
        .mirrored(crate::i18n::rtl())
        .align_y(iced::Alignment::Center)
        .width(Length::Fill);
        if has_search {
            let search_width = (size.width - tabs_width - theme.spacing.sm())
                .clamp(0.0, enzui_widgets::search_field::TITLEBAR_SEARCH_MAX_WIDTH);
            content = content.push(
                container(titlebar_search(model, theme))
                    .width(Length::Fixed(search_width))
                    .align_x(iced::Alignment::End),
            );
        }
        content
    })
    .height(Length::Shrink)
    .into()
}

fn titlebar_search<'a>(model: &'a Model, theme: &EnzTheme) -> Element<'a, Message> {
    enz_titlebar_search(&model.query, Message::SearchChanged)
        .presentation(SearchPresentation::AnchoredPopover)
        .radius(theme.radius.full)
        .placeholder(crate::i18n::display(model.active.search_placeholder()))
        .id(crate::search_field_id())
        .on_clear(Message::SearchChanged(String::new()))
        .into()
}

fn sidebar_titlebar<'a>() -> Element<'a, Message> {
    enz_title_bar()
        .title(crate::i18n::display("Resources"))
        .into_element()
}

fn shell_composition<'a>(
    model: &'a Model,
    theme: &'a EnzTheme,
    nav: NavSplitState,
) -> Element<'a, Message> {
    let has_sidebar = model.active.has_sidebar();

    let content = move |geometry: &enzui_widgets::app_shell::ShellGeometry| {
        enzui_core::perf_frame::record_page_build();

        super::collection::page(model, theme, geometry.content_width)
    };

    let mut shell = enz_app_shell_with_geometry(content)
        .layout(ShellLayout::TitlebarSidebarContent)
        .tokens(resolved_shell_tokens(&theme.spacing))
        .breakpoints(theme.breakpoints)
        .state(nav)
        .titlebar(move || titlebar(model, theme))
        .on_window_gesture(Message::Window)
        .sidebar_label(crate::i18n::display("Resources"))
        .on_sidebar_state(Message::Sidebar);

    // Keep both slots mounted while the sidebar animates out.
    shell = shell
        .sidebar_titlebar(sidebar_titlebar)
        .sidebar(move || super::collection::sidebar(model, theme))
        .sidebar_available(has_sidebar);

    shell.into_element()
}

/// Compose app chrome and page content to fill the window.
pub(crate) fn view<'a>(
    model: &'a Model,
    theme: &'a EnzTheme,
    nav: NavSplitState,
) -> Element<'a, Message> {
    let shell = shell_composition(model, theme, nav);
    container(shell)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}
