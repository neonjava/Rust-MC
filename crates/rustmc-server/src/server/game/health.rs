//! Health: fall and void damage, natural regeneration, death and respawn.
//!
//! Why: Survival needs danger and its consequence; the server owns health
//! so a client cannot ignore damage.
//! Done: health and food sent with Set Health; fall damage of one point per
//! block fallen beyond three in Survival and Adventure; void damage below
//! the world; regeneration of one point every 80 ticks while food is 18 or
//! more; death with the respawn screen, a death message to everyone and the
//! inventory dropped; respawn at the world spawn with full health.
//! Not done: hunger (food stays full), other damage sources (mobs, lava,
//! drowning, suffocation), armour, the hurt animation for onlookers, saving
//! health with the player, translated death messages (the English text is
//! sent).
//!
//! Sources: minecraft.wiki `Health`, `Damage` (fall damage: distance fallen
//! minus the 3-block safe fall distance), `Void` (4 damage every 10 ticks
//! below 64 blocks under the world), `Hunger` (natural regeneration),
//! `Java Edition protocol/Packets` (Set Health, Combat Death, Respawn,
//! Client Status).

use std::sync::Arc;

use super::chat::system_message;
use super::command_run::sync_changed_slots;
use super::{Game, SPAWN_XZ};
use crate::chunk_adapter::OVERWORLD_MIN_Y as MIN_Y;
use crate::java_packets::play::clientbound;
use crate::java_wire::{put_nbt_text, put_varint, read_varint};
use crate::net::framing::Packet;
use crate::protocol::entity_spawn::RemoveEntitiesPacket;
use crate::server::connection::{ConnectionId, Outgoing};
use crate::server::join::{GameMode, abilities, chunk_cache_center, respawn, set_time, teleport};
use crate::server::world::terrain::surface_y;
use crate::server::world::view::{View, chunk_of};

/// Full health, in half-hearts.
pub const MAX_HEALTH: f32 = 20.0;
/// Full food.
pub const MAX_FOOD: u32 = 20;
/// Saturation a fresh player has.
pub const START_SATURATION: f32 = 5.0;
/// Blocks a player can fall without damage.
pub const SAFE_FALL_DISTANCE: f64 = 3.0;
/// Food from which health regenerates.
pub const REGENERATION_FOOD: u32 = 18;
/// Ticks between regenerated points.
pub const REGENERATION_TICKS: u32 = 80;
/// How far below the lowest block the void starts hurting.
pub const VOID_DEPTH: f64 = 64.0;
/// Void damage and how often it repeats.
pub const VOID_DAMAGE: f32 = 4.0;
pub const VOID_INTERVAL_TICKS: u64 = 10;
/// Client Status action: respawn after death.
pub const ACTION_RESPAWN: u32 = 0;

/// What killed a player, for the death message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    Fall,
    Void,
}

impl Cause {
    /// Vanilla's English death message.
    fn message(self, name: &str) -> String {
        match self {
            Self::Fall => format!("{name} fell from a high place"),
            Self::Void => format!("{name} fell out of the world"),
        }
    }
}

/// A player's health state.
#[derive(Debug, Clone, PartialEq)]
pub struct Health {
    pub health: f32,
    pub food: u32,
    pub saturation: f32,
    /// Highest point of the current fall, while airborne.
    fall_peak: Option<f64>,
    regeneration: u32,
    pub dead: bool,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            health: MAX_HEALTH,
            food: MAX_FOOD,
            saturation: START_SATURATION,
            fall_peak: None,
            regeneration: 0,
            dead: false,
        }
    }
}

impl Health {
    /// Tracks a move; returns the fall damage of a landing, if any.
    pub fn moved(&mut self, y: f64, on_ground: bool, takes_damage: bool) -> Option<f32> {
        if !takes_damage {
            self.fall_peak = None;
            return None;
        }
        if !on_ground {
            self.fall_peak = Some(self.fall_peak.map_or(y, |peak| peak.max(y)));
            return None;
        }
        let distance = self.fall_peak.take()? - y;
        let damage = (distance - SAFE_FALL_DISTANCE).ceil();
        (damage > 0.0).then_some(damage as f32)
    }

    /// Forgets the current fall, as a teleport does.
    pub fn reset_fall(&mut self) {
        self.fall_peak = None;
    }

    fn packet(&self) -> Packet {
        let mut body = self.health.max(0.0).to_be_bytes().to_vec();
        put_varint(self.food, &mut body);
        body.extend(self.saturation.to_be_bytes());
        Packet {
            id: clientbound::SET_HEALTH,
            body,
        }
    }
}

/// Whether a game mode takes damage.
pub fn takes_damage(mode: GameMode) -> bool {
    matches!(mode, GameMode::Survival | GameMode::Adventure)
}

impl Game {
    /// Sends a player their health.
    pub(super) fn send_health(&self, id: ConnectionId) {
        if let Some(player) = self.players.get(&id) {
            player.handle.send(Outgoing::Packet(player.health.packet()));
        }
    }

    /// Applies fall damage after a move.
    pub(super) fn after_move(&mut self, id: ConnectionId, y: f64, on_ground: bool) {
        let Some(player) = self.players.get_mut(&id) else {
            return;
        };
        let damage = player
            .health
            .moved(y, on_ground, takes_damage(player.game_mode));
        if let Some(damage) = damage {
            self.damage(id, damage, Cause::Fall);
        }
    }

    /// Takes health from a player; at zero they die.
    pub(super) fn damage(&mut self, id: ConnectionId, amount: f32, cause: Cause) {
        let Some(player) = self.players.get_mut(&id) else {
            return;
        };
        if player.health.dead {
            return;
        }
        player.health.health -= amount;
        player.handle.send(Outgoing::Packet(player.health.packet()));
        if player.health.health <= 0.0 {
            self.die(id, cause);
        }
    }

    fn die(&mut self, id: ConnectionId, cause: Cause) {
        let Some(player) = self.players.get_mut(&id) else {
            return;
        };
        player.health.dead = true;
        player.health.reset_fall();
        let message = cause.message(&player.profile.name);
        let mut kill = Vec::new();
        put_varint(player.entity_id as u32, &mut kill);
        if put_nbt_text(&message, &mut kill).is_ok() {
            player.handle.send(Outgoing::Packet(Packet {
                id: clientbound::PLAYER_COMBAT_KILL,
                body: kill,
            }));
        }
        // Everything carried drops where the player died.
        let before = player.inventory.slots().to_vec();
        let carried: Vec<_> = before.iter().flatten().cloned().collect();
        player.inventory = crate::world::inventory::PlayerInventory::new();
        sync_changed_slots(
            &player.inventory,
            &player.handle,
            &before,
            &self.ids,
            &self.dynamic_ids,
        );
        let at = player.position;
        let entity_id = player.entity_id;
        for stack in carried {
            self.spawn_item(
                stack,
                (
                    at[0].floor() as i32,
                    at[1].floor() as i32,
                    at[2].floor() as i32,
                ),
            );
        }
        let mut body = Vec::new();
        RemoveEntitiesPacket::single(entity_id).encode_payload(&mut body);
        let removed = Arc::new(Packet {
            id: clientbound::REMOVE_ENTITIES,
            body,
        });
        for viewer in self.players.values_mut() {
            if viewer.seen.remove(&entity_id) {
                viewer.handle.send(Outgoing::Shared(Arc::clone(&removed)));
            }
        }
        if let Some(packet) = system_message(&message) {
            self.broadcast(packet);
        }
        crate::server::log::event("player_died", &[("message", &message)]);
    }

    /// Client Status from a player.
    pub(super) fn client_command(
        &mut self,
        id: ConnectionId,
        body: &[u8],
    ) -> Result<(), &'static str> {
        let (action, n) = read_varint(body)
            .ok()
            .flatten()
            .ok_or(super::reason::BAD_PACKET)?;
        if n != body.len() {
            return Err(super::reason::BAD_PACKET);
        }
        if action == ACTION_RESPAWN {
            self.respawn_player(id);
        }
        Ok(())
    }

    /// Brings a dead player back at the world spawn.
    fn respawn_player(&mut self, id: ConnectionId) {
        let spawn = self.spawn_point();
        let tick = self.tick;
        let day_time = self.day_time;
        let Some(player) = self.players.get_mut(&id) else {
            return;
        };
        if !player.health.dead {
            return;
        }
        player.health = super::health::Health::default();
        let Ok(packet) = respawn(player.game_mode, &self.manifest) else {
            return;
        };
        player.next_teleport_id = player.next_teleport_id.wrapping_add(1);
        let teleport_id = player.next_teleport_id;
        player.awaiting_teleport = Some(teleport_id);
        player.position = spawn;
        player.moved = true;
        // The client forgets its world on respawn: chunks and entities are
        // sent again.
        let center = chunk_of(spawn[0], spawn[2]);
        player.view = View::new(center, player.view.distance());
        player.seen.clear();
        let mut packets = vec![packet, abilities(player.game_mode), player.health.packet()];
        if let Ok(time) = set_time(tick, day_time, &self.manifest) {
            packets.push(time);
        }
        packets.push(chunk_cache_center(center.0, center.1));
        packets.push(teleport(teleport_id, spawn, player.yaw, player.pitch));
        for packet in packets {
            player.handle.send(Outgoing::Packet(packet));
        }
    }

    /// Where players spawn: on the terrain at the world spawn column.
    pub(super) fn spawn_point(&self) -> [f64; 3] {
        [
            SPAWN_XZ[0],
            f64::from(surface_y(
                self.source.generator,
                SPAWN_XZ[0].floor() as i64,
                SPAWN_XZ[1].floor() as i64,
            )),
            SPAWN_XZ[1],
        ]
    }

    /// Per-tick health: regeneration and the void.
    pub(super) fn tick_health(&mut self) {
        let void = f64::from(MIN_Y) - VOID_DEPTH;
        let mut hurt = Vec::new();
        for (id, player) in &mut self.players {
            if player.health.dead {
                continue;
            }
            if takes_damage(player.game_mode)
                && player.position[1] < void
                && self.tick.is_multiple_of(VOID_INTERVAL_TICKS)
            {
                hurt.push(*id);
            }
            if player.health.health < MAX_HEALTH && player.health.food >= REGENERATION_FOOD {
                player.health.regeneration += 1;
                if player.health.regeneration >= REGENERATION_TICKS {
                    player.health.regeneration = 0;
                    player.health.health = (player.health.health + 1.0).min(MAX_HEALTH);
                    player.handle.send(Outgoing::Packet(player.health.packet()));
                }
            }
        }
        for id in hurt {
            self.damage(id, VOID_DAMAGE, Cause::Void);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_beyond_three_blocks_hurt_one_point_per_block() {
        let mut health = Health::default();
        assert_eq!(health.moved(80.0, false, true), None);
        assert_eq!(health.moved(78.0, false, true), None);
        assert_eq!(
            health.moved(70.0, true, true),
            Some(7.0),
            "10 blocks: 7 damage"
        );
        assert_eq!(health.moved(70.0, false, true), None);
        assert_eq!(health.moved(67.0, true, true), None, "3 blocks are safe");
        assert_eq!(health.moved(80.0, false, false), None);
        assert_eq!(
            health.moved(60.0, true, false),
            None,
            "no damage in Creative"
        );
    }

    #[test]
    fn set_health_carries_health_food_and_saturation() {
        let packet = Health::default().packet();
        assert_eq!(packet.id, clientbound::SET_HEALTH);
        assert_eq!(packet.body[..4], MAX_HEALTH.to_be_bytes());
        assert_eq!(packet.body[4], 20);
        assert_eq!(packet.body[5..], START_SATURATION.to_be_bytes());
    }
}
