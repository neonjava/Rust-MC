//! Unit tests for command parsing.

use super::*;
use crate::commands::Coordinate;

#[test]
fn simple_commands_parse_with_or_without_a_slash() {
    assert_eq!(parse("/help"), Ok(Command::Help));
    assert_eq!(parse("list"), Ok(Command::List));
    assert_eq!(parse("stop"), Ok(Command::Stop));
    assert_eq!(
        parse("say hello  world"),
        Ok(Command::Say("hello  world".to_owned()))
    );
    assert!(parse("say").is_err());
    assert_eq!(
        parse("frobnicate").unwrap_err(),
        "Unknown command: frobnicate"
    );
    assert!(parse("list extra").unwrap_err().starts_with("Usage: /list"));
}

#[test]
fn gamemode_accepts_names_and_ids_and_an_optional_target() {
    assert_eq!(
        parse("gamemode creative"),
        Ok(Command::GameMode {
            mode: GameMode::Creative,
            target: None
        })
    );
    assert_eq!(
        parse("gamemode 3 Alex"),
        Ok(Command::GameMode {
            mode: GameMode::Spectator,
            target: Some(TargetSelector::Named("Alex".to_owned()))
        })
    );
    assert!(parse("gamemode god").is_err());
}

#[test]
fn teleport_takes_positions_players_and_targets() {
    assert_eq!(
        parse("tp 1 ~2 -3.5"),
        Ok(Command::Teleport {
            targets: None,
            destination: Destination::Position(Coordinates3 {
                x: Coordinate::Absolute(1.0),
                y: Coordinate::Relative(2.0),
                z: Coordinate::Absolute(-3.5),
            }),
        })
    );
    assert_eq!(
        parse("teleport @a Steve"),
        Ok(Command::Teleport {
            targets: Some(TargetSelector::AllPlayers),
            destination: Destination::Player(TargetSelector::Named("Steve".to_owned())),
        })
    );
    assert!(parse("tp ^ ^ ^1").is_err(), "local coordinates are refused");
}

#[test]
fn give_namespaces_items_and_bounds_counts() {
    assert_eq!(
        parse("give @s stone 32"),
        Ok(Command::Give {
            targets: TargetSelector::Self_,
            item: "minecraft:stone".to_owned(),
            count: 32,
        })
    );
    assert!(parse("give @s stone 0").is_err());
    assert!(parse(&format!("give @s stone {}", MAX_GIVE_COUNT + 1)).is_err());
}

#[test]
fn time_accepts_named_times_and_units() {
    assert_eq!(
        parse("time set noon"),
        Ok(Command::Time(TimeAction::Set(TIME_NOON)))
    );
    assert_eq!(
        parse("time add 2d"),
        Ok(Command::Time(TimeAction::Add(2 * TICKS_PER_DAY)))
    );
    assert_eq!(parse("time add 3s"), Ok(Command::Time(TimeAction::Add(60))));
    assert_eq!(
        parse("time query gametime"),
        Ok(Command::Time(TimeAction::Query(TimeQuery::GameTime)))
    );
    assert!(parse("time add noon").is_err(), "named times only for set");
    assert!(parse("time set -5").is_err());
}

#[test]
fn kick_op_and_deop_name_players() {
    assert_eq!(
        parse("kick Bob being rude"),
        Ok(Command::Kick {
            target: TargetSelector::Named("Bob".to_owned()),
            reason: Some("being rude".to_owned()),
        })
    );
    assert_eq!(parse("op Alex"), Ok(Command::Op("Alex".to_owned())));
    assert!(parse("op @a").is_err());
    assert_eq!(parse("deop Alex"), Ok(Command::Deop("Alex".to_owned())));
}

#[test]
fn levels_follow_the_vanilla_command_pages() {
    assert_eq!(required_level("help"), Some(0));
    assert_eq!(required_level("teleport"), Some(2));
    assert_eq!(required_level("kick"), Some(3));
    assert_eq!(required_level("stop"), Some(4));
    assert_eq!(required_level("nope"), None);
}
