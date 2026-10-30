# MCP server

The engine can expose one long-lived session as [Model Context Protocol](https://modelcontextprotocol.io)
tools over stdio. This removes the friction described in the main README: a coding agent whose
shell runs one command at a time cannot keep `agentforge serve` alive between requests, but an
MCP client keeps the server process open. Revisions, open projects, snapshots and the running
episode therefore persist across tool calls.

The adapter adds no simulation code. Tools are the engine's own operations, and their
descriptions and argument schemas come from the same catalog that generates `discover` and the
published protocol references.

```sh
# Grid engine, trusted authoring session.
cargo run --release -p agentforge-cli -- mcp --role author

# Grid engine, restricted player bound to one saved scene.
cargo run --release -p agentforge-cli -- mcp --role player \
  --project out/demo/project.agentforge.json --scene blocked_dungeon

# Physics engine.
cargo run --release -p agentforge-cli -- physics mcp --role author
```

Launch arguments mean the same as for `serve`, including `--replay-dir` for player sessions.
The role is fixed by the launcher and cannot be changed by a tool call. This is local process
separation, not authentication.

## Client configuration

Claude Code:

```sh
claude mcp add agentforge -- /absolute/path/target/release/agentforge mcp --role author
```

Generic MCP client JSON:

```json
{
  "mcpServers": {
    "agentforge": {
      "command": "/absolute/path/target/release/agentforge",
      "args": ["mcp", "--role", "author"]
    },
    "agentforge-physics": {
      "command": "/absolute/path/target/release/agentforge",
      "args": ["physics", "mcp", "--role", "author"]
    }
  }
}
```

Only JSON-RPC messages go to stdout; logs go to stderr (`AGENTFORGE_LOG=info|debug`).

## Tools

Tool names replace dots with underscores because many MCP clients restrict names to
`[A-Za-z0-9_-]`: `entity.create` is `entity_create`, `episode.act` is `episode_act`. The
dotted operation name is unchanged in `discover`, the JSON Lines protocol and the generated
references. Tools are filtered to the operations the launcher-fixed role may call, so a player
server does not advertise authoring tools.

`tools/list` publishes each tool's description and standalone JSON Schema of its arguments.
Result schemas are intentionally not repeated in the tool list to keep the model context
small. Grid sessions can request one operation's result schema with
`discover` and `{"operation": "<name>", "include_schemas": true}`; physics `discover` returns
every schema at once. The committed `docs/protocol/protocol.schema.json` and
`docs/physics.schema.json` carry the same schemas.

### Result shape

Every successful call returns both pretty JSON text and structured content:

```json
{"revision": 2, "result": {"scene": "hall", "entities": []}}
```

`revision` is the session revision after the call, so an agent can use it directly as
`expected_revision` for the next mutation. Engine failures are MCP tool errors (`isError:
true`) whose text carries the same actionable body as a JSON Lines response:

```json
{"revision": 2, "error": {"code": "stale_revision", "message": "...", "details": {...}, "hint": "..."}}
```

JSON-RPC errors are reserved for transport problems: malformed JSON, unknown methods, unknown
tools, non-object arguments, and requests sent before `initialize`.

### Mapping

| MCP tool | Engine operation |
|---|---|
| `discover` | `discover` |
| `session_status` | `session.status` |
| `project_create`, `project_load`, `project_save` | `project.*` |
| `scene_create`, `scene_get`, `scene_validate`, `scene_apply_batch`, `scene_update_rules`, `scene_render` | `scene.*` (grid) |
| `scene_set`, `scene_svg`, `scene_save`, `scene_load` | `scene.*` (physics) |
| `entity_create`, `entity_get`, `entity_update`, `entity_remove` | `entity.*` |
| `analysis_reachability` | `analysis.reachability` |
| `episode_start`, `episode_reset`, `episode_observe`, `episode_act`, `episode_events`, `episode_assert` | `episode.*` |
| `snapshot_capture`, `snapshot_restore` | `snapshot.*` |
| `replay_export`, `replay_verify`, `replay_verify_file` | `replay.*` |

## Protocol revision

This server implements the legacy (initialize-handshake) MCP revision **2025-06-18**. A
client that probes with `server/discover` receives an unknown-method error and, as the
specification requires, falls back to `initialize`. The modern per-request-metadata revision
(2026-07-28) is not implemented; a client that supports only that revision cannot use this
server yet.

Supported behavior includes `initialize`, the `notifications/initialized` notification,
`ping`, `tools/list`, `tools/call`, structured tool output, and JSON-RPC error codes. JSON-RPC
batching is rejected because the 2025-06-18 revision removed it.

## Limits

- One request in flight; the server is single-threaded and synchronous, like the engine.
- MCP messages are limited to 4 MiB. The engine still enforces its own 1 MiB request limit,
  so very large `scene.apply_batch` arguments must be split.
- Tool output is not truncated. A large replay export or observation is returned in full, just
  as a JSON Lines response would be.
- The session lives until stdin closes. A client restart starts a fresh session with revision
  0 and no project, exactly like a new `serve` process.

## What this does not change

Determinism, replay verification and capability separation are the engine's existing
guarantees. The MCP adapter only changes the transport: the same accepted actions produce the
same hashes, and a player server still cannot author. There is no network transport, no remote
authentication and no model call inside the simulation loop.
