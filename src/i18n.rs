//! Application catalogs and presentation-only locale formatting.

use enzui_core::localization::{
    self, Arguments, CatalogBuilder, Decimal, LocaleCategory, Localizer, MessageArgument, Value,
};

mod vocabulary;

pub(crate) const ENGLISH: &str = include_str!("../assets/locales/en-US.ftl");
pub(crate) const FRENCH: &str = include_str!("../assets/locales/fr.ftl");
pub(crate) const ARABIC: &str = include_str!("../assets/locales/ar.ftl");

pub(crate) fn from_environment() -> Localizer {
    let localizer = make_localizer(localization::preferred_locales());
    for category in LocaleCategory::ALL {
        let locale =
            localization::category_locale_from_env(category, |key| std::env::var(key).ok());
        let _ = localizer.set_category_locale(category, locale);
    }
    localizer
}

pub(crate) fn make_localizer(preferred: Vec<localization::Locale>) -> Localizer {
    let mut catalog = CatalogBuilder::new();
    for (locale, source) in [("en-US", ENGLISH), ("fr", FRENCH), ("ar", ARABIC)] {
        catalog
            .add_resource(locale, source)
            .expect("embedded app catalog must be valid");
    }
    Localizer::new(catalog.build(), preferred, localization::Locale::default())
}

/// Carry the caller's catalog into a worker; thread-local scopes do not cross dispatch.
pub(crate) fn in_worker<Context, Result>(
    work: impl FnOnce(Context) -> Result + Send,
) -> impl FnOnce(Context) -> Result + Send {
    let localizer = localization::current();
    move |context| localizer.scope(|| work(context))
}

pub(crate) fn message(key: &str, values: &[(&str, Value<'_>)]) -> String {
    let mut arguments = Arguments::new();
    for (name, value) in values {
        arguments.set(*name, value.clone());
    }
    localization::text_with(key, &arguments)
}

pub(crate) fn argument<T: MessageArgument + ?Sized>(value: &T) -> Value<'_> {
    value.message_value()
}

pub(crate) fn display(value: impl AsRef<str>) -> String {
    let value = value.as_ref();
    let reading = reading(value);
    if reading != value {
        return reading;
    }
    vocabulary::key(value).map_or(reading, |key| {
        let translated = localization::text(key);
        if translated == key {
            value.to_owned()
        } else {
            translated
        }
    })
}

pub(crate) fn number_text(value: String) -> String {
    value
        .parse::<Decimal>()
        .ok()
        .and_then(|number| {
            localization::current()
                .locale_for(LocaleCategory::Numeric)
                .format_decimal(&number)
                .ok()
        })
        .unwrap_or(value)
}

pub(crate) fn rtl() -> bool {
    localization::current().locale().direction() == enzui_core::placement::Direction::RightToLeft
}

pub(crate) fn reading(value: &str) -> String {
    if let Some((size, percent)) = value
        .strip_suffix("%)")
        .and_then(|value| value.split_once(" ("))
        && percent.parse::<Decimal>().is_ok()
    {
        return format!("{} ({}%)", reading(size), number_text(percent.to_owned()));
    }
    if let Some((count, size)) = value
        .strip_suffix(')')
        .and_then(|value| value.split_once(" bytes ("))
        && let Ok(count) = count.parse::<u64>()
    {
        return message(
            "tm-byte-count",
            &[
                ("count", argument(&count)),
                ("size", argument(&reading(size))),
            ],
        );
    }
    if let Some(percent) = value.strip_suffix("% utilization")
        && let Ok(percent) = percent.parse::<u32>()
    {
        return message("tm-utilization-reading", &[("percent", argument(&percent))]);
    }
    if let Some((left, right)) = value.split_once(" / ") {
        return format!("{} / {}", reading(left), reading(right));
    }
    for unit in [
        "GiB/s", "MiB/s", "KiB/s", "Gbps", "Mbps", "Kbps", "GiB", "MiB", "KiB", "GHz", "MHz", "°C",
        "%",
    ] {
        if let Some(number) = value.strip_suffix(unit) {
            let number = number.trim();
            if number.parse::<Decimal>().is_ok() {
                let separator = if matches!(unit, "%" | "°C") { "" } else { " " };
                return format!("{}{}{}", number_text(number.to_owned()), separator, unit);
            }
        }
    }
    if let Some(state) = value.strip_prefix("Network • ") {
        return format!("{} • {}", display("Network"), display(state));
    }
    value.to_owned()
}

#[cfg(test)]
mod tests;
