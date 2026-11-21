//! Operators: who may run privileged commands, kept in vanilla's `ops.json`
//! format so an existing server's list carries over.
//!
//! Why: permissions must survive restarts and be editable by hand, and
//! operators moving from a vanilla server should not have to re-grant them.
//! Done: reading and atomically writing the list; levels 1 to 4.
//! Not done: `bypassesPlayerLimit` is kept but not acted on.
//!
//! Source: minecraft.wiki `Ops.json` (an array of objects with `uuid`,
//! `name`, `level` and `bypassesPlayerLimit`) and `Permission level` (levels
//! 0 to 4; operators get the server's operator level, 4 by default).

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::net::identity::uuid_text;
use crate::storage::region_file::replace_atomically;

/// Level given to a new operator: vanilla's default `op-permission-level`.
pub const DEFAULT_OP_LEVEL: u8 = 4;
/// Highest permission level.
pub const MAX_LEVEL: u8 = 4;

/// One operator entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operator {
    pub uuid: [u8; 16],
    pub name: String,
    pub level: u8,
    pub bypasses_player_limit: bool,
}

/// Why the operator file could not be used.
#[derive(Debug)]
pub enum OpsError {
    Io(PathBuf, std::io::Error),
    Malformed(PathBuf, String),
}

impl fmt::Display for OpsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Malformed(path, what) => write!(f, "{}: {what}", path.display()),
        }
    }
}

/// The operator list and where it is saved.
#[derive(Debug)]
pub struct Ops {
    path: PathBuf,
    operators: Vec<Operator>,
}

impl Ops {
    /// Reads the list; a missing file is an empty list.
    pub fn load(path: &Path) -> Result<Self, OpsError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(OpsError::Io(path.to_path_buf(), error)),
        };
        let operators = if text.trim().is_empty() {
            Vec::new()
        } else {
            parse(&text).map_err(|what| OpsError::Malformed(path.to_path_buf(), what))?
        };
        Ok(Self {
            path: path.to_path_buf(),
            operators,
        })
    }

    /// The permission level of `uuid`: its operator level, else 0.
    pub fn level(&self, uuid: &[u8; 16]) -> u8 {
        self.operators
            .iter()
            .find(|op| op.uuid == *uuid)
            .map_or(0, |op| op.level)
    }

    /// Makes `uuid` an operator at the default level; `false` if it already
    /// was one.
    pub fn grant(&mut self, uuid: [u8; 16], name: &str) -> Result<bool, OpsError> {
        if self.operators.iter().any(|op| op.uuid == uuid) {
            return Ok(false);
        }
        self.operators.push(Operator {
            uuid,
            name: name.to_owned(),
            level: DEFAULT_OP_LEVEL,
            bypasses_player_limit: false,
        });
        self.save().map(|()| true)
    }

    /// Removes `uuid` from the operators; `false` if it was not one.
    pub fn revoke(&mut self, uuid: &[u8; 16]) -> Result<bool, OpsError> {
        let before = self.operators.len();
        self.operators.retain(|op| op.uuid != *uuid);
        if self.operators.len() == before {
            return Ok(false);
        }
        self.save().map(|()| true)
    }

    fn save(&self) -> Result<(), OpsError> {
        let list: Vec<Value> = self
            .operators
            .iter()
            .map(|op| {
                json!({
                    "uuid": uuid_text(&op.uuid),
                    "name": op.name,
                    "level": op.level,
                    "bypassesPlayerLimit": op.bypasses_player_limit,
                })
            })
            .collect();
        let text = serde_json::to_string_pretty(&list)
            .map_err(|error| OpsError::Malformed(self.path.clone(), error.to_string()))?;
        replace_atomically(&self.path, &[text.as_bytes()])
            .map_err(|error| OpsError::Io(self.path.clone(), error))
    }
}

fn parse(text: &str) -> Result<Vec<Operator>, String> {
    let value: Value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let entries = value.as_array().ok_or("not a JSON array")?;
    entries
        .iter()
        .map(|entry| {
            let uuid = entry
                .get("uuid")
                .and_then(Value::as_str)
                .and_then(parse_uuid)
                .ok_or("an entry has no valid uuid")?;
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .ok_or("an entry has no name")?
                .to_owned();
            let level = entry
                .get("level")
                .and_then(Value::as_u64)
                .and_then(|level| u8::try_from(level).ok())
                .filter(|level| (1..=MAX_LEVEL).contains(level))
                .ok_or("an entry has no level from 1 to 4")?;
            let bypasses_player_limit = entry
                .get("bypassesPlayerLimit")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(Operator {
                uuid,
                name,
                level,
                bypasses_player_limit,
            })
        })
        .collect()
}

/// Parses the hyphenated UUID form `uuid_text` writes.
fn parse_uuid(text: &str) -> Option<[u8; 16]> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || text.len() != 36 {
        return None;
    }
    let mut uuid = [0u8; 16];
    for (index, byte) in uuid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(uuid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("rustmc-ops-{}-{name}", std::process::id()))
            .join("ops.json")
    }

    #[test]
    fn a_vanilla_list_loads_and_grants_and_revokes_are_saved() {
        let path = temp_file("roundtrip");
        std::fs::create_dir_all(path.parent().expect("dir")).expect("dir");
        std::fs::write(
            &path,
            r#"[{"uuid":"0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0","name":"Admin","level":3,"bypassesPlayerLimit":true}]"#,
        )
        .expect("write");
        let mut ops = Ops::load(&path).expect("load");
        let admin = parse_uuid("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0").expect("uuid");
        assert_eq!(ops.level(&admin), 3);
        let other = [7; 16];
        assert_eq!(ops.level(&other), 0);
        assert!(ops.grant(other, "Other").expect("grant"));
        assert!(!ops.grant(other, "Other").expect("grant again"));
        let reloaded = Ops::load(&path).expect("reload");
        assert_eq!(reloaded.level(&other), DEFAULT_OP_LEVEL);
        assert_eq!(reloaded.level(&admin), 3, "existing entry kept");
        let mut reloaded = reloaded;
        assert!(reloaded.revoke(&admin).expect("revoke"));
        assert_eq!(Ops::load(&path).expect("reload").level(&admin), 0);
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[test]
    fn missing_files_are_empty_and_bad_entries_are_errors() {
        let ops = Ops::load(&temp_file("missing")).expect("missing is empty");
        assert_eq!(ops.level(&[1; 16]), 0);
        assert!(parse(r#"{"not":"a list"}"#).is_err());
        assert!(parse(r#"[{"uuid":"bad","name":"x","level":4}]"#).is_err());
        assert!(
            parse(r#"[{"uuid":"0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0","name":"x","level":9}]"#)
                .is_err()
        );
        assert_eq!(
            uuid_text(&parse_uuid("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0").expect("uuid")),
            "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0"
        );
    }
}
