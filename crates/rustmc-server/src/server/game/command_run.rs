//! Carrying out parsed commands against the world and the players.
//!
//! Why: every command's effect goes through the same game-loop paths as
//! play (teleports, inventory, the clock), so clients stay in step.
//! Done: the commands listed in `commands`, with replies modelled on
//! vanilla's wording; selectors `@a`, `@s`, `@p`, `@r` and player names.
//! Not done: items that do not fit are not dropped on the ground (no item
//! entities yet), and `op` of an offline player in online mode (no profile
//! lookup).
//!
//! Sources: minecraft.wiki `Commands/<name>` for each command;
//! `Java Edition protocol/Packets` (Game Event 3 "Change game mode",
//! Set Container Slot, Synchronize Player Position, Set Time).

use super::Game;
use super::chat::Sender;
use super::commands::{COMMANDS, Command, Destination, TimeAction, TimeQuery, required_level};
use super::movement::in_world;
use crate::commands::TargetSelector;
use crate::item::ItemStack;
use crate::java_packets::play::clientbound;
use crate::java_wire::put_varint;
use crate::net::crypto::random_bytes;
use crate::net::framing::Packet;
use crate::net::identity::offline_uuid;
use crate::protocol::slot::encode_slot;
use crate::server::connection::{ConnectionId, Outgoing};
use crate::server::join::{GameMode, abilities, set_time, teleport};
use crate::server::world::view::chunk_of;
use crate::world::inventory::{
    HOTBAR_SLOT_COUNT, PlayerInventory, TOTAL_SLOTS, WINDOW_HOTBAR_SLOTS,
};

/// Game Event 3: the player's game mode changed.
pub const GAME_EVENT_CHANGE_GAME_MODE: u8 = 3;
/// Player inventory window id.
pub const PLAYER_WINDOW: u32 = 0;
/// Reason given to a kicked player when the operator names none (vanilla's
/// default).
pub const DEFAULT_KICK_REASON: &str = "Kicked by an operator";
/// Ticks in a Minecraft day.
const TICKS_PER_DAY: u64 = crate::commands::TICKS_PER_DAY;

type Outcome = Result<String, String>;

impl Game {
    /// Runs one parsed command; the text is the reply either way.
    pub(super) fn execute(&mut self, sender: Sender, command: Command) -> Outcome {
        match command {
            Command::Help => Ok(self.help(sender)),
            Command::List => Ok(self.list()),
            Command::Say(text) => {
                let line = format!("[{}] {text}", self.name_of(sender));
                if let Some(packet) = super::chat::system_message(&line) {
                    self.broadcast(packet);
                }
                Ok(String::new())
            }
            Command::GameMode { mode, target } => {
                let targets = match target {
                    Some(selector) => self.resolve_targets(sender, &selector)?,
                    None => vec![self.sender_player(sender)?],
                };
                let mut names = Vec::new();
                for id in targets {
                    self.set_game_mode(id, mode);
                    names.push(self.name_of(Sender::Player(id)));
                }
                Ok(format!(
                    "Set game mode to {} for {}",
                    mode_name(mode),
                    names.join(", ")
                ))
            }
            Command::Teleport {
                targets,
                destination,
            } => self.teleport_command(sender, targets, &destination),
            Command::Give {
                targets,
                item,
                count,
            } => self.give(sender, &targets, &item, count),
            Command::Time(action) => Ok(self.time(action)),
            Command::Kick { target, reason } => {
                let targets = self.resolve_targets(sender, &target)?;
                let reason = reason.unwrap_or_else(|| DEFAULT_KICK_REASON.to_owned());
                let mut names = Vec::new();
                for id in targets {
                    names.push(self.name_of(Sender::Player(id)));
                    self.remove_player(id, &reason);
                }
                Ok(format!("Kicked {}: {reason}", names.join(", ")))
            }
            Command::Op(name) => self.op(&name, true),
            Command::Deop(name) => self.op(&name, false),
            Command::Stop => {
                self.stop_requested = true;
                Ok("Stopping the server".to_owned())
            }
        }
    }

    fn help(&self, sender: Sender) -> String {
        let level = self.level_of(sender);
        COMMANDS
            .iter()
            .filter(|(name, _, _)| required_level(name).is_some_and(|need| level >= need))
            .map(|(_, _, usage)| *usage)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn list(&self) -> String {
        let mut names: Vec<&str> = self
            .players
            .values()
            .map(|player| player.profile.name.as_str())
            .collect();
        names.sort_unstable();
        format!(
            "There are {} of a max of {} players online: {}",
            names.len(),
            self.config.max_players,
            names.join(", ")
        )
    }

    /// The sender as a player, for commands without an explicit target.
    fn sender_player(&self, sender: Sender) -> Result<ConnectionId, String> {
        match sender {
            Sender::Player(id) if self.players.contains_key(&id) => Ok(id),
            _ => Err("A player is required to run this command here".to_owned()),
        }
    }

    /// The players a selector names.
    fn resolve_targets(
        &self,
        sender: Sender,
        selector: &TargetSelector,
    ) -> Result<Vec<ConnectionId>, String> {
        let mut by_name: Vec<(&str, ConnectionId)> = self
            .players
            .iter()
            .map(|(id, player)| (player.profile.name.as_str(), *id))
            .collect();
        by_name.sort_unstable();
        let found = match selector {
            TargetSelector::AllPlayers => by_name.iter().map(|(_, id)| *id).collect(),
            TargetSelector::Self_ => vec![self.sender_player(sender)?],
            TargetSelector::Named(name) => by_name
                .iter()
                .filter(|(other, _)| other.eq_ignore_ascii_case(name))
                .map(|(_, id)| *id)
                .collect(),
            TargetSelector::NearestPlayer => {
                let origin = self
                    .players
                    .get(&self.sender_player(sender)?)
                    .map(|p| p.position);
                let nearest = origin.and_then(|origin| {
                    self.players
                        .iter()
                        .map(|(id, player)| (distance_sq(origin, player.position), *id))
                        .min_by(|a, b| a.0.total_cmp(&b.0))
                        .map(|(_, id)| id)
                });
                nearest.into_iter().collect()
            }
            TargetSelector::RandomPlayer => {
                let mut bytes = [0u8; 8];
                random_bytes(&mut bytes).map_err(|_| "No random source".to_owned())?;
                let index = (u64::from_be_bytes(bytes) % by_name.len().max(1) as u64) as usize;
                by_name.get(index).map(|(_, id)| *id).into_iter().collect()
            }
        };
        if found.is_empty() {
            Err("No player was found".to_owned())
        } else {
            Ok(found)
        }
    }

    fn set_game_mode(&mut self, id: ConnectionId, mode: GameMode) {
        let Some(player) = self.players.get_mut(&id) else {
            return;
        };
        player.game_mode = mode;
        let mut event = vec![GAME_EVENT_CHANGE_GAME_MODE];
        event.extend(f32::from(mode as u8).to_be_bytes());
        player.handle.send(Outgoing::Packet(Packet {
            id: clientbound::GAME_EVENT,
            body: event,
        }));
        player.handle.send(Outgoing::Packet(abilities(mode)));
        self.announce_game_mode(id);
    }

    fn teleport_command(
        &mut self,
        sender: Sender,
        targets: Option<TargetSelector>,
        destination: &Destination,
    ) -> Outcome {
        let targets = match &targets {
            Some(selector) => self.resolve_targets(sender, selector)?,
            None => vec![self.sender_player(sender)?],
        };
        let position = match destination {
            Destination::Player(selector) => {
                let to = self.resolve_targets(sender, selector)?;
                let [only] = to.as_slice() else {
                    return Err(
                        "Only one player is allowed, but the selector allows more".to_owned()
                    );
                };
                self.players
                    .get(only)
                    .map(|p| p.position)
                    .ok_or("No player was found")?
            }
            Destination::Position(coordinates) => {
                let origin = match sender {
                    Sender::Player(id) => self.players.get(&id).map(|p| p.position),
                    Sender::Console => None,
                };
                if !coordinates.is_absolute() && origin.is_none() {
                    return Err("The console can only use absolute coordinates".to_owned());
                }
                let [x, y, z] = origin.unwrap_or_default();
                let (x, y, z) = coordinates.resolve((x, y, z));
                [x, y, z]
            }
        };
        if !in_world(position) {
            return Err("Cannot teleport outside the world".to_owned());
        }
        let mut names = Vec::new();
        for id in targets {
            self.teleport_player(id, position);
            names.push(self.name_of(Sender::Player(id)));
        }
        Ok(format!(
            "Teleported {} to {:.2}, {:.2}, {:.2}",
            names.join(", "),
            position[0],
            position[1],
            position[2]
        ))
    }

    /// Moves a player and makes their client follow.
    pub(super) fn teleport_player(&mut self, id: ConnectionId, position: [f64; 3]) {
        let Some(player) = self.players.get_mut(&id) else {
            return;
        };
        player.next_teleport_id = player.next_teleport_id.wrapping_add(1);
        let teleport_id = player.next_teleport_id;
        player.awaiting_teleport = Some(teleport_id);
        player.position = position;
        player.moved = true;
        player.handle.send(Outgoing::Packet(teleport(
            teleport_id,
            position,
            player.yaw,
            player.pitch,
        )));
        for out in player.view.recenter(chunk_of(position[0], position[2])) {
            player.handle.send(out);
        }
    }

    fn give(
        &mut self,
        sender: Sender,
        targets: &TargetSelector,
        item: &str,
        count: u32,
    ) -> Outcome {
        let max_stack = self
            .items
            .max_stack(item)
            .ok_or_else(|| format!("Unknown item '{item}'"))?;
        let targets = self.resolve_targets(sender, targets)?;
        let mut lines = Vec::new();
        for id in targets {
            let Some(player) = self.players.get_mut(&id) else {
                continue;
            };
            let before: Vec<Option<ItemStack>> = player.inventory.slots().to_vec();
            let mut remaining = count;
            while remaining > 0 {
                let size = remaining.min(u32::from(max_stack)) as u16;
                let Ok(stack) = ItemStack::try_new(item, size, max_stack) else {
                    break;
                };
                match player.inventory.grant_item(stack) {
                    Ok(left) => {
                        remaining -= u32::from(size - left);
                        if left > 0 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let given = count - remaining;
            sync_changed_slots(
                &player.inventory,
                &player.handle,
                &before,
                &self.ids,
                &self.dynamic_ids,
            );
            let name = player.profile.name.clone();
            lines.push(if remaining == 0 {
                format!("Gave {given} [{item}] to {name}")
            } else {
                format!("Gave {given} [{item}] to {name}; {remaining} did not fit")
            });
        }
        Ok(lines.join("\n"))
    }

    fn time(&mut self, action: TimeAction) -> String {
        let reply = match action {
            TimeAction::Set(value) => {
                self.day_time = value;
                format!("Set the time to {value}")
            }
            TimeAction::Add(value) => {
                self.day_time = self.day_time.saturating_add(value);
                format!("Set the time to {}", self.day_time % TICKS_PER_DAY)
            }
            TimeAction::Query(TimeQuery::DayTime) => {
                return format!("The time is {}", self.day_time % TICKS_PER_DAY);
            }
            TimeAction::Query(TimeQuery::GameTime) => return format!("The time is {}", self.tick),
            TimeAction::Query(TimeQuery::Day) => {
                return format!("The time is {}", self.day_time / TICKS_PER_DAY);
            }
        };
        if let Ok(packet) = set_time(self.tick, self.day_time, &self.manifest) {
            self.broadcast(packet);
        }
        reply
    }

    fn op(&mut self, name: &str, grant: bool) -> Outcome {
        let online = self
            .players
            .iter()
            .find(|(_, player)| player.profile.name.eq_ignore_ascii_case(name))
            .map(|(id, player)| (*id, player.profile.uuid, player.profile.name.clone()));
        let (id, uuid, name) = match online {
            Some((id, uuid, name)) => (Some(id), uuid, name),
            None if !self.config.online_mode => (None, offline_uuid(name), name.to_owned()),
            None => return Err("That player is not online".to_owned()),
        };
        let changed = if grant {
            self.ops.grant(uuid, &name)
        } else {
            self.ops.revoke(&uuid)
        }
        .map_err(|error| format!("Could not save the operator list: {error}"))?;
        if let Some(id) = id {
            self.send_permission_level(id);
        }
        Ok(match (changed, grant) {
            (true, true) => format!("Made {name} a server operator"),
            (true, false) => format!("Made {name} no longer a server operator"),
            (false, true) => "Nothing changed. The player already is an operator".to_owned(),
            (false, false) => "Nothing changed. The player is not an operator".to_owned(),
        })
    }
}

fn distance_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter().zip(b).map(|(a, b)| (a - b) * (a - b)).sum()
}

fn mode_name(mode: GameMode) -> &'static str {
    match mode {
        GameMode::Survival => "Survival Mode",
        GameMode::Creative => "Creative Mode",
        GameMode::Adventure => "Adventure Mode",
        GameMode::Spectator => "Spectator Mode",
    }
}

/// Sends Set Container Slot for every inventory slot that changed.
fn sync_changed_slots(
    inventory: &PlayerInventory,
    handle: &crate::server::connection::ConnectionHandle,
    before: &[Option<ItemStack>],
    ids: &crate::protocol::ids::ProtocolIds,
    dynamic: &crate::protocol::ids::DynamicRegistryIds,
) {
    let state_id = inventory.revision() as u32;
    for index in TOTAL_SLOTS {
        let now = inventory.get_slot(index).cloned();
        if before.get(usize::from(index)).cloned().flatten() == now {
            continue;
        }
        // Hotbar slots 0..=8 are window slots 36..=44; the main inventory
        // keeps its numbers.
        let window_slot = if usize::from(index) < HOTBAR_SLOT_COUNT {
            *WINDOW_HOTBAR_SLOTS.start() + i16::from(index)
        } else {
            i16::from(index)
        };
        let mut body = Vec::new();
        put_varint(PLAYER_WINDOW, &mut body);
        put_varint(state_id, &mut body);
        body.extend(window_slot.to_be_bytes());
        if encode_slot(&now, ids, dynamic, &mut body).is_ok() {
            handle.send(Outgoing::Packet(Packet {
                id: clientbound::CONTAINER_SET_SLOT,
                body,
            }));
        }
    }
}
