//! The small icon beside an application's name in the Applications list.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use enzui_widgets::icon::IconSource;
use iced::Element;
use iced::widget::{image, svg};

use crate::message::Message;

pub(crate) const APP_ICON_SIZE: f32 = 16.0;

pub(crate) const HEADING_ICON_SIZE: f32 = 24.0;

// One lookup serves both icon sizes at up to 2x scale.
const LOOKUP_SIZE: u16 = 48;

fn from_path(path: &Path) -> IconSource {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
    {
        svg::Handle::from_path(path).into()
    } else {
        image::Handle::from_path(path).into()
    }
}

thread_local! {
    // Cache misses too, so absent icons do not trigger repeated filesystem searches.
    static ICONS: RefCell<HashMap<String, Option<IconSource>>> = RefCell::new(HashMap::new());
}

pub(crate) fn app_icon(name: &str) -> Option<IconSource> {
    ICONS.with_borrow_mut(|icons| {
        if let Some(icon) = icons.get(name) {
            return icon.clone();
        }
        let icon = crate::services::desktop_app_index()
            .icon_for_name(name)
            .and_then(|icon| enzui_icons::app_icon::find_app_icon(icon, LOOKUP_SIZE, roots()))
            .map(|path| from_path(&path));
        icons.insert(name.to_owned(), icon.clone());
        icon
    })
}

fn roots() -> &'static [std::path::PathBuf] {
    static ROOTS: OnceLock<Vec<std::path::PathBuf>> = OnceLock::new();
    ROOTS.get_or_init(enzui_icons::app_icon::data_roots)
}

pub(crate) fn app_icon_element<'a>(icon: Option<IconSource>, size: f32) -> Element<'a, Message> {
    icon.unwrap_or_else(|| "app-window".into())
        .into_element(size)
}
