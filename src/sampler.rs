//! Cadence wakes only. Accepted blocking work lives in the App's retained owner.

use crate::services::SampleScope;
use iced::Subscription;
use std::time::Duration;

pub(crate) const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) fn subscription(scope: SampleScope, window_visible: bool) -> Subscription<()> {
    if !window_visible {
        return Subscription::none();
    }
    iced::time::every(SAMPLE_INTERVAL).with(scope).map(|_| ())
}
