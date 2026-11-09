//! Process signals and identity.

use super::*;

// Unavailable identity or exit observations must fail closed.
pub(crate) trait PinnedProcess: Send {
    fn start_time_ticks(&self) -> Result<u64, SignalFailure>;
    fn send_signal(&self, signal: Signal) -> Result<(), SignalFailure>;
    fn uid(&self) -> Result<u32, SignalFailure> {
        Err(SignalFailure::IdentityUnverifiable)
    }
    fn has_exited(&self) -> Result<bool, SignalFailure> {
        Err(SignalFailure::IdentityUnverifiable)
    }
}

pub(crate) trait ProcessSignalBackend: Send + Sync {
    fn pin(&self, pid: u32) -> Result<Box<dyn PinnedProcess>, SignalFailure>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SignalFailure {
    NotPermitted,
    Gone,
    IdentityChanged,
    IdentityUnverifiable,
    Unsupported,
    // Raw diagnostic text is excluded from user-facing failure messages.
    Other(String),
}

impl std::fmt::Display for SignalFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignalFailure::NotPermitted => f.write_str("operation not permitted"),
            SignalFailure::Gone => f.write_str("no such process"),
            SignalFailure::IdentityChanged => f.write_str("process identity changed"),
            SignalFailure::IdentityUnverifiable => f.write_str("process identity unreadable"),
            SignalFailure::Unsupported => f.write_str("operation not supported"),
            SignalFailure::Other(raw) => f.write_str(raw),
        }
    }
}

impl SignalFailure {
    fn from_errno(errno: rustix::io::Errno) -> Self {
        use rustix::io::Errno;
        match errno {
            Errno::PERM | Errno::ACCESS => SignalFailure::NotPermitted,
            Errno::SRCH | Errno::NOENT => SignalFailure::Gone,
            // `pidfd_open`/`pidfd_send_signal` are missing, or this signal is
            // not deliverable that way: not a permission problem and not
            // something a refresh fixes.
            Errno::NOSYS | Errno::OPNOTSUPP | Errno::INVAL => SignalFailure::Unsupported,
            other => SignalFailure::Other(other.to_string()),
        }
    }
}

// Put the PID first so clamping a hostile process name cannot hide it.
pub(crate) fn process_subject(pid: &str, name: &str) -> String {
    let pid = bounded_subject(pid);
    let name = bounded_subject(name);
    if name.is_empty() {
        crate::i18n::message(
            "tm-pid-pid-4a52d4",
            &[("arg0", crate::i18n::argument(&(pid)))],
        )
    } else {
        crate::i18n::message(
            "tm-pid-pid-name-e88e9b",
            &[
                ("arg0", crate::i18n::argument(&(pid))),
                ("arg1", crate::i18n::argument(&(name))),
            ],
        )
    }
}

/// Announcement urgency carried with the worker outcome, independent of its wording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Urgency {
    /// A polite status announcement.
    Info,
    /// An assertive alert announcement.
    Warning,
}

/// An action message and its announcement urgency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActionOutcome {
    pub(crate) text: String,
    pub(crate) urgency: Urgency,
}

impl ActionOutcome {
    pub(crate) fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            urgency: Urgency::Info,
        }
    }

    pub(crate) fn warning(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            urgency: Urgency::Warning,
        }
    }
}

pub(crate) struct LinuxProcessSignalBackend;

struct LinuxPinnedProcess {
    proc_directory: std::fs::File,
    pidfd: rustix::fd::OwnedFd,
}

impl ProcessSignalBackend for LinuxProcessSignalBackend {
    fn pin(&self, pid: u32) -> Result<Box<dyn PinnedProcess>, SignalFailure> {
        let raw_pid = i32::try_from(pid).map_err(|_| SignalFailure::IdentityUnverifiable)?;
        let rustix_pid =
            rustix::process::Pid::from_raw(raw_pid).ok_or(SignalFailure::IdentityUnverifiable)?;
        let pidfd = rustix::process::pidfd_open(rustix_pid, rustix::process::PidfdFlags::empty())
            .map_err(SignalFailure::from_errno)?;
        let proc_directory =
            std::fs::File::open(format!("/proc/{pid}")).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => SignalFailure::Gone,
                std::io::ErrorKind::PermissionDenied => SignalFailure::NotPermitted,
                _ => SignalFailure::IdentityUnverifiable,
            })?;
        Ok(Box::new(LinuxPinnedProcess {
            proc_directory,
            pidfd,
        }))
    }
}

impl PinnedProcess for LinuxPinnedProcess {
    fn start_time_ticks(&self) -> Result<u64, SignalFailure> {
        use std::os::fd::AsRawFd;
        let text = read_action_proc_file(format!(
            "/proc/self/fd/{}/stat",
            self.proc_directory.as_raw_fd()
        ))?;
        parse_process_stat(&text)
            .map(|stat| stat.start_time_ticks)
            .ok_or(SignalFailure::IdentityUnverifiable)
    }

    fn uid(&self) -> Result<u32, SignalFailure> {
        process_uid_in(&self.proc_directory)
    }

    fn has_exited(&self) -> Result<bool, SignalFailure> {
        use std::os::fd::AsRawFd;
        let text = read_action_proc_file(format!("/proc/self/fdinfo/{}", self.pidfd.as_raw_fd()))?;
        // Linux fdinfo reports -1 only after this pidfd's process has been reaped;
        // missing procfs data is not evidence of exit, nor is an empty process list.
        let pid = text
            .lines()
            .find_map(|line| line.strip_prefix("Pid:"))
            .and_then(|pid| pid.trim().parse::<i32>().ok())
            .ok_or(SignalFailure::IdentityUnverifiable)?;
        match pid {
            -1 => Ok(true),
            1.. => Ok(false),
            _ => Err(SignalFailure::IdentityUnverifiable),
        }
    }

    fn send_signal(&self, signal: Signal) -> Result<(), SignalFailure> {
        rustix::process::pidfd_send_signal(&self.pidfd, signal.as_rustix())
            .map_err(SignalFailure::from_errno)
    }
}

pub(crate) fn read_action_proc_file(path: String) -> Result<String, SignalFailure> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| SignalFailure::IdentityUnverifiable)?;
    let mut text = String::new();
    file.take(4097)
        .read_to_string(&mut text)
        .map_err(|_| SignalFailure::IdentityUnverifiable)?;
    if text.len() > 4096 {
        return Err(SignalFailure::IdentityUnverifiable);
    }
    Ok(text)
}

#[cfg(test)]
pub(crate) fn process_action_allowed(
    pid: &str,
    comm: &str,
    command: &str,
    user: &str,
    _kind: &str,
    caller: &str,
) -> bool {
    caller_may_act(pid, user, caller) && !is_protected_process(comm, command)
}

/// Returns `USER`, then `LOGNAME`, or an empty string when both are absent.
/// This display identity does not authorize kernel signals.
pub(crate) fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default()
}

pub(crate) fn caller_may_act(pid: &str, user: &str, caller: &str) -> bool {
    let pid = pid.trim().parse::<u32>().unwrap_or(0);
    if pid <= 1 || pid == std::process::id() {
        return false;
    }
    if user == "root" || user == "systemd-resolve" {
        return false;
    }
    caller == "root" || user == caller
}

// Cached command classification never substitutes for per-row PID and owner checks.
pub(crate) fn row_action_allowed(
    identity: &ProcessIdentity,
    pid: &str,
    user: &str,
    caller: &str,
) -> bool {
    !identity.protected && caller_may_act(pid, user, caller)
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessIdentity {
    pub(crate) display_name: String,
    pub(crate) kind: String,
    pub(crate) protected: bool,
}
