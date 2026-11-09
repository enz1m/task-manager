use super::{Entry, FLIGHT_BUDGET, Failure, Limit, Reading};
use crate::services::ProcessTarget;
use enzui_core::cancellation::CancelToken;
use rustix::fd::{AsFd, OwnedFd};
use rustix::fs::{Dir, Mode, OFlags, open, openat};
use std::ffi::CStr;
use std::io::Read;
use std::path::Path;
use std::time::Instant;

pub(super) const ENTRY_LIMIT: usize = 1024;
pub(super) const NAME_LIMIT: usize = 64;
pub(super) const PATH_LIMIT: usize = 4096;
pub(super) const INPUT_LIMIT: usize = 4096;
pub(super) const BYTE_LIMIT: usize = 256 * 1024;
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

pub(super) fn valid_target(target: &ProcessTarget) -> bool {
    target.kernel_uid.is_some()
        && target.start_time_ticks != 0
        && numeric_id(target.pid.as_bytes()).is_ok_and(|pid| pid > 0)
}

pub(super) fn collect(target: &ProcessTarget, token: &CancelToken) -> Reading {
    collect_in(Path::new("/proc"), target, token)
}

pub(super) fn collect_in(root: &Path, target: &ProcessTarget, token: &CancelToken) -> Reading {
    collect_observed(root, target, token, |_| {})
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Census {
    Threads,
    Files,
    Cwd,
}

pub(super) fn collect_observed(
    root: &Path,
    target: &ProcessTarget,
    token: &CancelToken,
    after: impl FnMut(Census),
) -> Reading {
    match Proc::open(root, target, token).and_then(|proc| proc.collect(after)) {
        Ok(reading) => reading,
        Err(failure) => Reading::failed(failure),
    }
}

struct Proc<'a> {
    directory: OwnedFd,
    target: &'a ProcessTarget,
    token: &'a CancelToken,
    started: Instant,
}

#[derive(Clone, Copy)]
struct Identity {
    kernel_thread: bool,
}

struct Budget {
    retained: usize,
}

impl Budget {
    fn retain(&mut self, bytes: usize) -> Result<(), Failure> {
        let total = self
            .retained
            .checked_add(bytes)
            .ok_or(Failure::Limit(Limit::Aggregate))?;
        if total > BYTE_LIMIT {
            return Err(Failure::Limit(Limit::Aggregate));
        }
        self.retained = total;
        Ok(())
    }
}

impl<'a> Proc<'a> {
    fn open(
        root: &Path,
        target: &'a ProcessTarget,
        token: &'a CancelToken,
    ) -> Result<Self, Failure> {
        if !valid_target(target) {
            return Err(Failure::IdentityUnknown);
        }
        let root = open(root, DIRECTORY_FLAGS, Mode::empty()).map_err(io_failure)?;
        let directory = openat(&root, target.pid.as_str(), DIRECTORY_FLAGS, Mode::empty())
            .map_err(identity_io_failure)?;
        Ok(Self {
            directory,
            target,
            token,
            started: Instant::now(),
        })
    }

    fn check(&self) -> Result<(), Failure> {
        if self.token.is_terminal() {
            Err(Failure::Cancelled)
        } else if self.started.elapsed() >= FLIGHT_BUDGET {
            Err(Failure::TimedOut)
        } else {
            Ok(())
        }
    }

    fn identity(&self) -> Result<Identity, Failure> {
        self.check()?;
        let before = self.stat()?;
        let status = read_input(&self.directory, c"status", self).map_err(identity_failure)?;
        let status = std::str::from_utf8(&status).map_err(|_| Failure::IdentityUnknown)?;
        let uid = status
            .lines()
            .find_map(|line| line.strip_prefix("Uid:"))
            .and_then(|line| line.split_whitespace().next())
            .and_then(|uid| uid.parse::<u32>().ok())
            .ok_or(Failure::IdentityUnknown)?;
        let after = self.stat()?;
        if before.0 != self.target.start_time_ticks
            || after.0 != self.target.start_time_ticks
            || Some(uid) != self.target.kernel_uid
        {
            return Err(Failure::IdentityChanged);
        }
        self.check()?;
        Ok(Identity {
            kernel_thread: after.1,
        })
    }

    fn stat(&self) -> Result<(u64, bool), Failure> {
        let text = read_input(&self.directory, c"stat", self).map_err(identity_failure)?;
        let text = std::str::from_utf8(&text).map_err(|_| Failure::IdentityUnknown)?;
        let (prefix, rest) = text.rsplit_once(") ").ok_or(Failure::IdentityUnknown)?;
        let pid = prefix
            .split_once(" (")
            .map(|(pid, _)| pid)
            .ok_or(Failure::IdentityUnknown)?;
        if pid != self.target.pid {
            return Err(Failure::IdentityChanged);
        }
        let mut fields = rest.split_whitespace();
        let state = fields.next().ok_or(Failure::IdentityUnknown)?;
        if matches!(state, "Z" | "X" | "x") {
            return Err(Failure::Gone);
        }
        // The remainder starts at stat field 3; flags and starttime are fields 9 and 22.
        let flags = fields
            .nth(5)
            .and_then(|raw| raw.parse::<u64>().ok())
            .ok_or(Failure::IdentityUnknown)?;
        let start = fields
            .nth(12)
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|start| *start > 0)
            .ok_or(Failure::IdentityUnknown)?;
        Ok((start, flags & 0x0020_0000 != 0))
    }

    fn collect(&self, mut after: impl FnMut(Census)) -> Result<Reading, Failure> {
        let mut budget = Budget { retained: 0 };
        let before = self.identity()?;
        let threads = self.threads(&mut budget, before);
        after(Census::Threads);
        self.identity()?;
        let before = self.identity()?;
        let files = self.files(&mut budget, before);
        after(Census::Files);
        self.identity()?;
        let before = self.identity()?;
        let cwd = self.cwd(&mut budget, before);
        after(Census::Cwd);
        self.identity()?;
        Ok(Reading {
            threads,
            files,
            cwd,
        })
    }

    fn threads(&self, budget: &mut Budget, identity: Identity) -> Result<Vec<Entry>, Failure> {
        let directory = openat(&self.directory, c"task", DIRECTORY_FLAGS, Mode::empty())
            .map_err(|error| hidden_or_entry(error, identity))?;
        let entries = self.census(&directory, budget, |name, budget| {
            let thread =
                openat(&directory, name, DIRECTORY_FLAGS, Mode::empty()).map_err(io_failure)?;
            let bytes = read_bounded(&thread, c"comm", NAME_LIMIT, Limit::Name, self)?;
            let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
            retain_text(bytes, NAME_LIMIT, Limit::Name, budget)
        })?;
        if entries.is_empty() {
            return Err(if identity.kernel_thread {
                Failure::HiddenKernelThreads
            } else {
                Failure::EntryVanished
            });
        }
        Ok(entries)
    }

    fn files(&self, budget: &mut Budget, identity: Identity) -> Result<Vec<Entry>, Failure> {
        let directory = openat(&self.directory, c"fd", DIRECTORY_FLAGS, Mode::empty())
            .map_err(|error| hidden_or_entry(error, identity))?;
        self.census(&directory, budget, |name, budget| {
            read_link(&directory, name, budget, self)
        })
    }

    fn cwd(&self, budget: &mut Budget, identity: Identity) -> Result<String, Failure> {
        read_link(&self.directory, c"cwd", budget, self).map_err(|failure| {
            if identity.kernel_thread && failure == Failure::EntryVanished {
                Failure::HiddenKernelThreads
            } else {
                failure
            }
        })
    }

    fn census(
        &self,
        directory: &OwnedFd,
        budget: &mut Budget,
        mut value: impl FnMut(&CStr, &mut Budget) -> Result<String, Failure>,
    ) -> Result<Vec<Entry>, Failure> {
        let directory = Dir::read_from(directory).map_err(io_failure)?;
        let mut entries = Vec::new();
        let mut visited = 0usize;
        for entry in directory {
            self.check()?;
            let entry = entry.map_err(io_failure)?;
            let name = entry.file_name();
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            visited += 1;
            if visited > ENTRY_LIMIT {
                return Err(Failure::Limit(Limit::Entries));
            }
            let id = numeric_id(name.to_bytes())?;
            budget.retain(std::mem::size_of::<Entry>())?;
            let value = value(name, budget)?;
            entries.push(Entry { id, value });
        }
        entries.sort_unstable_by_key(|entry| entry.id);
        Ok(entries)
    }
}

fn numeric_id(bytes: &[u8]) -> Result<u32, Failure> {
    if bytes.len() > 10 {
        return Err(Failure::Limit(Limit::Name));
    }
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return Err(Failure::Refused);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| Failure::Refused)?;
    let id = text.parse::<u32>().map_err(|_| Failure::Refused)?;
    if text.len() > 1 && text.starts_with('0') {
        return Err(Failure::Refused);
    }
    Ok(id)
}

fn read_input(directory: &OwnedFd, name: &CStr, proc: &Proc<'_>) -> Result<Vec<u8>, Failure> {
    read_bounded(directory, name, INPUT_LIMIT, Limit::Input, proc)
}

fn read_bounded(
    directory: &OwnedFd,
    name: &CStr,
    limit: usize,
    kind: Limit,
    proc: &Proc<'_>,
) -> Result<Vec<u8>, Failure> {
    proc.check()?;
    let fd = openat(
        directory,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(io_failure)?;
    let metadata = rustix::fs::fstat(&fd).map_err(io_failure)?;
    if rustix::fs::FileType::from_raw_mode(metadata.st_mode) != rustix::fs::FileType::RegularFile {
        return Err(Failure::Refused);
    }
    let mut file = std::fs::File::from(fd);
    let mut buffer = [0u8; INPUT_LIMIT + 1];
    let mut len = 0;
    while len <= limit {
        proc.check()?;
        let count = file
            .read(&mut buffer[len..=limit])
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::PermissionDenied => Failure::PermissionDenied,
                std::io::ErrorKind::NotFound => Failure::EntryVanished,
                _ => Failure::Io,
            })?;
        if count == 0 {
            break;
        }
        len += count;
    }
    if len > limit {
        return Err(Failure::Limit(kind));
    }
    Ok(buffer[..len].to_vec())
}

fn read_link(
    directory: &OwnedFd,
    name: &CStr,
    budget: &mut Budget,
    proc: &Proc<'_>,
) -> Result<String, Failure> {
    proc.check()?;
    let mut bytes = [0u8; PATH_LIMIT + 1];
    // readlinkat reads the proc magic link itself, never its target (which may block).
    let len =
        rustix::fs::readlinkat_raw(directory.as_fd(), name, &mut bytes[..]).map_err(io_failure)?;
    retain_text(&bytes[..len], PATH_LIMIT, Limit::Path, budget)
}

fn retain_text(
    bytes: &[u8],
    limit: usize,
    kind: Limit,
    budget: &mut Budget,
) -> Result<String, Failure> {
    if bytes.len() > limit {
        return Err(Failure::Limit(kind));
    }
    // Hex escapes preserve non-UTF-8 paths and prevent control characters from impersonating labels.
    let prefix = if bytes
        .iter()
        .any(|byte| !(0x20..=0x7e).contains(byte) || *byte == b'\\')
    {
        "Escaped bytes: "
    } else {
        ""
    };
    let escaped_len = bytes
        .iter()
        .try_fold(prefix.len(), |len, byte| {
            len.checked_add(if (0x20..=0x7e).contains(byte) && *byte != b'\\' {
                1
            } else {
                4
            })
        })
        .ok_or(Failure::Limit(Limit::Aggregate))?;
    if escaped_len > limit {
        return Err(Failure::Limit(kind));
    }
    budget.retain(escaped_len)?;
    let mut text = String::with_capacity(escaped_len);
    text.push_str(prefix);
    use std::fmt::Write;
    for byte in bytes {
        if (0x20..=0x7e).contains(byte) && *byte != b'\\' {
            text.push(char::from(*byte));
        } else {
            let _ = write!(text, "\\x{byte:02x}");
        }
    }
    Ok(text)
}

fn hidden_or_entry(error: rustix::io::Errno, identity: Identity) -> Failure {
    if error == rustix::io::Errno::NOENT && identity.kernel_thread {
        Failure::HiddenKernelThreads
    } else {
        io_failure(error)
    }
}

fn identity_io_failure(error: rustix::io::Errno) -> Failure {
    identity_failure(io_failure(error))
}

fn identity_failure(failure: Failure) -> Failure {
    if failure == Failure::EntryVanished {
        Failure::Gone
    } else {
        failure
    }
}

pub(super) fn io_failure(error: rustix::io::Errno) -> Failure {
    match error {
        rustix::io::Errno::ACCESS | rustix::io::Errno::PERM => Failure::PermissionDenied,
        rustix::io::Errno::NOENT | rustix::io::Errno::SRCH => Failure::EntryVanished,
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => Failure::Refused,
        _ => Failure::Io,
    }
}
