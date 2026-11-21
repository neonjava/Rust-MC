//! The item facts the server needs from the official data reports: which
//! items exist and how many fit in one stack.
//!
//! Why: commands such as `give` must build stacks with vanilla's stack
//! sizes (1 for tools, 16 for ender pearls, 64 for most items); guessing them
//! would put wrong stacks into inventories.
//! Done: loading `max_stack_size` from every per-item component report.
//! Not done: the other default components (they are not needed until items
//! are used, in the survival phase).
//!
//! Source: the official 26.3 data generator's
//! `reports/minecraft/components/item/<name>.json`, whose `components` object
//! carries `minecraft:max_stack_size` for every item (1,658 items: 1,342 of
//! 64, 248 of 1 and 68 of 16 in the 26.3 reports).

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Why the item reports could not be loaded.
#[derive(Debug)]
pub enum ItemTableError {
    Io(PathBuf, std::io::Error),
    Malformed(PathBuf, &'static str),
    Empty(PathBuf),
}

impl fmt::Display for ItemTableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Malformed(path, what) => write!(f, "{}: no valid {what}", path.display()),
            Self::Empty(path) => write!(f, "{}: no item reports", path.display()),
        }
    }
}

/// Stack sizes by namespaced item name.
#[derive(Debug, Default)]
pub struct ItemTable {
    max_stack: HashMap<String, u16>,
}

impl ItemTable {
    /// Loads every `<name>.json` in the component report directory.
    pub fn load(directory: &Path) -> Result<Self, ItemTableError> {
        let io = |error| ItemTableError::Io(directory.to_path_buf(), error);
        let mut max_stack = HashMap::new();
        for entry in std::fs::read_dir(directory).map_err(io)? {
            let path = entry.map_err(io)?.path();
            let Some(name) = path
                .extension()
                .filter(|extension| *extension == "json")
                .and(path.file_stem())
                .and_then(|stem| stem.to_str())
            else {
                continue;
            };
            let text = std::fs::read_to_string(&path)
                .map_err(|error| ItemTableError::Io(path.clone(), error))?;
            let size = parse_max_stack(&text)
                .ok_or_else(|| ItemTableError::Malformed(path.clone(), "max_stack_size"))?;
            max_stack.insert(format!("minecraft:{name}"), size);
        }
        if max_stack.is_empty() {
            return Err(ItemTableError::Empty(directory.to_path_buf()));
        }
        Ok(Self { max_stack })
    }

    /// The stack size of `item`, accepting names with or without the
    /// `minecraft:` namespace; `None` for an unknown item.
    pub fn max_stack(&self, item: &str) -> Option<u16> {
        match item.split_once(':') {
            Some(_) => self.max_stack.get(item).copied(),
            None => self.max_stack.get(&format!("minecraft:{item}")).copied(),
        }
    }

    /// Builds a table from `(name, size)` pairs, for tests.
    #[cfg(test)]
    pub fn from_pairs(pairs: &[(&str, u16)]) -> Self {
        Self {
            max_stack: pairs.iter().map(|(n, s)| ((*n).to_owned(), *s)).collect(),
        }
    }
}

fn parse_max_stack(text: &str) -> Option<u16> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let size = value
        .get("components")?
        .get("minecraft:max_stack_size")?
        .as_u64()?;
    u16::try_from(size).ok().filter(|size| *size > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_stack_size_component_and_rejects_reports_without_it() {
        assert_eq!(
            parse_max_stack(r#"{"components":{"minecraft:max_stack_size":16}}"#),
            Some(16)
        );
        assert_eq!(parse_max_stack(r#"{"components":{}}"#), None);
        assert_eq!(
            parse_max_stack(r#"{"components":{"minecraft:max_stack_size":0}}"#),
            None
        );
        assert_eq!(parse_max_stack("not json"), None);
    }

    #[test]
    fn names_resolve_with_or_without_the_namespace() {
        let table = ItemTable::from_pairs(&[("minecraft:diamond_sword", 1)]);
        assert_eq!(table.max_stack("diamond_sword"), Some(1));
        assert_eq!(table.max_stack("minecraft:diamond_sword"), Some(1));
        assert_eq!(table.max_stack("minecraft:unknown"), None);
    }

    #[test]
    fn loads_a_report_directory() {
        let dir = std::env::temp_dir().join(format!("rustmc-items-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(
            dir.join("ender_pearl.json"),
            r#"{"components":{"minecraft:max_stack_size":16}}"#,
        )
        .expect("write");
        std::fs::write(dir.join("notes.txt"), "ignored").expect("write");
        let table = ItemTable::load(&dir).expect("load");
        assert_eq!(table.max_stack("ender_pearl"), Some(16));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
