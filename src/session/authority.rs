use super::{RAW_BYTE_LIMIT, Session};
#[cfg(test)]
use enzui_core::persist::LoadOutcome;
use enzui_core::persist::Persistable;
use enzui_core::state_authority::{CustodySnapshot, PublishCustody, StateAuthority};
use std::io;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    Refused,
    BeforePublication(io::ErrorKind),
    DurabilityDebt,
    Lost,
    Worker,
    Busy,
    Exhausted,
    Join,
}

impl Failure {
    pub(super) fn summary(self) -> &'static str {
        match self {
            Self::Refused => {
                "Saved page refused: invalid, foreign, changed or oversized state. The file has not been replaced."
            }
            Self::BeforePublication(_) => {
                "Page publication refused without changing custody. The newest page remains pending."
            }
            Self::DurabilityDebt => {
                "Page publication is not durably confirmed. Exact retained bytes must settle before Close."
            }
            Self::Lost => {
                "Session file custody was lost. No pathname will be reopened or overwritten; Close is permanently vetoed."
            }
            Self::Worker => "Session worker failed. Save custody is unverified; Close is blocked.",
            Self::Busy => "Session worker is busy. The newest page remains pending for retry.",
            Self::Exhausted => {
                "Session generation exhausted. No further changes can be remembered; Close is blocked."
            }
            Self::Join => "Session owner join is unverified. Close is blocked.",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Encoded {
    bytes: [u8; super::MAX_BODY_BYTES as usize],
    len: usize,
}

impl Encoded {
    pub(super) fn new(record: Session) -> Result<Self, Failure> {
        let body = record.persist_serialize().map_err(|_| Failure::Refused)?;
        let mut encoded = Self {
            bytes: [0; super::MAX_BODY_BYTES as usize],
            len: body.len(),
        };
        if body.len() > encoded.bytes.len() {
            return Err(Failure::Refused);
        }
        encoded.bytes[..body.len()].copy_from_slice(body.as_bytes());
        Ok(encoded)
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Decoded {
    pub(super) record: Session,
    pub(super) migration_pending: bool,
}

enum Backend {
    Unopened(PathBuf),
    File(StateAuthority),
    Memory(Option<Encoded>),
    Refused,
}

pub(super) struct Authority {
    backend: Backend,
    loaded: bool,
    blocked: Option<Failure>,
    debt: Option<(u64, Encoded)>,
}

impl Authority {
    pub(super) fn new(path: Option<PathBuf>) -> Self {
        Self {
            backend: path.map_or(Backend::Memory(None), Backend::Unopened),
            loaded: false,
            blocked: None,
            debt: None,
        }
    }

    fn initialize(&mut self) -> Result<(), Failure> {
        if matches!(self.backend, Backend::Unopened(_)) {
            let Backend::Unopened(path) = std::mem::replace(&mut self.backend, Backend::Refused)
            else {
                unreachable!()
            };
            if !path.is_absolute()
                || path.file_name() != Some(std::ffi::OsStr::new(super::FILE_NAME))
            {
                return Err(Failure::Refused);
            }
            let parent = path.parent().ok_or(Failure::Refused)?;
            enzui_core::secure_dir::ensure_private_dir(parent).map_err(|_| Failure::Refused)?;
            self.backend = Backend::File(
                StateAuthority::open_or_create_tightening(&path).map_err(|_| Failure::Refused)?,
            );
        }
        Ok(())
    }

    pub(super) fn load(&mut self) -> Result<Option<Decoded>, Failure> {
        if let Some(failure) = self.blocked {
            return Err(failure);
        }
        let result = (|| {
            self.initialize()?;
            match &self.backend {
                Backend::File(state) => match state
                    .read_bounded(RAW_BYTE_LIMIT.bytes() as usize)
                    .map_err(|_| Failure::Refused)?
                {
                    CustodySnapshot::Absent => Ok(None),
                    CustodySnapshot::Current {
                        bytes,
                        overflow: false,
                        ..
                    } => decode(&bytes).map(Some),
                    CustodySnapshot::Current { overflow: true, .. } => Err(Failure::Refused),
                    CustodySnapshot::Lost => Err(Failure::Lost),
                },
                Backend::Memory(bytes) => bytes
                    .as_ref()
                    .map(|bytes| decode(bytes.as_bytes()))
                    .transpose(),
                _ => Err(Failure::Refused),
            }
        })();
        match &result {
            Ok(_) => self.loaded = true,
            Err(failure) => self.blocked = Some(*failure),
        }
        result
    }

    pub(super) fn save(&mut self, generation: u64, bytes: Encoded) -> Result<(), Failure> {
        if let Some(failure) = self.blocked {
            return Err(failure);
        }
        if !self.loaded {
            self.load()?;
        }
        if let Some((owed_generation, owed_bytes)) = self.debt {
            // The framework exposes no durability-only settlement; retry the owed bytes
            // through the same authority before admitting a newer publication.
            self.publish(owed_generation, owed_bytes)?;
            if (owed_generation, owed_bytes) == (generation, bytes) {
                return Ok(());
            }
        }
        self.publish(generation, bytes)
    }

    fn publish(&mut self, generation: u64, bytes: Encoded) -> Result<(), Failure> {
        match &mut self.backend {
            Backend::File(state) => {
                if state.generation().checked_add(1).is_none() {
                    self.blocked = Some(Failure::Exhausted);
                    return Err(Failure::Exhausted);
                }
                match state.publish(bytes.as_bytes()) {
                    Ok(_) => {
                        self.debt = None;
                        Ok(())
                    }
                    Err(error) => match error.custody() {
                        PublishCustody::Unchanged => Err(if self.debt.is_some() {
                            Failure::DurabilityDebt
                        } else {
                            Failure::BeforePublication(error.io_error().kind())
                        }),
                        PublishCustody::Rotated => {
                            self.debt = Some((generation, bytes));
                            Err(Failure::DurabilityDebt)
                        }
                        PublishCustody::Lost => {
                            self.blocked = Some(Failure::Lost);
                            Err(Failure::Lost)
                        }
                        _ => {
                            self.blocked = Some(Failure::Lost);
                            Err(Failure::Lost)
                        }
                    },
                }
            }
            Backend::Memory(current) => {
                *current = Some(bytes);
                Ok(())
            }
            _ => Err(Failure::Refused),
        }
    }

    #[cfg(test)]
    pub(super) fn load_outcome(&mut self) -> LoadOutcome<Session> {
        match self.load() {
            Ok(Some(value)) => LoadOutcome::Loaded(value.record),
            Ok(None) => LoadOutcome::Absent {
                default: Session::default(),
            },
            Err(failure) => LoadOutcome::Refused {
                fallback: Session::default(),
                reason: enzui_core::atomic_state::StateRefusal::OpenRefused,
                diagnostic: failure.summary().into(),
            },
        }
    }

    #[cfg(test)]
    pub(super) fn refused(&self) -> bool {
        self.blocked.is_some()
    }
}

fn decode(bytes: &[u8]) -> Result<Decoded, Failure> {
    if bytes.len() as u64 > RAW_BYTE_LIMIT.bytes() {
        return Err(Failure::Refused);
    }
    let framed_body = bytes.strip_prefix(super::LEGACY_FRAME_V1);
    let body = std::str::from_utf8(framed_body.unwrap_or(bytes)).map_err(|_| Failure::Refused)?;
    Ok(Decoded {
        record: Session::persist_deserialize(body).map_err(|_| Failure::Refused)?,
        migration_pending: framed_body.is_some(),
    })
}
