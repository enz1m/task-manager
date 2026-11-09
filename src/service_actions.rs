use crate::services::ServiceData;
use enzui_core::cancellation::CancelToken;
use enzui_services::process_transport::{
    self, ProcessErrorKind, ProcessOutput, ProcessRequest, StdinPolicy,
};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verb {
    Start,
    Stop,
    Restart,
}
impl Verb {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            "restart" => Some(Self::Restart),
            _ => None,
        }
    }
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
    pub(crate) fn target(self) -> &'static str {
        match self {
            Self::Start | Self::Restart => "active",
            Self::Stop => "inactive",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Denied,
    Unavailable,
    NotFound,
    Changed,
    Failed,
    Timeout,
    Cancelled,
    InvalidUnit,
    Unverifiable,
    CleanupPending,
}
impl Failure {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Denied => "permission denied",
            Self::Unavailable => "service manager or helper unavailable",
            Self::NotFound => "unit not found",
            Self::Changed => "unit identity or prior state changed",
            Self::Failed => "service command or unit failed",
            Self::Timeout => "deadline exceeded",
            Self::Cancelled => "cancelled; a wired command is not undone",
            Self::InvalidUnit => "invalid canonical unit name",
            Self::Unverifiable => "authoritative unit state unavailable",
            Self::CleanupPending => "exact helper reap is still unverified",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sample {
    pub(crate) unit: String,
    pub(crate) fragment: String,
    pub(crate) load: String,
    pub(crate) active: String,
    pub(crate) sub: String,
}
impl Sample {
    pub(crate) fn from_row(row: &ServiceData) -> Result<Self, Failure> {
        if !valid_unit(&row.name) {
            return Err(Failure::InvalidUnit);
        }
        if row.load != "loaded"
            || !matches!(
                row.state.as_str(),
                "active"
                    | "inactive"
                    | "failed"
                    | "activating"
                    | "deactivating"
                    | "reloading"
                    | "maintenance"
                    | "refreshing"
            )
            || row.sub_state.is_empty()
            || row.sub_state.len() > 64
            || !row
                .sub_state
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || row.fragment_path.len() > 4096
            || row.fragment_path.chars().any(char::is_control)
        {
            return Err(Failure::Unverifiable);
        }
        Ok(Self {
            unit: row.name.clone(),
            fragment: row.fragment_path.clone(),
            load: row.load.clone(),
            active: row.state.clone(),
            sub: row.sub_state.clone(),
        })
    }
    pub(crate) fn same_identity(&self, other: &Self) -> bool {
        self.unit == other.unit && self.fragment == other.fragment
    }
}

pub(crate) fn valid_unit(unit: &str) -> bool {
    if unit.len() > 255
        || !unit.ends_with(".service")
        || unit.len() <= ".service".len()
        || unit.starts_with('-')
        || unit.starts_with('.')
    {
        return false;
    }
    let bytes = unit.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\\' {
            if bytes.get(index + 1) != Some(&b'x')
                || !bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit)
                || !bytes.get(index + 3).is_some_and(u8::is_ascii_hexdigit)
            {
                return false;
            }
            index += 4;
        } else if byte.is_ascii_alphanumeric() || b"_.@:-".contains(&byte) {
            index += 1;
        } else {
            return false;
        }
    }
    true
}

pub(crate) trait Backend: Send + Sync {
    fn sample(&self, unit: &str, budget: Duration, cancel: &CancelToken)
    -> Result<Sample, Failure>;
    fn command(
        &self,
        unit: &str,
        verb: Verb,
        budget: Duration,
        cancel: &CancelToken,
    ) -> Result<(), Failure>;
}
pub(crate) struct System;
impl Backend for System {
    fn sample(
        &self,
        unit: &str,
        budget: Duration,
        cancel: &CancelToken,
    ) -> Result<Sample, Failure> {
        if !valid_unit(unit) {
            return Err(Failure::InvalidUnit);
        }
        let output = invoke(
            request(budget).args([
                "--system",
                "--no-pager",
                "--no-ask-password",
                "show",
                "--property=Id,LoadState,ActiveState,SubState,FragmentPath",
                "--",
                unit,
            ]),
            cancel,
        )?;
        let text = std::str::from_utf8(&output.stdout).map_err(|_| Failure::Unverifiable)?;
        let mut properties = std::collections::BTreeMap::new();
        for line in text.lines() {
            let (key, value) = line.split_once('=').ok_or(Failure::Unverifiable)?;
            if properties.insert(key, value).is_some() {
                return Err(Failure::Unverifiable);
            }
        }
        let field = |key| properties.get(key).copied().ok_or(Failure::Unverifiable);
        if field("LoadState")? == "not-found" {
            return Err(Failure::NotFound);
        }
        if field("Id")? != unit {
            return Err(Failure::Changed);
        }
        let row = ServiceData {
            name: field("Id")?.to_owned(),
            fragment_path: field("FragmentPath")?.to_owned(),
            load: field("LoadState")?.to_owned(),
            state: field("ActiveState")?.to_owned(),
            sub_state: field("SubState")?.to_owned(),
            ..Default::default()
        };
        Sample::from_row(&row)
    }
    fn command(
        &self,
        unit: &str,
        verb: Verb,
        budget: Duration,
        cancel: &CancelToken,
    ) -> Result<(), Failure> {
        if !valid_unit(unit) {
            return Err(Failure::InvalidUnit);
        }
        invoke(
            request(budget).args([
                "--system",
                "--no-pager",
                "--no-ask-password",
                verb.as_str(),
                "--",
                unit,
            ]),
            cancel,
        )
        .map(|_| ())
    }
}
fn request(budget: Duration) -> ProcessRequest {
    ProcessRequest::new(
        "/usr/bin/systemctl",
        StdinPolicy::Null,
        16 * 1024,
        16 * 1024,
        budget,
    )
    .env("LC_ALL", "C")
    .current_dir("/")
}
fn invoke(request: ProcessRequest, cancel: &CancelToken) -> Result<ProcessOutput, Failure> {
    let output = process_transport::run(request, cancel).map_err(|error| match error.kind() {
        ProcessErrorKind::Cancelled(_) => Failure::Cancelled,
        ProcessErrorKind::Deadline => Failure::Timeout,
        ProcessErrorKind::Spawn | ProcessErrorKind::ExecutableAuthority => Failure::Unavailable,
        // Setup can fail after spawn; no per-request rescue-reap receipt is exposed.
        ProcessErrorKind::Setup
        | ProcessErrorKind::DirectChildCleanup
        | ProcessErrorKind::ResidualCleanup
        | ProcessErrorKind::GroupSignal
        | ProcessErrorKind::Wait => Failure::CleanupPending,
        _ => Failure::Unverifiable,
    })?;
    if output.status.success() {
        return Ok(output);
    }
    if output.status.code().is_none() {
        return Err(Failure::Unverifiable);
    }
    let diagnostic = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    Err(
        if diagnostic.contains("access denied")
            || diagnostic.contains("permission denied")
            || diagnostic.contains("authentication is required")
            || diagnostic.contains("interactive authentication required")
        {
            Failure::Denied
        } else if diagnostic.contains("could not be found")
            || diagnostic.contains("not found")
            || diagnostic.contains("not loaded")
            || output.status.code() == Some(4)
        {
            Failure::NotFound
        } else if diagnostic.contains("failed to connect to")
            || diagnostic.contains("not been booted with systemd")
        {
            Failure::Unavailable
        } else if (diagnostic.starts_with("job for ")
            || diagnostic.starts_with("a dependency job for "))
            && diagnostic.contains(" failed")
        {
            Failure::Failed
        } else {
            Failure::Unverifiable
        },
    )
}
