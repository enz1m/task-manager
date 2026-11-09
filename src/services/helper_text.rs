//! Helper output bounds.

use super::*;

/// Longest unit (or other subject) name a helper failure sentence may quote.
/// Unit names are untrusted, so this keeps every translated sentence bounded.
pub(crate) const HELPER_SUBJECT_MAX_CHARS: usize = 64;

/// Maximum UTF-8 byte length of a translated journal failure.
pub(crate) const HELPER_MESSAGE_MAX_BYTES: usize = 320;

/// Names and capture limits used to translate journal transport failures.
#[derive(Clone, Copy)]
pub(crate) struct HelperRoute<'a> {
    /// The invocation as the user would name it, e.g. `"journalctl"`.
    pub(crate) call: &'a str,
    /// What the route acts on, e.g. the unit name. Clamped before use.
    pub(crate) subject: &'a str,
    /// This route's standard output, as a capitalised noun phrase that can
    /// open a sentence, e.g. `"Logs"`.
    pub(crate) stdout_noun: &'a str,
    /// The route's standard-output ceiling, in bytes.
    pub(crate) stdout_limit: usize,
    /// The route's standard-error ceiling, in bytes.
    pub(crate) stderr_limit: usize,
}

/// Clamp untrusted text to a bounded, printable, single-line quotable subject.
///
/// Also used by the Logs dialog heading (`view::logs::logs_heading`).
/// Invisible formatting is removed, and anything the clamp dropped (extra lines
/// or characters) is marked with `…`.
pub(crate) fn bounded_subject(subject: &str) -> String {
    let (line, continues) = first_line_and_continuation(subject);
    let mut clamped: String = line.chars().take(HELPER_SUBJECT_MAX_CHARS).collect();
    let truncated = clamped.chars().count() < line.chars().count();
    if truncated || continues {
        clamped.push('…');
    }
    clamped
}

/// Clamp a finished sentence to [`HELPER_MESSAGE_MAX_BYTES`] on a char
/// boundary.
pub(crate) fn bounded_message(mut message: String) -> String {
    if message.len() <= HELPER_MESSAGE_MAX_BYTES {
        return message;
    }
    let mut end = HELPER_MESSAGE_MAX_BYTES - '…'.len_utf8();
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message.push('…');
    message
}

/// Longest run of a helper's own words a translated sentence may quote.
///
/// The helper's first line is the most actionable detail but is untrusted, so
/// it is clamped here and the whole sentence again by [`bounded_message`].
const HELPER_DETAIL_MAX_CHARS: usize = 120;

/// Every code point that ends a line of untrusted text, including `U+0085`,
/// `U+2028` and `U+2029`, which renderers also honour as line breaks.
fn is_line_boundary(character: char) -> bool {
    matches!(
        character,
        '\n' | '\r' | '\u{0085}' | '\u{2028}' | '\u{2029}'
    )
}

/// A code point that occupies no space yet changes what the user sees: the
/// zero-width family (`U+200B..U+200D`, `U+2060..U+2064`, `U+FEFF`,
/// `U+180E`) and the bidi marks/overrides/isolates (`U+061C`, `U+200E`,
/// `U+200F`, `U+202A..U+202E`, `U+2066..U+2069`).
///
/// An override can reverse the rest of a heading or announcement, and a
/// zero-width run pads a name past a visible clamp. They carry no content, so
/// they are removed rather than folded to a space.
fn is_invisible_format(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
    )
}

/// Collapse one line of untrusted text into printable content.
///
/// Control characters (C0/C1/DEL) and whitespace runs become a single space;
/// invisible formatting characters are removed; leading and trailing
/// separators are dropped. The result has no line break, so it cannot grow the
/// toast vertically or smuggle a second "sentence" into an announcement.
fn sanitize_helper_line(line: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for character in line.chars() {
        if is_invisible_format(character) {
            continue;
        }
        if character.is_control() || character.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(character);
        }
    }
    out
}

/// The first line of untrusted text that has any printable content, plus
/// whether a further such line was dropped.
///
/// Splitting on [`is_line_boundary`] is what makes a `U+2028`-separated
/// payload a continuation rather than one long "line" folded into spaces.
fn first_line_and_continuation(text: &str) -> (String, bool) {
    let mut lines = text
        .split(is_line_boundary)
        .map(sanitize_helper_line)
        .filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or_default();
    (first, lines.next().is_some())
}

/// The short, printable detail a complete nonzero helper capture may contribute
/// to a translated sentence, or `None` when it said nothing usable.
///
/// Only the first printable line is quoted; any truncation is marked with `…`.
pub(crate) fn bounded_detail(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let (first, more_follows) = first_line_and_continuation(&text);
    if first.is_empty() {
        return None;
    }
    let mut detail: String = first.chars().take(HELPER_DETAIL_MAX_CHARS).collect();
    if more_follows || detail.chars().count() < first.chars().count() {
        detail.push('…');
    }
    Some(detail)
}

/// Which pipe overflowed or failed, in user words rather than the enum's.
fn stream_noun(stream: OutputStream) -> &'static str {
    match stream {
        OutputStream::Stdout => "standard output",
        OutputStream::Stderr => "error output",
    }
}

/// Translate a typed transport failure into one bounded human sentence.
///
/// Exhaustive over [`ProcessErrorKind`] and [`CancelCause`], so a new framework
/// variant fails to compile rather than reaching a user as a `Display` string.
pub(crate) fn helper_failure_message(route: HelperRoute<'_>, kind: &ProcessErrorKind) -> String {
    let HelperRoute {
        call,
        subject,
        stdout_noun,
        stdout_limit,
        stderr_limit,
    } = route;
    let subject = bounded_subject(subject);
    let message = match kind {
        ProcessErrorKind::InvalidLimit(stream) => crate::i18n::message(
            "tm-call-could-not-start-for-subject-the-app-asked-for-a-4f5758",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
                (
                    "arg2",
                    crate::i18n::argument(&crate::i18n::display(stream_noun(*stream))),
                ),
            ],
        ),
        ProcessErrorKind::Cancelled(cause) => match cause {
            CancelCause::Deadline => crate::i18n::message(
                "tm-call-ran-out-of-time-for-subject-9669b5",
                &[
                    ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                    ("arg1", crate::i18n::argument(&(subject))),
                ],
            ),
            CancelCause::Cancelled => crate::i18n::message(
                "tm-call-was-cancelled-for-subject-before-it-finished-bbcf80",
                &[
                    ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                    ("arg1", crate::i18n::argument(&(subject))),
                ],
            ),
            CancelCause::Disconnected => crate::i18n::message(
                "tm-call-stopped-for-subject-the-connection-to-it-was-lo-249f0b",
                &[
                    ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                    ("arg1", crate::i18n::argument(&(subject))),
                ],
            ),
            CancelCause::Shutdown => crate::i18n::message(
                "tm-call-was-stopped-for-subject-because-the-app-is-clos-8ab4ae",
                &[
                    ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                    ("arg1", crate::i18n::argument(&(subject))),
                ],
            ),
        },
        ProcessErrorKind::Deadline => crate::i18n::message(
            "tm-call-timed-out-for-subject-9b5a45",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
            ],
        ),
        ProcessErrorKind::ExecutableAuthority => crate::i18n::message(
            "tm-call-could-not-start-for-subject-the-app-has-no-trus-0c2040",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
            ],
        ),
        ProcessErrorKind::Spawn => crate::i18n::message(
            "tm-call-could-not-start-for-subject-the-program-could-n-384493",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
            ],
        ),
        ProcessErrorKind::Setup => crate::i18n::message(
            "tm-call-could-not-start-for-subject-its-output-pipes-co-3177e7",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
            ],
        ),
        ProcessErrorKind::StdinWrite => crate::i18n::message(
            "tm-call-failed-for-subject-input-could-not-be-written-t-31c1bd",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
            ],
        ),
        ProcessErrorKind::Read(stream) => crate::i18n::message(
            "tm-call-failed-for-subject-its-could-not-be-read-ee9bff",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
                (
                    "arg2",
                    crate::i18n::argument(&crate::i18n::display(stream_noun(*stream))),
                ),
            ],
        ),
        ProcessErrorKind::ReaderPanicked(stream) => crate::i18n::message(
            "tm-call-failed-for-subject-the-reader-stopped-unexpecte-8339fa",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
                (
                    "arg2",
                    crate::i18n::argument(&crate::i18n::display(stream_noun(*stream))),
                ),
            ],
        ),
        ProcessErrorKind::WorkerPanicked => crate::i18n::message(
            "tm-call-failed-for-subject-a-helper-task-stopped-unexpe-e70bb6",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
            ],
        ),
        ProcessErrorKind::OutputLimitExceeded(OutputStream::Stdout) => crate::i18n::message(
            "tm-stdout-noun-for-subject-exceeded-the-stdout-limit-by-0e5737",
            &[
                (
                    "arg0",
                    crate::i18n::argument(&crate::i18n::display(stdout_noun)),
                ),
                ("arg1", crate::i18n::argument(&(subject))),
                ("arg2", crate::i18n::argument(&(stdout_limit))),
            ],
        ),
        ProcessErrorKind::OutputLimitExceeded(OutputStream::Stderr) => crate::i18n::message(
            "tm-call-failed-for-subject-it-reported-more-than-stderr-b9fb2d",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
                ("arg2", crate::i18n::argument(&(stderr_limit))),
            ],
        ),
        ProcessErrorKind::Wait => crate::i18n::message(
            "tm-call-finished-for-subject-but-its-exit-status-could-77a5e6",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
            ],
        ),
        ProcessErrorKind::GroupSignal => crate::i18n::message(
            "tm-call-was-stopped-for-subject-but-its-helper-processe-7473b9",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
            ],
        ),
        ProcessErrorKind::ResidualCleanup => crate::i18n::message(
            "tm-call-was-stopped-for-subject-but-a-leftover-helper-p-04222f",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
            ],
        ),
        ProcessErrorKind::DirectChildCleanup => crate::i18n::message(
            "tm-call-was-stopped-for-subject-but-the-program-had-not-ba714b",
            &[
                ("arg0", crate::i18n::argument(&crate::i18n::display(call))),
                ("arg1", crate::i18n::argument(&(subject))),
            ],
        ),
    };
    bounded_message(message)
}
