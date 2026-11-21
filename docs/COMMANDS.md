# Commands

What RustMC's command dispatcher accepts today, and where it runs.

**Where commands run.** On `main`, commands run only inside the opt-in Java
preview session. There they are parsed, checked against the sender's
permission level and answered with a chat message; they do not change the
world, because the preview has no shared, mutable world. The playable server
(Phase 1) does not accept commands yet. Since Phase 1 E it does; see "The
playable server" below.

**Permissions.** Each command has a required permission level. In the preview
every player has level 2, so every command below is available; `help` needs
level 0.

| Command | Level | Arguments | What it checks |
| --- | --- | --- | --- |
| `help` | 0 | none | Lists the commands the sender may use |
| `tp`, `teleport` | 2 | a destination (coordinates, `~` relative or a target selector), optionally a target first | Coordinates are finite and inside the world border; the console may only use absolute coordinates |
| `gamemode` | 2 | `survival`, `creative`, `adventure` or `spectator`, optionally a target | The mode name and the target |
| `seed` | 2 | none | Replies with the world seed |
| `time` | 2 | `set` with ticks or `day` (1000), `noon` (6000), `night` (13000), `midnight` (18000); `add` with ticks; `query` with `daytime`, `gametime` or `day` | Values are in range |
| `kill` | 2 | optionally a target | The target |
| `give` | 2 | a target, an item id and optionally a count | The item id is a valid namespaced identifier and the count is at most 6,400 |

The source of truth is `crates/rustmc-server/src/commands/`.

## The playable server

The playable server ([SERVER.md](SERVER.md)) runs its own commands against the
shared world, typed by players (with or without `/`) or at the server
console. Players have their level from `ops.json` (0 if they are not an
operator); the console has level 4. Levels follow the vanilla command pages
on minecraft.wiki.

| Command | Level | Arguments | Effect |
| --- | --- | --- | --- |
| `help` | 0 | none | Lists the commands the sender may use |
| `list` | 0 | none | Players online and the limit |
| `say` | 2 | a message | Sends `[sender] message` to everyone |
| `gamemode` | 2 | a mode (name, abbreviation or 0 to 3), optionally a target | Changes the game mode; abilities and the tab list follow |
| `tp`, `teleport` | 2 | a location or a player, optionally targets first | Teleports; `~` is relative to the sender, `^` is refused, the console needs absolute coordinates |
| `give` | 2 | targets, an item and optionally a count (1 to 6,400) | Adds stacks of the item's vanilla stack size; what does not fit is reported, not dropped |
| `time` | 2 | `set` (ticks, `d`/`s`/`t` units, or `day`, `noon`, `night`, `midnight`), `add`, `query daytime`/`gametime`/`day` | Changes or reports the day clock; every client is updated |
| `kick` | 3 | a target and optionally a reason | Disconnects the player |
| `op`, `deop` | 3 | a player name | Grants or removes operator level 4 and saves `ops.json` |
| `stop` | 4 | none | Saves and shuts the server down |

Selectors: `@a`, `@s`, `@p`, `@r` and player names; selector arguments such
as `@a[distance=..5]` are not supported yet. The client receives no command
tree yet, so it offers no suggestions and colours commands as unknown, but
sends them all the same.

