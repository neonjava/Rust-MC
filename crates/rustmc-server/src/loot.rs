//! Block loot tables: what a broken block drops, from the official 26.3
//! data.
//!
//! Why: drops differ per block, per tool (Silk Touch, shears, Fortune) and
//! per block state; vanilla defines all of it in data, so RustMC evaluates
//! that data rather than keeping its own lists.
//! Done: the entry, condition and modifier types the 1,201 vanilla block
//! tables use: `item` and `alternatives` entries; `survives_explosion`,
//! `match_block`, `location_check` (a neighbouring block),
//! `match_tool` (items, item tags, enchantments), predicate
//! references such as `minecraft:tool/can_silk_touch`, `table_bonus`,
//! `random_chance`, `inverted`, `any_of`, `all_of` and `entity_properties`
//! conditions; `set_count`, `limit_count`, `apply_bonus` (all three
//! formulas) and `explosion_decay` modifiers; constant, uniform and binomial
//! numbers.
//! Not done: `copy_components` and `copy_state` (block-entity data and states
//! copied into the item) are skipped, so such drops are plain items; the
//! `dynamic` entry (block-entity contents) drops nothing; explosions do not
//! exist yet, so explosion conditions pass. Whether a block needs the right
//! tool to drop anything is a block property no data report contains, so it
//! is not applied.
//!
//! Sources: minecraft.wiki `Loot table`, `Predicate` and `Item modifier`;
//! the official data in `data/minecraft/loot_table/blocks`,
//! `data/minecraft/predicate` and `data/minecraft/tags/item`.

mod load;

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::vanilla::random::RandomSource;

pub use load::{LootError, LootTables};

/// A number provider.
#[derive(Debug, Clone, PartialEq)]
pub enum Number {
    Constant(f32),
    Uniform { min: f32, max: f32 },
    Binomial { n: u32, p: f32 },
}

impl Number {
    fn roll(&self, random: &mut RandomSource) -> f32 {
        match *self {
            Self::Constant(value) => value,
            Self::Uniform { min, max } => min + random.next_float() * (max - min),
            Self::Binomial { n, p } => (0..n).filter(|_| random.next_float() < p).count() as f32,
        }
    }

    /// Rolled and rounded down, as vanilla's integer numbers are.
    fn roll_int(&self, random: &mut RandomSource) -> i32 {
        match self {
            // A uniform integer includes both ends.
            Self::Uniform { min, max } => {
                let (low, high) = (min.floor() as i32, max.floor() as i32);
                if high <= low {
                    low
                } else {
                    low + random.next_int_bounded((high - low + 1) as u32) as i32
                }
            }
            other => other.roll(random).floor() as i32,
        }
    }
}

/// A test on the tool that broke the block.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolPredicate {
    /// Item names the tool must be one of; `None` for any.
    pub items: Option<HashSet<String>>,
    /// Enchantments with the minimum level each needs.
    pub enchantments: Vec<(String, u8)>,
}

/// A loot condition.
#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    SurvivesExplosion,
    MatchBlock {
        block: String,
        state: BTreeMap<String, String>,
    },
    MatchTool(ToolPredicate),
    /// The block at an offset from the broken one has this name and state.
    LocationBlock {
        offset: (i32, i32, i32),
        block: String,
        state: BTreeMap<String, String>,
    },
    TableBonus {
        enchantment: String,
        chances: Vec<f32>,
    },
    RandomChance(f32),
    /// An entity is breaking the block.
    EntityPresent,
    Inverted(Box<Condition>),
    AnyOf(Vec<Condition>),
    AllOf(Vec<Condition>),
}

/// An item modifier.
#[derive(Debug, Clone, PartialEq)]
pub enum Modifier {
    SetCount {
        count: Number,
        add: bool,
    },
    LimitCount {
        min: Option<i32>,
        max: Option<i32>,
    },
    Bonus {
        enchantment: String,
        formula: Bonus,
    },
    ExplosionDecay,
    /// A modifier with its own conditions.
    Conditional {
        conditions: Vec<Condition>,
        modifier: Box<Modifier>,
    },
    /// Copies data RustMC does not model yet (`copy_components`,
    /// `copy_state`); the item is dropped without it.
    Skipped,
}

/// The three `apply_bonus` formulas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bonus {
    OreDrops,
    UniformBonusCount { multiplier: f32 },
    BinomialWithBonusCount { extra: u32, probability: f32 },
}

/// A loot entry.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Item {
        name: String,
        conditions: Vec<Condition>,
        modifiers: Vec<Modifier>,
    },
    /// The first child whose conditions pass.
    Alternatives {
        children: Vec<Entry>,
        conditions: Vec<Condition>,
    },
    /// Block-entity contents, not modelled: drops nothing.
    Dynamic,
}

/// A pool: some rolls, each choosing among entries.
#[derive(Debug, Clone, PartialEq)]
pub struct Pool {
    pub rolls: Number,
    pub conditions: Vec<Condition>,
    pub entries: Vec<Entry>,
}

/// A block's loot table.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Table {
    pub pools: Vec<Pool>,
}

/// A block's name and state properties.
pub type BlockState = (String, BTreeMap<String, String>);
/// Looks up the block at an offset from the broken one.
pub type Neighbour = dyn Fn((i32, i32, i32)) -> Option<BlockState>;

/// What a drop depends on.
pub struct Context<'a> {
    /// The broken block's name and state properties.
    pub block: &'a str,
    pub state: &'a BTreeMap<String, String>,
    /// The tool's item name and enchantment levels; `None` for an empty hand.
    pub tool: Option<(&'a str, &'a BTreeMap<String, u8>)>,
    pub random: &'a mut RandomSource,
    /// The name and state of the block at an offset from the broken one.
    pub neighbour: &'a Neighbour,
}

impl Context<'_> {
    fn enchantment(&self, name: &str) -> u8 {
        self.tool
            .and_then(|(_, levels)| levels.get(name).copied())
            .unwrap_or(0)
    }
}

impl Condition {
    fn test(&self, context: &mut Context<'_>) -> bool {
        match self {
            // Nothing explodes yet: every drop survives.
            Self::SurvivesExplosion | Self::EntityPresent => true,
            Self::MatchBlock { block, state } => {
                block == context.block
                    && state
                        .iter()
                        .all(|(key, value)| context.state.get(key) == Some(value))
            }
            Self::LocationBlock {
                offset,
                block,
                state,
            } => (context.neighbour)(*offset).is_some_and(|(name, properties)| {
                name == *block
                    && state
                        .iter()
                        .all(|(key, value)| properties.get(key) == Some(value))
            }),
            Self::MatchTool(predicate) => {
                let Some((item, levels)) = context.tool else {
                    return false;
                };
                predicate
                    .items
                    .as_ref()
                    .is_none_or(|items| items.contains(item))
                    && predicate
                        .enchantments
                        .iter()
                        .all(|(name, min)| levels.get(name).is_some_and(|level| level >= min))
            }
            Self::TableBonus {
                enchantment,
                chances,
            } => {
                let level = usize::from(context.enchantment(enchantment));
                let chance = chances
                    .get(level)
                    .or(chances.last())
                    .copied()
                    .unwrap_or(0.0);
                context.random.next_float() < chance
            }
            Self::RandomChance(chance) => context.random.next_float() < *chance,
            Self::Inverted(inner) => !inner.test(context),
            Self::AnyOf(terms) => terms.iter().any(|term| term.test(context)),
            Self::AllOf(terms) => terms.iter().all(|term| term.test(context)),
        }
    }
}

fn all_pass(conditions: &[Condition], context: &mut Context<'_>) -> bool {
    conditions.iter().all(|condition| condition.test(context))
}

impl Modifier {
    fn apply(&self, count: i32, context: &mut Context<'_>) -> i32 {
        match self {
            Self::SetCount { count: number, add } => {
                let value = number.roll_int(context.random);
                if *add { count + value } else { value }
            }
            Self::LimitCount { min, max } => {
                let low = min.map_or(count, |min| count.max(min));
                max.map_or(low, |max| low.min(max))
            }
            Self::Bonus {
                enchantment,
                formula,
            } => {
                let level = i32::from(context.enchantment(enchantment));
                if level == 0 {
                    return count;
                }
                match *formula {
                    Bonus::OreDrops => {
                        let extra = context.random.next_int_bounded((level + 2) as u32) as i32 - 1;
                        count * (extra.max(0) + 1)
                    }
                    Bonus::UniformBonusCount { multiplier } => {
                        let bound = (level as f32 * multiplier).round() as u32;
                        count + context.random.next_int_bounded(bound + 1) as i32
                    }
                    Bonus::BinomialWithBonusCount { extra, probability } => {
                        let trials = level as u32 + extra;
                        count
                            + (0..trials)
                                .filter(|_| context.random.next_float() < probability)
                                .count() as i32
                    }
                }
            }
            Self::Conditional {
                conditions,
                modifier,
            } => {
                if all_pass(conditions, context) {
                    modifier.apply(count, context)
                } else {
                    count
                }
            }
            Self::ExplosionDecay | Self::Skipped => count,
        }
    }
}

impl Entry {
    /// Adds this entry's drops; `false` when its conditions failed, which
    /// lets `alternatives` try the next child.
    fn expand(&self, context: &mut Context<'_>, drops: &mut Vec<(String, u32)>) -> bool {
        match self {
            Self::Item {
                name,
                conditions,
                modifiers,
            } => {
                if !all_pass(conditions, context) {
                    return false;
                }
                let count = modifiers
                    .iter()
                    .fold(1, |count, modifier| modifier.apply(count, context));
                if let Ok(count) = u32::try_from(count)
                    && count > 0
                {
                    drops.push((name.clone(), count));
                }
                true
            }
            Self::Alternatives {
                children,
                conditions,
            } => {
                if !all_pass(conditions, context) {
                    return false;
                }
                children.iter().any(|child| child.expand(context, drops))
            }
            Self::Dynamic => true,
        }
    }
}

impl Table {
    /// The items (name and count) this table drops in `context`.
    pub fn roll(&self, context: &mut Context<'_>) -> Vec<(String, u32)> {
        let mut drops = Vec::new();
        for pool in &self.pools {
            if !all_pass(&pool.conditions, context) {
                continue;
            }
            for _ in 0..pool.rolls.roll_int(context.random).max(0) {
                // Every 26.3 block pool has exactly one entry and no
                // weights (the loader refuses anything else), so vanilla's
                // weighted choice among passing entries is that entry.
                for entry in &pool.entries {
                    if entry.expand(context, &mut drops) {
                        break;
                    }
                }
            }
        }
        drops
    }
}

/// Block names whose tables were loaded, keyed by `minecraft:` name.
pub type BlockTables = HashMap<String, Table>;

#[cfg(test)]
mod tests;
