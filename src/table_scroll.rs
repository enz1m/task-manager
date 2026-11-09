//! Reducer-side table scroll integration.
//!
//! Wheel/touchpad input stays a normalized pixel delta until this module applies
//! it to the latest model offset. Scrollbar dragging remains an absolute offset
//! and continues through [`Model::scroll_table_to`].

use crate::model::Model;

impl Model {
    /// Apply one wheel delta using iced's sign convention (wheel-up positive).
    pub(crate) fn scroll_table_by_wheel(&mut self, dy: f32) {
        if dy.is_finite() {
            self.scroll_table_to(self.table_scroll - dy);
        }
    }
}
