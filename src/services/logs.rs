//! Journal and log tails.

use super::*;

// Bound transport retention even for a giant single-line entry.
pub(crate) const SERVICE_LOG_MAX_BYTES: usize = 128 * 1024;

/// Maximum journal entries requested in each system or user read.
pub(crate) const JOURNAL_TAIL_LINES: u32 = 300;

pub(crate) fn journal_tail_arg() -> String {
    JOURNAL_TAIL_LINES.to_string()
}

/// Describes the requested journal entry limit.
pub(crate) fn journal_tail_caption() -> String {
    crate::i18n::message(
        "tm-journal-caption",
        &[("count", crate::i18n::argument(&JOURNAL_TAIL_LINES))],
    )
}

pub(crate) fn journal_request(scope_flag: &str, unit: &str, deadline: Duration) -> ProcessRequest {
    ProcessRequest::new(
        JOURNALCTL_EXE,
        StdinPolicy::Null,
        SERVICE_LOG_MAX_BYTES,
        HELPER_STDERR_MAX_BYTES,
        deadline,
    )
    .args(["--no-pager", "-n"])
    .arg(journal_tail_arg())
    .arg(scope_flag)
    .arg(unit)
}

fn run_journal(
    scope_flag: &str,
    unit: &str,
    deadline: Duration,
    cancellation: &CancelToken,
) -> Result<ProcessOutput, ProcessError> {
    run_request(journal_request(scope_flag, unit, deadline), cancellation)
}

/// Returns a bounded journal tail, trying system then user scope within one deadline.
/// Transport failures and absent entries return a diagnostic sentence.
pub(crate) fn service_logs(unit: &str, cancellation: &CancelToken) -> String {
    let unit = unit.trim();
    if unit.is_empty() {
        return "No service selected.".to_owned();
    }
    // System and user journal reads share one deadline, so a user-only unit
    // cannot cost two full deadlines.
    let budget = RouteBudget::new(SERVICE_LOG_DEADLINE);
    service_logs_reduce(unit, budget, |scope, deadline| {
        run_journal(scope, unit, deadline, cancellation)
    })
}

pub(crate) fn service_logs_route(unit: &str) -> HelperRoute<'_> {
    HelperRoute {
        call: "journalctl",
        subject: unit,
        stdout_noun: "Logs",
        stdout_limit: SERVICE_LOG_MAX_BYTES,
        stderr_limit: HELPER_STDERR_MAX_BYTES,
    }
}

fn log_transport_error(unit: &str, error: &ProcessError) -> String {
    helper_failure_message(service_logs_route(unit), error.kind())
}

pub(crate) fn service_logs_reduce(
    unit: &str,
    budget: RouteBudget,
    mut run_scope: impl FnMut(&str, Duration) -> Result<ProcessOutput, ProcessError>,
) -> String {
    // Preserve the system diagnostic if the shared budget prevents a user read.
    let primary_stderr = match run_scope("-u", budget.remaining()) {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !text.is_empty() && !text.starts_with("-- No entries --") {
                return text;
            }
            output.stderr
        }
        Ok(output) => output.stderr,
        Err(error) => return log_transport_error(unit, &error),
    };

    // Per-user units only appear in the user journal; retry with the remaining
    // budget, or report the primary result when none is left.
    let Some(remaining) = budget.next_child_deadline() else {
        return no_logs_available(unit, &primary_stderr);
    };
    match run_scope("--user-unit", remaining) {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            if !text.is_empty() && !text.starts_with("-- No entries --") {
                return text;
            }
        }
        Ok(output) => {
            if !output.stderr.is_empty() {
                return no_logs_available(unit, &output.stderr);
            }
        }
        Err(error) => return log_transport_error(unit, &error),
    }

    no_logs_available(unit, &primary_stderr)
}

// The Logs view splits this prefix and the optional reason at ": ".
pub(crate) fn no_logs_available(unit: &str, stderr: &[u8]) -> String {
    let subject = bounded_subject(unit);
    let message = match bounded_detail(stderr) {
        Some(detail) => format!("No logs available for {subject}: {detail}"),
        None => format!("No logs available for {subject}."),
    };
    bounded_message(message)
}
