//! Parsing typed commands into the actions the game loop carries out.
//!
//! Why: parsing is pure and testable on its own; running a command needs
//! the world and players, which only the game loop owns.
//! Done: `help`, `list`, `say`, `gamemode`, `tp`/`teleport`, `give`,
//! `time`, `kick`, `op`, `deop`, `stop`, each with vanilla's permission level.
//! Not done: the Commands packet (client-side suggestions and syntax
//! colouring), selector arguments such as `@a[distance=..5]`, the other
//! vanilla commands.
//!
//! Sources: minecraft.wiki `Commands` and the page of each command (syntax
//! and required permission level), `Permission level`.

use crate::commands::{
    Coordinates3, GameMode as ModeName, MAX_GIVE_COUNT, TICKS_PER_DAY, TIME_DAY, TIME_MIDNIGHT,
    TIME_NIGHT, TIME_NOON, TargetSelector, tokenize_command,
};
use crate::server::join::GameMode;

/// Where a teleport goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Destination {
    Position(Coordinates3),
    Player(TargetSelector),
}

/// What `/time query` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeQuery {
    DayTime,
    GameTime,
    Day,
}

/// A change to the day clock or a question about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeAction {
    Set(u64),
    Add(u64),
    Query(TimeQuery),
}

/// A parsed command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Help,
    List,
    Say(String),
    GameMode {
        mode: GameMode,
        target: Option<TargetSelector>,
    },
    Teleport {
        targets: Option<TargetSelector>,
        destination: Destination,
    },
    Give {
        targets: TargetSelector,
        item: String,
        count: u32,
    },
    Time(TimeAction),
    Kick {
        target: TargetSelector,
        reason: Option<String>,
    },
    Op(String),
    Deop(String),
    Stop,
}

/// Every command name with its required permission level and usage, in the
/// order `help` lists them. Levels follow the vanilla command pages.
pub const COMMANDS: [(&str, u8, &str); 11] = [
    ("help", 0, "/help"),
    ("list", 0, "/list"),
    ("say", 2, "/say <message>"),
    ("gamemode", 2, "/gamemode <mode> [<target>]"),
    ("tp", 2, "/tp [<targets>] <location>|<destination>"),
    ("give", 2, "/give <targets> <item> [<count>]"),
    (
        "time",
        2,
        "/time set|add <time> | /time query daytime|gametime|day",
    ),
    ("kick", 3, "/kick <target> [<reason>]"),
    ("op", 3, "/op <player>"),
    ("deop", 3, "/deop <player>"),
    ("stop", 4, "/stop"),
];

/// The permission level `name` requires, after aliases.
pub fn required_level(name: &str) -> Option<u8> {
    let name = if name == "teleport" { "tp" } else { name };
    COMMANDS
        .iter()
        .find(|(command, _, _)| *command == name)
        .map(|(_, level, _)| *level)
}

/// Parses a command line, with or without its leading `/`.
pub fn parse(input: &str) -> Result<Command, String> {
    let input = input.strip_prefix('/').unwrap_or(input).trim();
    let (name, rest) = input.split_once(' ').unwrap_or((input, ""));
    let rest = rest.trim();
    let args = tokenize_command(rest).map_err(|error| error.to_string())?;
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let usage = || {
        let name = if name == "teleport" { "tp" } else { name };
        COMMANDS
            .iter()
            .find(|(command, _, _)| *command == name)
            .map_or_else(
                || format!("Unknown command: {name}"),
                |(_, _, usage)| format!("Usage: {usage}"),
            )
    };
    let selector = |text: &str| TargetSelector::parse(text).map_err(|error| error.to_string());
    Ok(match (name, args.as_slice()) {
        ("help", []) => Command::Help,
        ("list", []) => Command::List,
        ("stop", []) => Command::Stop,
        ("say", [_, ..]) => Command::Say(rest.to_owned()),
        ("gamemode", [mode, target @ ..]) if target.len() <= 1 => Command::GameMode {
            mode: game_mode(mode)?,
            target: target.first().map(|t| selector(t)).transpose()?,
        },
        ("tp" | "teleport", [destination]) => Command::Teleport {
            targets: None,
            destination: Destination::Player(selector(destination)?),
        },
        ("tp" | "teleport", [targets, destination]) => Command::Teleport {
            targets: Some(selector(targets)?),
            destination: Destination::Player(selector(destination)?),
        },
        ("tp" | "teleport", [x, y, z]) => Command::Teleport {
            targets: None,
            destination: Destination::Position(coordinates(x, y, z)?),
        },
        ("tp" | "teleport", [targets, x, y, z]) => Command::Teleport {
            targets: Some(selector(targets)?),
            destination: Destination::Position(coordinates(x, y, z)?),
        },
        ("give", [targets, item]) => give(selector(targets)?, item, None)?,
        ("give", [targets, item, count]) => give(selector(targets)?, item, Some(count))?,
        ("time", ["set", value]) => Command::Time(TimeAction::Set(time_value(value, true)?)),
        ("time", ["add", value]) => Command::Time(TimeAction::Add(time_value(value, false)?)),
        ("time", ["query", what]) => Command::Time(TimeAction::Query(match *what {
            "daytime" => TimeQuery::DayTime,
            "gametime" => TimeQuery::GameTime,
            "day" => TimeQuery::Day,
            _ => return Err(usage()),
        })),
        ("kick", [target, reason @ ..]) => Command::Kick {
            target: selector(target)?,
            reason: (!reason.is_empty()).then(|| reason.join(" ")),
        },
        ("op", [player]) => Command::Op(player_name(player)?),
        ("deop", [player]) => Command::Deop(player_name(player)?),
        _ => return Err(usage()),
    })
}

fn game_mode(text: &str) -> Result<GameMode, String> {
    Ok(
        match ModeName::parse(text).map_err(|error| error.to_string())? {
            ModeName::Survival => GameMode::Survival,
            ModeName::Creative => GameMode::Creative,
            ModeName::Adventure => GameMode::Adventure,
            ModeName::Spectator => GameMode::Spectator,
        },
    )
}

fn coordinates(x: &str, y: &str, z: &str) -> Result<Coordinates3, String> {
    let coordinates = Coordinates3::parse(x, y, z).map_err(|error| error.to_string())?;
    if coordinates.is_local() {
        return Err("Local coordinates (^) are not supported yet".to_owned());
    }
    Ok(coordinates)
}

fn give(targets: TargetSelector, item: &str, count: Option<&&str>) -> Result<Command, String> {
    let count = match count {
        Some(text) => text
            .parse::<u32>()
            .ok()
            .filter(|count| (1..=MAX_GIVE_COUNT).contains(count))
            .ok_or_else(|| format!("Count must be from 1 to {MAX_GIVE_COUNT}"))?,
        None => 1,
    };
    let item = if item.contains(':') {
        item.to_owned()
    } else {
        format!("minecraft:{item}")
    };
    Ok(Command::Give {
        targets,
        item,
        count,
    })
}

/// A `/time` value: ticks, optionally with a `d`, `s` or `t` unit, or for
/// `set` one of the named times.
fn time_value(text: &str, named: bool) -> Result<u64, String> {
    if named {
        match text {
            "day" => return Ok(TIME_DAY),
            "noon" => return Ok(TIME_NOON),
            "night" => return Ok(TIME_NIGHT),
            "midnight" => return Ok(TIME_MIDNIGHT),
            _ => {}
        }
    }
    const TICKS_PER_SECOND: u64 = 20;
    let (number, scale) = match text.as_bytes().last() {
        Some(b'd') => (&text[..text.len() - 1], TICKS_PER_DAY),
        Some(b's') => (&text[..text.len() - 1], TICKS_PER_SECOND),
        Some(b't') => (&text[..text.len() - 1], 1),
        _ => (text, 1),
    };
    number
        .parse::<u64>()
        .ok()
        .and_then(|value| value.checked_mul(scale))
        .ok_or_else(|| format!("Invalid time: {text}"))
}

fn player_name(text: &str) -> Result<String, String> {
    match TargetSelector::parse(text).map_err(|error| error.to_string())? {
        TargetSelector::Named(name) => Ok(name),
        _ => Err("A player name is required".to_owned()),
    }
}

#[cfg(test)]
mod tests;
