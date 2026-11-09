# Task Manager contributor notes

Use shared enzUI controls, theme tokens and native services. Keep blocking work off
the UI thread, preserve cancellation and stale-result ownership, and protect user
data and credentials. Keep tests for durable user workflows and app-owned data or
security boundaries. Avoid source-spelling, widget census and cosmetic tests.

Use ordinary Cargo commands. Private development limits,
fixtures and review output belong under ignored AGENTS/. Use isolated state for
GUI checks and inspect retained screenshots. Translations live in assets/locales.
