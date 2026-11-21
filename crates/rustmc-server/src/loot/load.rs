//! Reading block loot tables, the predicates they reference and the item
//! tags their tool tests name, from the official data directory.
//!
//! Anything the evaluator would not handle exactly (an unknown entry,
//! condition or modifier type, weighted or multi-entry pools) is a load
//! error naming the file, never a silent approximation.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{Bonus, Condition, Entry, Modifier, Number, Pool, Table, ToolPredicate};

/// Nesting allowed for tags that include tags, against cycles.
const MAX_TAG_DEPTH: usize = 16;

/// Why the loot data could not be loaded.
#[derive(Debug)]
pub enum LootError {
    Io(PathBuf, std::io::Error),
    Invalid(PathBuf, String),
}

impl fmt::Display for LootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Invalid(path, what) => write!(f, "{}: {what}", path.display()),
        }
    }
}

/// Every block loot table, by block name.
#[derive(Debug, Default)]
pub struct LootTables {
    blocks: HashMap<String, Table>,
}

impl LootTables {
    /// Loads `<data>/minecraft/loot_table/blocks/*.json`, resolving
    /// predicate references and item tags beside them.
    pub fn load(data: &Path) -> Result<Self, LootError> {
        let root = data.join("minecraft");
        let blocks_dir = root.join("loot_table").join("blocks");
        let mut loader = Loader {
            root,
            file: PathBuf::new(),
        };
        let mut blocks = HashMap::new();
        let entries = std::fs::read_dir(&blocks_dir)
            .map_err(|error| LootError::Io(blocks_dir.clone(), error))?;
        for entry in entries {
            let path = entry
                .map_err(|error| LootError::Io(blocks_dir.clone(), error))?
                .path();
            let Some(name) = path
                .extension()
                .filter(|extension| *extension == "json")
                .and(path.file_stem())
                .and_then(|stem| stem.to_str())
                .map(|stem| format!("minecraft:{stem}"))
            else {
                continue;
            };
            loader.file = path.clone();
            let value = loader.read(&path)?;
            blocks.insert(name, loader.table(&value)?);
        }
        if blocks.is_empty() {
            return Err(LootError::Invalid(
                blocks_dir,
                "no block loot tables".to_owned(),
            ));
        }
        Ok(Self { blocks })
    }

    /// The table of `block` (a `minecraft:` name), if it has one.
    pub fn block(&self, block: &str) -> Option<&Table> {
        self.blocks.get(block)
    }

    /// Number of loaded tables.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// A table set from parsed JSON, for tests.
    #[cfg(test)]
    pub fn from_json(block: &str, json: &str) -> Result<Self, LootError> {
        let mut loader = Loader {
            root: PathBuf::new(),
            file: PathBuf::from(block),
        };
        let value: Value = serde_json::from_str(json)
            .map_err(|error| LootError::Invalid(PathBuf::from(block), error.to_string()))?;
        let table = loader.table(&value)?;
        Ok(Self {
            blocks: HashMap::from([(block.to_owned(), table)]),
        })
    }
}

struct Loader {
    root: PathBuf,
    /// The table being read, for error messages.
    file: PathBuf,
}

impl Loader {
    fn invalid(&self, what: impl Into<String>) -> LootError {
        LootError::Invalid(self.file.clone(), what.into())
    }

    fn read(&self, path: &Path) -> Result<Value, LootError> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| LootError::Io(path.to_path_buf(), error))?;
        serde_json::from_str(&text)
            .map_err(|error| LootError::Invalid(path.to_path_buf(), error.to_string()))
    }

    fn table(&mut self, value: &Value) -> Result<Table, LootError> {
        let pools = match value.get("pools") {
            Some(Value::Array(pools)) => pools
                .iter()
                .map(|pool| self.pool(pool))
                .collect::<Result<_, _>>()?,
            None => Vec::new(),
            Some(_) => return Err(self.invalid("pools is not a list")),
        };
        Ok(Table { pools })
    }

    fn pool(&mut self, value: &Value) -> Result<Pool, LootError> {
        if value.get("bonus_rolls").is_some() {
            return Err(self.invalid("bonus_rolls is not supported"));
        }
        let entries = value
            .get("entries")
            .and_then(Value::as_array)
            .ok_or_else(|| self.invalid("pool without entries"))?;
        if entries.len() != 1 {
            return Err(self.invalid("pools with several entries are not supported"));
        }
        Ok(Pool {
            rolls: self.number(
                value
                    .get("rolls")
                    .ok_or_else(|| self.invalid("pool without rolls"))?,
            )?,
            conditions: self.conditions(value)?,
            entries: entries
                .iter()
                .map(|entry| self.entry(entry))
                .collect::<Result<_, _>>()?,
        })
    }

    fn entry(&mut self, value: &Value) -> Result<Entry, LootError> {
        if value.get("weight").is_some() || value.get("quality").is_some() {
            return Err(self.invalid("weighted entries are not supported"));
        }
        match type_of(value) {
            Some("minecraft:item") => Ok(Entry::Item {
                name: value
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| self.invalid("item entry without a name"))?
                    .to_owned(),
                conditions: self.conditions(value)?,
                modifiers: self.modifiers(value)?,
            }),
            Some("minecraft:alternatives") => Ok(Entry::Alternatives {
                children: value
                    .get("children")
                    .and_then(Value::as_array)
                    .ok_or_else(|| self.invalid("alternatives without children"))?
                    .iter()
                    .map(|child| self.entry(child))
                    .collect::<Result<_, _>>()?,
                conditions: self.conditions(value)?,
            }),
            Some("minecraft:dynamic") => Ok(Entry::Dynamic),
            other => Err(self.invalid(format!("unsupported entry type {other:?}"))),
        }
    }

    /// The `condition` (one) or `conditions` (a list) of an object.
    fn conditions(&mut self, value: &Value) -> Result<Vec<Condition>, LootError> {
        match (value.get("condition"), value.get("conditions")) {
            (Some(one), None) => Ok(vec![self.condition(one)?]),
            (None, Some(Value::Array(many))) => many.iter().map(|c| self.condition(c)).collect(),
            (None, None) => Ok(Vec::new()),
            _ => Err(self.invalid("malformed conditions")),
        }
    }

    fn condition(&mut self, value: &Value) -> Result<Condition, LootError> {
        // A string names a predicate file: `ns:path` is
        // `predicate/<path>.json`.
        if let Some(reference) = value.as_str() {
            let path = reference.strip_prefix("minecraft:").unwrap_or(reference);
            let file = self.root.join("predicate").join(format!("{path}.json"));
            let predicate = self.read(&file)?;
            return self.condition(&predicate);
        }
        let terms = |loader: &mut Self, key: &str| -> Result<Vec<Condition>, LootError> {
            value
                .get(key)
                .and_then(Value::as_array)
                .ok_or_else(|| loader.invalid(format!("missing {key}")))?
                .iter()
                .map(|term| loader.condition(term))
                .collect()
        };
        Ok(match type_of(value) {
            Some("minecraft:survives_explosion") => Condition::SurvivesExplosion,
            Some("minecraft:random_chance") => Condition::RandomChance(
                value
                    .get("chance")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| self.invalid("random_chance without a chance"))?
                    as f32,
            ),
            Some("minecraft:inverted") => Condition::Inverted(Box::new(
                self.condition(
                    value
                        .get("term")
                        .ok_or_else(|| self.invalid("inverted without a term"))?,
                )?,
            )),
            Some("minecraft:any_of") => Condition::AnyOf(terms(self, "terms")?),
            Some("minecraft:all_of") => Condition::AllOf(terms(self, "terms")?),
            Some("minecraft:match_block") => {
                let (block, state) = self.block_test(value)?;
                Condition::MatchBlock { block, state }
            }
            Some("minecraft:location_check") => {
                let offset = |key| {
                    value
                        .get(key)
                        .and_then(Value::as_i64)
                        .map_or(0, |v| v as i32)
                };
                let test = value
                    .get("predicate")
                    .and_then(|predicate| predicate.get("block"))
                    .filter(|_| {
                        value
                            .get("predicate")
                            .and_then(Value::as_object)
                            .is_some_and(|p| p.len() == 1)
                    })
                    .ok_or_else(|| self.invalid("location_check other than a block test"))?;
                let (block, state) = self.block_test(test)?;
                Condition::LocationBlock {
                    offset: (offset("offsetX"), offset("offsetY"), offset("offsetZ")),
                    block,
                    state,
                }
            }
            Some("minecraft:table_bonus") => Condition::TableBonus {
                enchantment: value
                    .get("enchantment")
                    .and_then(Value::as_str)
                    .ok_or_else(|| self.invalid("table_bonus without an enchantment"))?
                    .to_owned(),
                chances: value
                    .get("chances")
                    .and_then(Value::as_array)
                    .ok_or_else(|| self.invalid("table_bonus without chances"))?
                    .iter()
                    .map(|chance| chance.as_f64().map(|c| c as f32))
                    .collect::<Option<_>>()
                    .ok_or_else(|| self.invalid("table_bonus chance is not a number"))?,
            },
            Some("minecraft:match_tool") => Condition::MatchTool(
                self.tool(
                    value
                        .get("predicate")
                        .ok_or_else(|| self.invalid("match_tool without a predicate"))?,
                )?,
            ),
            // Only the empty predicate on the breaking entity occurs in
            // block tables: an entity is present.
            Some("minecraft:entity_properties")
                if value
                    .get("predicate")
                    .is_some_and(|p| p.as_object().is_some_and(|p| p.is_empty())) =>
            {
                Condition::EntityPresent
            }
            other => return Err(self.invalid(format!("unsupported condition {other:?}"))),
        })
    }

    /// A `blocks` name with exact `state` properties.
    fn block_test(&self, value: &Value) -> Result<(String, BTreeMap<String, String>), LootError> {
        let block = value
            .get("blocks")
            .and_then(Value::as_str)
            .ok_or_else(|| self.invalid("block test without one block"))?
            .to_owned();
        let state = match value.get("state") {
            Some(Value::Object(state)) => state
                .iter()
                .map(|(key, value)| match value {
                    Value::String(text) => Ok((key.clone(), text.clone())),
                    _ => Err(self.invalid("block state ranges are not supported")),
                })
                .collect::<Result<BTreeMap<_, _>, _>>()?,
            None => BTreeMap::new(),
            Some(_) => return Err(self.invalid("malformed block state")),
        };
        Ok((block, state))
    }

    fn tool(&mut self, value: &Value) -> Result<ToolPredicate, LootError> {
        let mut predicate = ToolPredicate::default();
        for (key, field) in value
            .as_object()
            .ok_or_else(|| self.invalid("malformed tool predicate"))?
        {
            match key.as_str() {
                "items" => predicate.items = Some(self.items(field)?),
                "predicates" => {
                    for (component, test) in field
                        .as_object()
                        .ok_or_else(|| self.invalid("malformed predicates"))?
                    {
                        if component != "minecraft:enchantments" {
                            return Err(
                                self.invalid(format!("unsupported tool predicate {component}"))
                            );
                        }
                        for wanted in test
                            .as_array()
                            .ok_or_else(|| self.invalid("malformed enchantments"))?
                        {
                            let name = wanted
                                .get("enchantments")
                                .and_then(Value::as_str)
                                .ok_or_else(|| {
                                    self.invalid("enchantment test without one enchantment")
                                })?;
                            let min = wanted
                                .get("levels")
                                .and_then(|levels| levels.get("min"))
                                .and_then(Value::as_u64)
                                .and_then(|min| u8::try_from(min).ok())
                                .unwrap_or(1);
                            predicate.enchantments.push((name.to_owned(), min));
                        }
                    }
                }
                other => return Err(self.invalid(format!("unsupported tool test {other}"))),
            }
        }
        Ok(predicate)
    }

    /// An item list: one name, a `#tag`, or a list of names.
    fn items(&mut self, value: &Value) -> Result<HashSet<String>, LootError> {
        let mut items = HashSet::new();
        match value {
            Value::String(name) => self.add_item(name, &mut items, 0)?,
            Value::Array(names) => {
                for name in names {
                    let name = name
                        .as_str()
                        .ok_or_else(|| self.invalid("item name is not text"))?;
                    self.add_item(name, &mut items, 0)?;
                }
            }
            _ => return Err(self.invalid("malformed items")),
        }
        Ok(items)
    }

    fn add_item(
        &mut self,
        name: &str,
        items: &mut HashSet<String>,
        depth: usize,
    ) -> Result<(), LootError> {
        let Some(tag) = name.strip_prefix('#') else {
            items.insert(name.to_owned());
            return Ok(());
        };
        if depth > MAX_TAG_DEPTH {
            return Err(self.invalid(format!("item tag {tag} nests too deeply")));
        }
        let path = tag.strip_prefix("minecraft:").unwrap_or(tag);
        let file = self
            .root
            .join("tags")
            .join("item")
            .join(format!("{path}.json"));
        let value = self.read(&file)?;
        for entry in value
            .get("values")
            .and_then(Value::as_array)
            .ok_or_else(|| self.invalid("tag without values"))?
        {
            // Optional entries are objects with an `id`.
            let id = entry
                .as_str()
                .or_else(|| entry.get("id").and_then(Value::as_str))
                .ok_or_else(|| self.invalid("malformed tag entry"))?;
            self.add_item(id, items, depth + 1)?;
        }
        Ok(())
    }

    /// The `modifier` (one or a list) of an entry.
    fn modifiers(&mut self, value: &Value) -> Result<Vec<Modifier>, LootError> {
        match value.get("modifier") {
            None => Ok(Vec::new()),
            Some(Value::Array(many)) => many.iter().map(|m| self.modifier(m)).collect(),
            Some(one) => Ok(vec![self.modifier(one)?]),
        }
    }

    fn modifier(&mut self, value: &Value) -> Result<Modifier, LootError> {
        let modifier = match type_of(value) {
            Some("minecraft:set_count") => Modifier::SetCount {
                count: self.number(
                    value
                        .get("count")
                        .ok_or_else(|| self.invalid("set_count without a count"))?,
                )?,
                add: value.get("add").and_then(Value::as_bool).unwrap_or(false),
            },
            Some("minecraft:limit_count") => {
                let limit = value
                    .get("limit")
                    .ok_or_else(|| self.invalid("limit_count without a limit"))?;
                let bound = |key| {
                    limit
                        .get(key)
                        .and_then(Value::as_i64)
                        .and_then(|v| i32::try_from(v).ok())
                };
                match limit.as_i64() {
                    Some(exact) => {
                        let exact = i32::try_from(exact).ok();
                        Modifier::LimitCount {
                            min: exact,
                            max: exact,
                        }
                    }
                    None => Modifier::LimitCount {
                        min: bound("min"),
                        max: bound("max"),
                    },
                }
            }
            Some("minecraft:apply_bonus") => Modifier::Bonus {
                enchantment: value
                    .get("enchantment")
                    .and_then(Value::as_str)
                    .ok_or_else(|| self.invalid("apply_bonus without an enchantment"))?
                    .to_owned(),
                formula: self.bonus(value)?,
            },
            Some("minecraft:explosion_decay") => Modifier::ExplosionDecay,
            Some("minecraft:copy_components" | "minecraft:copy_state") => Modifier::Skipped,
            other => return Err(self.invalid(format!("unsupported modifier {other:?}"))),
        };
        let conditions = self.conditions(value)?;
        Ok(if conditions.is_empty() {
            modifier
        } else {
            Modifier::Conditional {
                conditions,
                modifier: Box::new(modifier),
            }
        })
    }

    fn bonus(&self, value: &Value) -> Result<Bonus, LootError> {
        let parameter = |key: &str| {
            value
                .get("parameters")
                .and_then(|parameters| parameters.get(key))
                .and_then(Value::as_f64)
                .ok_or_else(|| self.invalid(format!("apply_bonus without {key}")))
        };
        Ok(match value.get("formula").and_then(Value::as_str) {
            Some("minecraft:ore_drops") => Bonus::OreDrops,
            Some("minecraft:uniform_bonus_count") => Bonus::UniformBonusCount {
                multiplier: parameter("bonusMultiplier")? as f32,
            },
            Some("minecraft:binomial_with_bonus_count") => Bonus::BinomialWithBonusCount {
                extra: parameter("extra")? as u32,
                probability: parameter("probability")? as f32,
            },
            other => return Err(self.invalid(format!("unsupported bonus formula {other:?}"))),
        })
    }

    fn number(&self, value: &Value) -> Result<Number, LootError> {
        if let Some(constant) = value.as_f64() {
            return Ok(Number::Constant(constant as f32));
        }
        let field = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_f64)
                .ok_or_else(|| self.invalid(format!("number without a numeric {key}")))
        };
        Ok(match type_of(value) {
            Some("minecraft:constant") => Number::Constant(field("value")? as f32),
            Some("minecraft:uniform") => Number::Uniform {
                min: field("min")? as f32,
                max: field("max")? as f32,
            },
            Some("minecraft:binomial") => Number::Binomial {
                n: field("n")? as u32,
                p: field("p")? as f32,
            },
            other => return Err(self.invalid(format!("unsupported number {other:?}"))),
        })
    }
}

fn type_of(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str)
}
