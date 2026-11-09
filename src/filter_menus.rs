//! Process and service filter menu models.

use enzui_core::menu_model::{CheckState, MenuEntryModel, MenuModel};

use crate::model::{ProcessFilter, ServiceFilter};

fn filter_entry(id: &str, label: &str, selected: bool) -> MenuEntryModel {
    MenuEntryModel::command(id, crate::i18n::display(label)).check(if selected {
        CheckState::Checked
    } else {
        CheckState::Unchecked
    })
}

pub(crate) fn process_filter_menu(filter: ProcessFilter) -> MenuModel {
    MenuModel::new(vec![
        filter_entry("all", "All", filter == ProcessFilter::All),
        filter_entry("running", "Running", filter == ProcessFilter::Running),
        filter_entry("sleeping", "Sleeping", filter == ProcessFilter::Sleeping),
    ])
}

pub(crate) fn service_filter_menu(filter: ServiceFilter) -> MenuModel {
    MenuModel::new(vec![
        filter_entry("all", "All", filter == ServiceFilter::All),
        filter_entry("running", "Running", filter == ServiceFilter::Running),
        filter_entry("stopped", "Stopped", filter == ServiceFilter::Stopped),
        filter_entry("failed", "Failed", filter == ServiceFilter::Failed),
    ])
}
