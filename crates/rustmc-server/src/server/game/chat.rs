//! Chat and the command entry points: player chat relayed to everyone,
//! commands from players and the console, and the system messages they send.
//!
//! Why: chat and commands are how players talk to each other and to the
//! server; both pass through the game loop so they see one consistent world.
//! Done: chat relayed as system chat with the sender's name; chat with
//! illegal characters kicks; commands from Chat Command and Signed Chat
//! Command; console lines run at the highest permission level; replies to
//! the sender (or the console log); each command logged.
//! Not done: signed chat (the join packet does not enforce secure chat, so
//! messages are relayed unsigned), chat type registry formatting.
//!
//! Sources: minecraft.wiki `Java Edition protocol/Packets` (Chat Message,
//! Chat Command, Signed Chat Command, System Chat Message, Entity Event),
//! `Chat` (the "Illegal characters in chat" kick for `§` and control
//! characters), `Entity statuses` (24 to 28 set a player's permission level 0
//! to 4).

use std::sync::Arc;

use super::Game;
use super::commands::{self, required_level};
use crate::java_packets::play::clientbound;
use crate::java_wire::read_varint;
use crate::net::framing::Packet;
use crate::protocol::chat::{
    ChatCommandPacket, ChatPacket, SignedChatCommandPacket, SystemChatMessagePacket, broadcast_chat,
};
use crate::server::connection::{ConnectionId, Outgoing};
use crate::server::log;
use crate::server::ops::MAX_LEVEL;

/// Why chat ends a connection.
pub const ILLEGAL_CHAT: &str = "Illegal characters in chat";
/// Why a malformed chat packet ends a connection.
pub const MALFORMED_CHAT: &str = "Invalid packet";
/// Entity status that sets a player's permission level 0; levels 1 to 4
/// follow it.
pub const OP_LEVEL_STATUS_BASE: u8 = 24;
/// The name `say` and logs use for the console.
pub const CONSOLE_NAME: &str = "Server";

/// Who ran a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sender {
    Console,
    Player(ConnectionId),
}

/// A System Chat Message body for plain `text`.
pub fn system_message(text: &str) -> Option<Packet> {
    packet_of(&SystemChatMessagePacket::chat_text(text).ok()?)
}

fn packet_of(message: &SystemChatMessagePacket) -> Option<Packet> {
    let encoded = message.encode_packet();
    let (_, id_len) = read_varint(&encoded).ok()??;
    Some(Packet {
        id: clientbound::SYSTEM_CHAT,
        body: encoded[id_len..].to_vec(),
    })
}

/// Whether vanilla would refuse this chat text: the section sign and
/// control characters.
fn is_illegal(text: &str) -> bool {
    text.chars().any(|c| c == '§' || c < ' ' || c == '\u{7f}')
}

impl Game {
    /// Chat Message: relayed to every player.
    pub(super) fn chat(&mut self, id: ConnectionId, body: &[u8]) -> Result<(), &'static str> {
        let message = ChatPacket::decode_payload(body).map_err(|_| MALFORMED_CHAT)?;
        if is_illegal(&message.message) {
            return Err(ILLEGAL_CHAT);
        }
        let Some(player) = self.players.get(&id) else {
            return Ok(());
        };
        let name = player.profile.name.clone();
        log::event("chat", &[("name", &name), ("message", &message.message)]);
        if let Some(packet) = broadcast_chat(&name, &message.message)
            .ok()
            .as_ref()
            .and_then(packet_of)
        {
            self.broadcast(packet);
        }
        Ok(())
    }

    /// Chat Command and Signed Chat Command.
    pub(super) fn chat_command(
        &mut self,
        id: ConnectionId,
        packet_id: u32,
        body: &[u8],
    ) -> Result<(), &'static str> {
        let command = if packet_id == crate::java_packets::play::serverbound::CHAT_COMMAND {
            ChatCommandPacket::decode_payload(body)
                .map_err(|_| MALFORMED_CHAT)?
                .command
        } else {
            SignedChatCommandPacket::decode_payload(body)
                .map_err(|_| MALFORMED_CHAT)?
                .command
        };
        if is_illegal(&command) {
            return Err(ILLEGAL_CHAT);
        }
        self.run_command(Sender::Player(id), &command);
        Ok(())
    }

    /// The permission level a sender has.
    pub(super) fn level_of(&self, sender: Sender) -> u8 {
        match sender {
            Sender::Console => MAX_LEVEL,
            Sender::Player(id) => self
                .players
                .get(&id)
                .map_or(0, |player| self.ops.level(&player.profile.uuid)),
        }
    }

    /// The name a sender goes by.
    pub(super) fn name_of(&self, sender: Sender) -> String {
        match sender {
            Sender::Console => CONSOLE_NAME.to_owned(),
            Sender::Player(id) => self
                .players
                .get(&id)
                .map_or_else(String::new, |player| player.profile.name.clone()),
        }
    }

    /// Checks permission, parses and runs one command line, replying to the
    /// sender.
    pub(super) fn run_command(&mut self, sender: Sender, line: &str) {
        let line = line.strip_prefix('/').unwrap_or(line).trim();
        let name = line.split_whitespace().next().unwrap_or_default();
        log::event(
            "command",
            &[("sender", &self.name_of(sender)), ("command", &line)],
        );
        let allowed = required_level(name).is_none_or(|level| self.level_of(sender) >= level);
        let reply = if !allowed {
            format!("You do not have permission to use /{name}")
        } else {
            match commands::parse(line) {
                Ok(command) => match self.execute(sender, command) {
                    Ok(text) | Err(text) => text,
                },
                Err(text) => text,
            }
        };
        self.reply(sender, &reply);
    }

    /// Sends `text` to the sender: system chat for a player, the log for the
    /// console. Several lines go out as several messages.
    pub(super) fn reply(&self, sender: Sender, text: &str) {
        match sender {
            Sender::Console => {
                for line in text.lines() {
                    log::event("console_reply", &[("text", &line)]);
                }
            }
            Sender::Player(id) => {
                if let Some(player) = self.players.get(&id) {
                    for line in text.lines() {
                        if let Some(packet) = system_message(line) {
                            player.handle.send(Outgoing::Packet(packet));
                        }
                    }
                }
            }
        }
    }

    /// Sends one packet to every player.
    pub(super) fn broadcast(&self, packet: Packet) {
        let shared = Arc::new(packet);
        for player in self.players.values() {
            player.handle.send(Outgoing::Shared(Arc::clone(&shared)));
        }
    }

    /// Tells a player's client its permission level, which it uses to
    /// decide what it offers (vanilla sends this on join and on op/deop).
    pub(super) fn send_permission_level(&self, id: ConnectionId) {
        let Some(player) = self.players.get(&id) else {
            return;
        };
        let level = self.ops.level(&player.profile.uuid).min(MAX_LEVEL);
        let mut body = player.entity_id.to_be_bytes().to_vec();
        body.push(OP_LEVEL_STATUS_BASE + level);
        player.handle.send(Outgoing::Packet(Packet {
            id: clientbound::ENTITY_EVENT,
            body,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_signs_and_control_characters_are_illegal() {
        assert!(!is_illegal("hello, world! ☃"));
        assert!(is_illegal("§cred"));
        assert!(is_illegal("line\nbreak"));
        assert!(is_illegal("del\u{7f}"));
    }

    #[test]
    fn system_messages_carry_the_system_chat_id_without_it_in_the_body() {
        let packet = system_message("hi").expect("packet");
        assert_eq!(packet.id, clientbound::SYSTEM_CHAT);
        let decoded = SystemChatMessagePacket::decode_payload(&packet.body).expect("decode");
        assert!(!decoded.is_overlay());
    }
}
