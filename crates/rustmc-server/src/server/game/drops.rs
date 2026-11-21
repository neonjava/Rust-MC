//! Dropped items: blocks broken in Survival drop their loot as item
//! entities that fall, can be picked up and despawn.
//!
//! Why: survival play depends on collecting what you mine.
//! Done: loot rolled from the block's vanilla loot table with the held tool;
//! item entities spawned for every viewer who has their chunk, falling under
//! gravity until they rest on a block; pickup by Survival, Adventure and
//! Creative players after a short delay into the first slots with room,
//! with the Collect Item animation; despawn after five minutes.
//! Not done: merging nearby stacks, items thrown by players, horizontal
//! motion and collision with block sides, hoppers.
//!
//! Sources: minecraft.wiki `Item (entity)` (gravity 0.04 blocks per tick
//! squared, drag 0.98, pickup delay 10 ticks for block drops, despawn after
//! 6,000 ticks, the pickup box is the player's hitbox grown by 1 block
//! horizontally and 0.5 vertically), `Entity metadata` (Item Entity index 8
//! is the stack), `Java Edition protocol/Packets` (Spawn Entity, Set Entity
//! Metadata, Pickup Item, Remove Entities, Teleport Entity).

use std::collections::BTreeMap;
use std::sync::Arc;

use super::Game;
use super::command_run::sync_changed_slots;
use crate::chunk_adapter::StateKind;
use crate::item::ItemStack;
use crate::java_packets::play::clientbound;
use crate::java_wire::put_varint;
use crate::loot::Context;
use crate::net::crypto::random_bytes;
use crate::net::framing::Packet;
use crate::protocol::entity_spawn::{
    EntityMetadataEntry, EntitySpawnFields, MetadataValue, RemoveEntitiesPacket,
    SetEntityDataPacket, SlotData, SpawnEntityPacket,
};
use crate::server::connection::{ConnectionId, Outgoing};
use crate::server::join::GameMode;
use crate::server::world::view::chunk_of;

/// The registry name of the item entity type.
pub const ITEM_ENTITY_TYPE: &str = "minecraft:item";
/// Downward acceleration of an item, blocks per tick squared.
pub const GRAVITY: f64 = 0.04;
/// Fraction of vertical speed an item keeps each tick.
pub const DRAG: f64 = 0.98;
/// Upward speed a block drop starts with, blocks per tick (RustMC choice;
/// vanilla's is random around this).
pub const POP_SPEED: f64 = 0.1;
/// Ticks before a block drop can be picked up.
pub const PICKUP_DELAY_TICKS: u32 = 10;
/// Ticks before an item despawns.
pub const DESPAWN_TICKS: u64 = 6_000;
/// How far the pickup box reaches past the player's hitbox.
pub const PICKUP_REACH_HORIZONTAL: f64 = 1.0;
pub const PICKUP_REACH_VERTICAL: f64 = 0.5;
/// Half the width (and the height) of an item entity's box.
pub const ITEM_HALF_SIZE: f64 = 0.125;
/// Item Entity metadata index holding the stack.
pub const METADATA_ITEM: u8 = 8;

/// One item entity.
pub struct Dropped {
    pub entity_id: i32,
    uuid: [u8; 16],
    pub stack: ItemStack,
    pub position: [f64; 3],
    speed_y: f64,
    resting: bool,
    age: u64,
    pickup_delay: u32,
    moved: bool,
}

/// Splits `minecraft:name[key=value,…]` into the name and its properties.
fn parse_state(key: &str) -> (String, BTreeMap<String, String>) {
    let Some((name, rest)) = key.split_once('[') else {
        return (key.to_owned(), BTreeMap::new());
    };
    let properties = rest
        .trim_end_matches(']')
        .split(',')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    (name.to_owned(), properties)
}

impl Game {
    /// Rolls the loot of a block a player broke and drops it.
    pub(super) fn drop_block_loot(
        &mut self,
        breaker: ConnectionId,
        pos: (i32, i32, i32),
        state: u32,
    ) {
        let Some(loot) = &self.loot else {
            return;
        };
        let Some(key) = self.source.tables.state_name(state) else {
            return;
        };
        let (block, properties) = parse_state(key);
        let Some(table) = loot.block(&block) else {
            return;
        };
        let tool = self
            .players
            .get(&breaker)
            .and_then(|player| player.inventory.held_item())
            .map(|stack| {
                (
                    stack.item().to_owned(),
                    stack.components().enchantments().clone(),
                )
            });
        let tables = Arc::clone(&self.source.tables);
        let store = &self.store;
        let neighbour = |(dx, dy, dz): (i32, i32, i32)| {
            let state = store.block(pos.0 + dx, pos.1 + dy, pos.2 + dz)?;
            tables.state_name(state).map(parse_state)
        };
        let mut context = Context {
            block: &block,
            state: &properties,
            tool: tool.as_ref().map(|(item, levels)| (item.as_str(), levels)),
            random: &mut self.random,
            neighbour: &neighbour,
        };
        let drops = table.roll(&mut context);
        for (item, count) in drops {
            let Some(max) = self.items.max_stack(&item) else {
                continue;
            };
            let mut left = count;
            while left > 0 {
                let size = left.min(u32::from(max)) as u16;
                left -= u32::from(size);
                if let Ok(stack) = ItemStack::try_new(&item, size, max) {
                    self.spawn_item(stack, pos);
                }
            }
        }
    }

    pub(super) fn spawn_item(&mut self, stack: ItemStack, (x, y, z): (i32, i32, i32)) {
        let entity_id = self.next_entity_id;
        self.next_entity_id = self.next_entity_id.wrapping_add(1).max(1);
        let mut uuid = [0u8; 16];
        if random_bytes(&mut uuid).is_err() {
            return;
        }
        self.drops.insert(
            entity_id,
            Dropped {
                entity_id,
                uuid,
                stack,
                position: [
                    f64::from(x) + 0.5,
                    f64::from(y) + 0.5 - ITEM_HALF_SIZE,
                    f64::from(z) + 0.5,
                ],
                speed_y: POP_SPEED,
                resting: false,
                age: 0,
                pickup_delay: PICKUP_DELAY_TICKS,
                moved: true,
            },
        );
    }

    /// One tick of every item: falling, pickup, despawn and what viewers see.
    pub(super) fn tick_drops(&mut self) {
        let tables = Arc::clone(&self.source.tables);
        let mut gone = Vec::new();
        for item in self.drops.values_mut() {
            item.age += 1;
            item.pickup_delay = item.pickup_delay.saturating_sub(1);
            if item.age >= DESPAWN_TICKS {
                gone.push((item.entity_id, None));
                continue;
            }
            if !item.resting {
                item.speed_y = (item.speed_y - GRAVITY) * DRAG;
                let next = item.position[1] + item.speed_y;
                let below = self.store.block(
                    item.position[0].floor() as i32,
                    next.floor() as i32,
                    item.position[2].floor() as i32,
                );
                // Air and fluids let an item fall; an unloaded column holds
                // it where it is.
                let falls = below.is_some_and(|state| {
                    matches!(tables.kind(state), Some(StateKind::Air | StateKind::Fluid))
                });
                if falls || item.speed_y > 0.0 {
                    item.position[1] = next;
                } else {
                    item.position[1] = next.floor() + 1.0;
                    item.speed_y = 0.0;
                    item.resting = true;
                }
                item.moved = true;
            }
        }
        for (entity_id, item) in &self.drops {
            if item.pickup_delay > 0 {
                continue;
            }
            let collector = self.players.iter().find(|(_, player)| {
                player.game_mode != GameMode::Spectator
                    && in_pickup_box(player.position, item.position)
            });
            if let Some((id, _)) = collector {
                gone.push((*entity_id, Some(*id)));
            }
        }
        for (entity_id, collector) in gone {
            match collector {
                Some(id) => self.pick_up(entity_id, id),
                None => self.remove_drop(entity_id),
            }
        }
        self.track_drops();
    }

    /// Moves as much of an item as fits into a player's inventory.
    fn pick_up(&mut self, entity_id: i32, id: ConnectionId) {
        let (Some(item), Some(player)) =
            (self.drops.get_mut(&entity_id), self.players.get_mut(&id))
        else {
            return;
        };
        let before = player.inventory.slots().to_vec();
        let count = item.stack.count();
        let Ok(left) = player.inventory.grant_item(item.stack.clone()) else {
            return;
        };
        sync_changed_slots(
            &player.inventory,
            &player.handle,
            &before,
            &self.ids,
            &self.dynamic_ids,
        );
        let taken = count - left;
        let collector_entity = player.entity_id;
        let mut collect = Vec::new();
        put_varint(entity_id as u32, &mut collect);
        put_varint(collector_entity as u32, &mut collect);
        put_varint(u32::from(taken), &mut collect);
        let collect = Arc::new(Packet {
            id: clientbound::TAKE_ITEM_ENTITY,
            body: collect,
        });
        for viewer in self.players.values() {
            if viewer.seen.contains(&entity_id) || viewer.entity_id == collector_entity {
                viewer.handle.send(Outgoing::Shared(Arc::clone(&collect)));
            }
        }
        if left == 0 {
            self.remove_drop(entity_id);
        } else if let Some(item) = self.drops.get_mut(&entity_id)
            && let Ok(rest) = ItemStack::try_new(item.stack.item(), left, item.stack.max_stack())
        {
            item.stack = rest;
        }
    }

    fn remove_drop(&mut self, entity_id: i32) {
        if self.drops.remove(&entity_id).is_none() {
            return;
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
    }

    /// Spawns, moves and removes item entities on each viewer's client.
    fn track_drops(&mut self) {
        let Some(entity_type) = self.item_entity_type.and_then(|t| i32::try_from(t).ok()) else {
            return;
        };
        for item in self.drops.values_mut() {
            let chunk = chunk_of(item.position[0], item.position[2]);
            let spawn = spawn_packets(item, entity_type, &self.ids);
            let movement = item.moved.then(|| Arc::new(position_packet(item)));
            item.moved = false;
            for viewer in self.players.values_mut() {
                let visible = viewer.view.has_sent(chunk);
                let known = viewer.seen.contains(&item.entity_id);
                if visible && !known {
                    for packet in spawn.iter().flatten() {
                        viewer.handle.send(Outgoing::Packet(packet.clone()));
                    }
                    viewer.seen.insert(item.entity_id);
                } else if !visible && known {
                    viewer.seen.remove(&item.entity_id);
                    let mut body = Vec::new();
                    RemoveEntitiesPacket::single(item.entity_id).encode_payload(&mut body);
                    viewer.handle.send(Outgoing::Packet(Packet {
                        id: clientbound::REMOVE_ENTITIES,
                        body,
                    }));
                } else if visible && let Some(movement) = &movement {
                    viewer.handle.send(Outgoing::Shared(Arc::clone(movement)));
                }
            }
        }
    }
}

/// Whether an item's box touches a player's pickup box.
fn in_pickup_box(player: [f64; 3], item: [f64; 3]) -> bool {
    let half_width = super::blocks::PLAYER_WIDTH / 2.0 + PICKUP_REACH_HORIZONTAL + ITEM_HALF_SIZE;
    let bottom = player[1] - PICKUP_REACH_VERTICAL - ITEM_HALF_SIZE;
    let top = player[1] + super::blocks::PLAYER_HEIGHT + PICKUP_REACH_VERTICAL + ITEM_HALF_SIZE;
    (item[0] - player[0]).abs() <= half_width
        && (item[2] - player[2]).abs() <= half_width
        && (bottom..=top).contains(&item[1])
}

/// Spawn Entity and the metadata carrying the stack.
fn spawn_packets(
    item: &Dropped,
    entity_type: i32,
    ids: &crate::protocol::ids::ProtocolIds,
) -> Option<[Packet; 2]> {
    let spawn = SpawnEntityPacket::new(EntitySpawnFields {
        entity_id: item.entity_id,
        uuid: item.uuid,
        entity_type,
        position: item.position,
        pitch: 0,
        yaw: 0,
        head_yaw: 0,
        data: 0,
    })
    .ok()?;
    let mut spawn_body = Vec::new();
    spawn.encode_payload(&mut spawn_body);
    let metadata = SetEntityDataPacket {
        entity_id: item.entity_id,
        metadata: vec![EntityMetadataEntry::new(
            METADATA_ITEM,
            MetadataValue::Slot(SlotData {
                item_id: i32::try_from(ids.item_id(item.stack.item())?).ok()?,
                count: i32::from(item.stack.count()),
            }),
        )],
    };
    let mut metadata_body = Vec::new();
    metadata.encode_payload(&mut metadata_body).ok()?;
    Some([
        Packet {
            id: clientbound::ADD_ENTITY,
            body: spawn_body,
        },
        Packet {
            id: clientbound::SET_ENTITY_DATA,
            body: metadata_body,
        },
    ])
}

/// Teleport Entity (`entity_position_sync`) to the item's position.
fn position_packet(item: &Dropped) -> Packet {
    let mut body = Vec::new();
    put_varint(item.entity_id as u32, &mut body);
    put_varint(0, &mut body); // linear path
    for axis in item.position {
        body.extend(axis.to_be_bytes());
    }
    body.extend(0f32.to_be_bytes());
    body.extend(0f32.to_be_bytes());
    body.push(u8::from(item.resting));
    Packet {
        id: clientbound::ENTITY_POSITION_SYNC,
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_keys_split_into_name_and_properties() {
        let (name, properties) = parse_state("minecraft:oak_door[facing=east,half=lower]");
        assert_eq!(name, "minecraft:oak_door");
        assert_eq!(properties.get("half").map(String::as_str), Some("lower"));
        assert_eq!(parse_state("minecraft:stone").1.len(), 0);
    }

    #[test]
    fn the_pickup_box_reaches_a_block_around_the_player() {
        let feet = [0.5, 64.0, 0.5];
        assert!(in_pickup_box(feet, [1.8, 64.1, 0.5]));
        assert!(!in_pickup_box(feet, [2.5, 64.1, 0.5]));
        assert!(in_pickup_box(feet, [0.5, 63.5, 0.5]));
        assert!(!in_pickup_box(feet, [0.5, 62.9, 0.5]));
        assert!(in_pickup_box(feet, [0.5, 66.3, 0.5]));
    }
}
