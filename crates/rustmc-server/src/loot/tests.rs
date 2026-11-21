//! Unit tests for block loot tables.

use std::collections::BTreeMap;

use super::*;
use crate::vanilla::random::RandomSource;

/// The shape of vanilla's stone table: Silk Touch keeps the block,
/// anything else drops cobblestone.
const STONE: &str = r#"{"type":"minecraft:block","pools":[{"rolls":1,"entries":[{"type":"minecraft:alternatives","children":[
  {"type":"minecraft:item","condition":{"type":"minecraft:match_tool","predicate":{"predicates":{"minecraft:enchantments":[{"enchantments":"minecraft:silk_touch","levels":{"min":1}}]}}},"name":"minecraft:stone"},
  {"type":"minecraft:item","condition":{"type":"minecraft:survives_explosion"},"name":"minecraft:cobblestone"}]}]}]}"#;

/// A door drops only from its lower half.
const DOOR: &str = r#"{"type":"minecraft:block","pools":[{"rolls":1,"entries":[{"type":"minecraft:item",
  "condition":{"type":"minecraft:match_block","blocks":"minecraft:oak_door","state":{"half":"lower"}},"name":"minecraft:oak_door"}]}]}"#;

/// Ore drops with a uniform count and Fortune.
const ORE: &str = r#"{"type":"minecraft:block","pools":[{"rolls":1,"entries":[{"type":"minecraft:item","modifier":[
  {"type":"minecraft:set_count","count":{"type":"minecraft:uniform","min":4,"max":5}},
  {"type":"minecraft:apply_bonus","enchantment":"minecraft:fortune","formula":"minecraft:uniform_bonus_count","parameters":{"bonusMultiplier":1}},
  {"type":"minecraft:explosion_decay"}],"name":"minecraft:redstone"}]}]}"#;

fn roll(
    json: &str,
    block: &str,
    state: &[(&str, &str)],
    tool: Option<(&str, &[(&str, u8)])>,
    seed: u64,
) -> Vec<(String, u32)> {
    let tables = LootTables::from_json(block, json).expect("table");
    let state: BTreeMap<String, String> = state
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    let levels: BTreeMap<String, u8> = tool
        .map(|(_, levels)| levels.iter().map(|(n, l)| ((*n).to_owned(), *l)).collect())
        .unwrap_or_default();
    let mut random = RandomSource::from_world_seed(seed);
    let mut context = Context {
        block,
        state: &state,
        tool: tool.map(|(item, _)| (item, &levels)),
        random: &mut random,
        neighbour: &|_| None,
    };
    tables.block(block).expect("block").roll(&mut context)
}

#[test]
fn silk_touch_chooses_the_first_alternative() {
    assert_eq!(
        roll(STONE, "minecraft:stone", &[], None, 1),
        [("minecraft:cobblestone".to_owned(), 1)]
    );
    let silk = [("minecraft:silk_touch", 1)];
    assert_eq!(
        roll(
            STONE,
            "minecraft:stone",
            &[],
            Some(("minecraft:diamond_pickaxe", &silk)),
            1
        ),
        [("minecraft:stone".to_owned(), 1)]
    );
}

#[test]
fn block_state_conditions_select_the_dropping_half() {
    assert_eq!(
        roll(DOOR, "minecraft:oak_door", &[("half", "lower")], None, 1).len(),
        1
    );
    assert!(roll(DOOR, "minecraft:oak_door", &[("half", "upper")], None, 1).is_empty());
}

#[test]
fn counts_stay_in_range_and_fortune_adds() {
    for seed in 0..200 {
        let [(_, count)] = roll(ORE, "minecraft:redstone_ore", &[], None, seed)[..] else {
            panic!("one drop");
        };
        assert!((4..=5).contains(&count), "{count}");
        let fortune = [("minecraft:fortune", 3)];
        let [(_, count)] = roll(
            ORE,
            "minecraft:redstone_ore",
            &[],
            Some(("minecraft:iron_pickaxe", &fortune)),
            seed,
        )[..] else {
            panic!("one drop");
        };
        assert!((4..=8).contains(&count), "{count}");
    }
}

#[test]
fn unsupported_shapes_are_load_errors() {
    let weighted = r#"{"pools":[{"rolls":1,"entries":[{"type":"minecraft:item","name":"minecraft:a","weight":2}]}]}"#;
    assert!(LootTables::from_json("minecraft:a", weighted).is_err());
    let two = r#"{"pools":[{"rolls":1,"entries":[{"type":"minecraft:item","name":"minecraft:a"},{"type":"minecraft:item","name":"minecraft:b"}]}]}"#;
    assert!(LootTables::from_json("minecraft:a", two).is_err());
    let unknown = r#"{"pools":[{"rolls":1,"entries":[{"type":"minecraft:item","name":"minecraft:a","condition":{"type":"minecraft:weather_check"}}]}]}"#;
    assert!(LootTables::from_json("minecraft:a", unknown).is_err());
}

/// Every vanilla block table loads, and common blocks drop what they do in
/// vanilla when broken by hand.
#[test]
#[ignore = "needs RUSTMC_VANILLA_DATA, the directory holding data/minecraft"]
fn every_vanilla_block_table_loads() {
    let root = std::path::PathBuf::from(
        std::env::var_os("RUSTMC_VANILLA_DATA").expect("set RUSTMC_VANILLA_DATA"),
    );
    let tables = LootTables::load(&root.join("data")).expect("load");
    assert_eq!(tables.len(), 1_201);
    let state = BTreeMap::new();
    let hand = |block: &str| {
        let mut random = RandomSource::from_world_seed(7);
        let mut context = Context {
            block,
            state: &state,
            tool: None,
            random: &mut random,
            neighbour: &|_| None,
        };
        tables.block(block).expect(block).roll(&mut context)
    };
    assert_eq!(
        hand("minecraft:stone"),
        [("minecraft:cobblestone".to_owned(), 1)]
    );
    assert_eq!(
        hand("minecraft:grass_block"),
        [("minecraft:dirt".to_owned(), 1)]
    );
    assert!(hand("minecraft:glass").is_empty(), "glass needs Silk Touch");
    assert_eq!(
        hand("minecraft:oak_log"),
        [("minecraft:oak_log".to_owned(), 1)]
    );
}
