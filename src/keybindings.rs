//! Keyboard bindings shared by the event dispatcher and shortcut-help dialog.
//!
//! Matching refuses repeated key presses. The dialog displays primary chords;
//! alternate chords also dispatch. The reducer suppresses actions during modals.

use enzui_core::action::{ShortcutBinding, shortcut_binding_for_event};
use enzui_core::shortcut::{Modifier, NamedKey, Shortcut};
use iced::keyboard;
use std::sync::LazyLock;

/// A command dispatched by a keyboard binding.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum TaskManagerAction {
    /// Request a fresh snapshot with Ctrl+R or F5.
    Refresh,
    /// Focus the titlebar search field, revealing it first if it is
    /// collapsed to its icon-button form. Bound to Ctrl+F and `/`.
    FocusSearch,
    /// Request confirmation to end the selected process with Delete or Ctrl+E.
    /// Protected targets are refused.
    EndProcess,
}

/// Command bindings in help-dialog order.
pub(crate) static ALL_BINDINGS: LazyLock<Vec<ShortcutBinding<TaskManagerAction>>> =
    LazyLock::new(|| {
        vec![
            ShortcutBinding::new(
                "task-manager.refresh",
                TaskManagerAction::Refresh,
                Shortcut::new(Modifier::CTRL, 'r'),
            )
            .with_description("Refresh")
            .with_alternate(Shortcut::new(Modifier::NONE, NamedKey::F5)),
            ShortcutBinding::new(
                "task-manager.search",
                TaskManagerAction::FocusSearch,
                Shortcut::new(Modifier::CTRL, 'f'),
            )
            .with_description("Search")
            .with_alternate(Shortcut::new(Modifier::NONE, '/')),
            // Force-kill escalation requires the confirmation dialog, not a chord.
            ShortcutBinding::new(
                "task-manager.end-process",
                TaskManagerAction::EndProcess,
                Shortcut::new(Modifier::NONE, NamedKey::Delete),
            )
            .with_description("End process")
            .with_alternate(Shortcut::new(Modifier::CTRL, 'e')),
        ]
    });

/// Help rows derived from the same bindings as dispatch.
pub(crate) fn shortcut_dialog_entries() -> Vec<enzui_widgets::shortcut_list::ShortcutEntry> {
    let mut rows = enzui_widgets::shortcut_list::shortcut_catalog_entries(&*ALL_BINDINGS);
    for row in &mut rows {
        row.description = crate::i18n::display(&row.description);
    }
    rows
}

/// Decode a raw key press into the [`TaskManagerAction`] the table binds it to,
/// or `None` if no shortcut claims it.
///
/// Takes the whole [`keyboard::Event`] so held-down repeats are refused. Tab
/// uses the shared focus route; Escape is handled by the modal host.
pub(crate) fn match_event(event: &keyboard::Event) -> Option<TaskManagerAction> {
    shortcut_binding_for_event(&*ALL_BINDINGS, event, &()).map(|binding| *binding.command())
}
