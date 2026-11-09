//! Remembers only the active page under an app-private `XDG_STATE_HOME` root.
//!
//! Process selection/details, sort/filter state, queries, window geometry and
//! sidebar presentation deliberately do not survive restart.
//!
//! Reads accept the v1 body alone or its exact shipped framework v1 envelope.
//! Writes use the body alone; a framed load migrates on the next ordinary page
//! save, including an unchanged page. Decoding never publishes or repairs bytes.

use std::path::{Path, PathBuf};

use enzui_core::persist::{Persistable, PersistenceKind, RawByteLimit, XdgResolver};

mod authority;
mod owner;
pub(crate) use owner::{Owner, WAKE_INTERVAL, notices};

use crate::model::Page;

const BODY_V1: &str = "enzui-task-manager-session-v1";
const LEGACY_FRAME_V1: &[u8] = b"enzui-iced task-manager-session v1\nbody\n";
const APP_DIR: &str = "task-manager-app";
const FILE_NAME: &str = "session";

const MAX_PAGE_ID_LEN: usize = "performance".len();

// Schema line + newline + one page record + newline.
const MAX_BODY_BYTES: u64 = (BODY_V1.len() + 1 + "page=".len() + MAX_PAGE_ID_LEN + 1) as u64;

pub(crate) const RAW_BYTE_LIMIT: RawByteLimit =
    RawByteLimit::new(LEGACY_FRAME_V1.len() as u64 + MAX_BODY_BYTES);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Session {
    /// `None` means no page was recorded; the caller keeps its launch default.
    pub(crate) page: Option<Page>,
}

fn validate_body(body: &str) -> Result<(), String> {
    if body.len() as u64 > MAX_BODY_BYTES {
        return Err(format!(
            "body length {} exceeds maximum {MAX_BODY_BYTES}",
            body.len()
        ));
    }
    let mut lines = body.lines();
    let schema = lines
        .next()
        .ok_or_else(|| "missing schema line".to_owned())?;
    if schema != BODY_V1 {
        return Err(format!("unsupported schema {schema:?}"));
    }
    let mut page_count: u32 = 0;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            return Err(format!("malformed line {line:?}"));
        };
        if name != "page" || Page::from_id(value).is_none() {
            return Err("unknown session field or page identity".into());
        }
        if name == "page" {
            page_count += 1;
            if page_count > 1 {
                return Err("duplicate page record".to_owned());
            }
            if value.len() > MAX_PAGE_ID_LEN {
                return Err(format!(
                    "page id length {} exceeds maximum {MAX_PAGE_ID_LEN}",
                    value.len()
                ));
            }
        }
    }
    Ok(())
}

impl Persistable for Session {
    fn persist_serialize(&self) -> Result<String, String> {
        let mut body = format!("{BODY_V1}\n");
        if let Some(page) = self.page {
            body.push_str("page=");
            body.push_str(page.id());
            body.push('\n');
        }
        validate_body(&body)?;
        Ok(body)
    }

    fn persist_deserialize(body: &str) -> Result<Self, String> {
        validate_body(body)?;
        let mut value = Self::default();
        for line in body.lines().skip(1) {
            if let Some(("page", raw)) = line.split_once('=') {
                value.page = Page::from_id(raw);
            }
        }
        Ok(value)
    }

    fn persist_default() -> Self {
        Self::default()
    }
}

pub(crate) fn store(path: Option<&Path>) -> Owner {
    Owner::new(path)
}

/// Returns `None` when no absolute state root is available.
pub(crate) fn state_path() -> Option<PathBuf> {
    path_under(&XdgResolver::from_env())
}

fn path_under(resolver: &XdgResolver) -> Option<PathBuf> {
    resolver
        .resolve(PersistenceKind::State, APP_DIR, FILE_NAME)
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_codec_accepts_legacy_routes_and_refuses_ambiguous_or_oversized_records() {
        for id in ["apps", "processes"] {
            let decoded = Session::persist_deserialize(&format!("{BODY_V1}\npage={id}\n")).unwrap();
            assert_eq!(decoded.page, Some(Page::Processes));
        }
        for body in [
            format!("{BODY_V1}\npage=apps\npage=services\n"),
            format!("{BODY_V1}\npage=unknown\n"),
            "x".repeat(MAX_BODY_BYTES as usize + 1),
        ] {
            assert!(
                Session::persist_deserialize(&body).is_err(),
                "invalid session accepted"
            );
        }
    }
}
