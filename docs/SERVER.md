# Running the playable server

The playable server is the shared multiplayer world: login (offline or
online mode), terrain streaming, saving, block play, other players, chat and
commands. It is configured by a `[server]` table instead of `[listener]`.
What it does and does not do yet is tracked in [STATUS.md](STATUS.md) and
[PARITY.md](PARITY.md).

## Inputs prepared from the official server

The server reads ids and item facts from files generated locally from the
official Java 26.3 server, never from the repository. Follow the preparation
steps in [DEVELOPMENT.md](DEVELOPMENT.md) (the preview section) to produce:

- the registry manifest (`preview-registries.toml`);
- the chunk registry table (`chunk-registry-26.3.json`, from
  `prepare_chunk_registry`);
- the data generator's reports directory, which holds `registries.json` and
  `minecraft/components/item/`.

## Example

```toml
schema_version = 1
log_level = "info"

[server]
bind_address = "0.0.0.0"
port = 25565
online_mode = true
max_players = 20
registry_manifest = "/path/to/preview-registries.toml"
registry_table = "/path/to/chunk-registry-26.3.json"
registries_report = "/path/to/reports/generated/reports/registries.json"
world_dir = "world"
```

Run it with `cargo run --release -p rustmc-server --locked -- --run server.toml`.
Lines typed on standard input run as console commands at the highest
permission level; `stop` saves and shuts down, as does Ctrl-C.

## Keys

Unknown keys are rejected. Names follow vanilla's `server.properties` with
underscores (`online-mode` becomes `online_mode`).

| Key | Default | Meaning |
| --- | --- | --- |
| `registry_manifest` | required | Registry identifier manifest prepared from the official server. |
| `registry_table` | required | Block-state and biome id table prepared from the official reports. |
| `registries_report` | required | The official `reports/registries.json` (item and entity ids). |
| `item_components` | `minecraft/components/item` beside `registries_report` | The official per-item component reports; stack sizes come from here. |
| `bind_address` | `0.0.0.0` | Address to listen on. |
| `port` | `25565` | TCP port; `0` picks a free one. |
| `online_mode` | `true` | Verify players with the session server; `false` gives offline UUIDs. |
| `max_players` | `20` | Players allowed at once, 1 to 1,000. |
| `motd` | `A Minecraft Server` | Server list message, at most 256 bytes. |
| `network_compression_threshold` | `256` | Packet size from which packets are compressed; a negative value disables compression. |
| `view_distance` | `10` | Chunks around each player, 2 to 32; a client asking for less gets less. |
| `game_mode` | `survival` | Game mode of new players: `survival`, `creative`, `adventure` or `spectator`. |
| `seed` | `0` | Seed of a new world; a saved world keeps its own. |
| `world_dir` | `world` | Directory of the saved world, in the 26.3 Anvil layout. |
| `ops_file` | `ops.json` | Operator list in vanilla's `ops.json` format. |
| `autosave_interval_seconds` | `300` | Seconds between saves, 10 to 3,600; edits in between are kept by the journal. |
| `generation_workers` | cores minus one | Chunk generation threads, 1 to 64. |
| `session_server` | Mojang's | Session server base URL for online mode. |
| `max_packet_bytes` | protocol maximum | Largest serverbound packet after decompression, from 1,024. |
| `read_bytes_per_second` | `1048576` | Read rate allowed per connection, 4,096 to 16,777,216. |
| `login_timeout_ms` | `30000` | Time from first byte to joining, 1,000 to 120,000. |
| `outbound_queue_packets` | `4096` | Packets queued for one slow client before it is disconnected, 64 to 65,536. |

## Operators

The first operator is made from the console: type `op <name>`. Operators get
level 4, vanilla's default; levels can be edited in `ops.json`, which uses
vanilla's format, so a list from a vanilla server carries over. The commands
and their levels are in [COMMANDS.md](COMMANDS.md).
