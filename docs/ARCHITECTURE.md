# Architecture

AgentForge is a deterministic simulation kernel with a machine-first protocol.
This document explains who owns state, how it changes, what "deterministic" means here,
how permissions work, how schemas stay in sync, and what was deliberately deferred.

## Crates and dependencies

```
agentforge-core   pure simulation, no I/O          (serde, schemars, sha2, thiserror)
      ^
agentforge-host   protocol + sessions + storage     (+ serde_json, tempfile, tracing)
      ^
agentforge-cli    `agentforge` binary               (+ clap, tracing-subscriber)
      |
      +---------> agentforge-demo   scripted clients (serde_json, thiserror only)
```

- **core** knows nothing about transports, sessions, files or model providers. Every public
  function is a pure function of its inputs.
- **host** turns protocol requests into core calls. It owns session state, enforces roles and
  revisions, and reads and writes files. It also carries the two transports: bounded JSON
  Lines and MCP over stdio.
- **cli** wires a host to stdin/stdout and provides the demo, replay verification, schema
  export and benchmarks.
- **demo** clients depend on **no** engine crate. They spawn `agentforge serve` processes and
  speak JSON lines, exactly like an external agent, so they cannot reach internals.

## State ownership and the single mutation path

Each `agentforge serve` process holds one `session::Host`:

| State | Kind | Notes |
|---|---|---|
| open project (scenes, rules, entities, ID allocators) | simulation | changed only by authoring operations |
| active episode (frozen scene, tick, player, inventory, outcome, events, step log) | simulation | changed only by `episode.act` / `start` / `reset` |
| snapshots | simulation copies | captured and restored by authors |
| revision, project path, dirty flag, episode/snapshot ids | session metadata | never part of state hashes |

Every request goes `Host::handle_line` → envelope checks → role check → typed argument
parse → one handler. Handlers validate and compute first, then commit with infallible
assignments at the end. Consequences:

- a rejected request (bad JSON, unknown field, stale revision, failed batch, illegal move)
  changes nothing: not the revision, the scene, the episode or the allocator;
- scene batches are applied to a copy (`Scene::apply_edits`) and swapped in only if every
  edit succeeded;
- `Episode::apply` computes the next state and events before committing;
- an engine panic is caught per request and reported as `internal_error`; because commits
  happen last, the request has no effect and the session keeps serving.

## Sessions, roles and permissions

The role is fixed by whoever launches the process, never by request content:

```
author process  --project.save-->  project file  --(launcher)-->  player process
(--role author)                                                 (--role player --project --scene)
```

- **Author**: every operation, including play.
- **Player**: `discover`, `session.status`, `episode.*` and `replay.export`, only for the
  scene the launcher bound. Players never see authoring labels: observations omit them, and
  their replay exports strip them. A player cannot edit scenes or rules, spawn entities, set state,
  capture or restore snapshots, load or save projects, verify replays from files, choose
  output paths, or start another scene. A `role` field in a request is rejected as an
  unknown envelope field.
- Player processes see the author's work only through saved project files. That is the
  play-mode boundary: a playtest never shares mutable state with the editor.

This is **local capability separation between processes**, not authentication. Anyone who
can launch the binary can launch an author session. Remote access would need a transport
with real authentication, which is out of scope.

## MCP transport

`agentforge mcp` (and `agentforge physics mcp`) serve the same session through the Model
Context Protocol, so an MCP client can keep one session alive without holding a pipe. The
adapter is a transport, not a second implementation:

- `tools/list` is generated from `OpId` (grid) or the physics operation catalog, filtered to
  the launcher-fixed role. Descriptions and argument schemas come from the same catalog as
  `discover`, so they cannot drift.
- `tools/call` rebuilds a protocol envelope and enters `handle_line`, the single dispatch
  path. Revision checks, atomic batches, stale-episode guards and error bodies are unchanged.
- Tool names replace dots with underscores (`entity.create` becomes `entity_create`) to
  satisfy MCP client name rules; the mapping is one-way.
- Successful results carry `{revision, result}`; engine failures are `isError` results with
  `{revision, error}`. JSON-RPC errors are reserved for transport problems: malformed
  messages, unknown methods, unknown tools and non-object arguments.
- Only the legacy 2025-06-18 revision is implemented. A modern client that probes
  `server/discover` gets an unknown-method error and falls back to `initialize`; a
  modern-only client cannot connect.

The server is synchronous with one request in flight, like the JSON Lines loop. It adds no
simulation state and no authorization: the role still comes from the launcher.

## Revisions, concurrency and retries

- The session revision starts at 0 (or 1 when a project is preloaded). Every accepted
  authoring mutation increments it, and every response reports it.
- Mutations require `expected_revision == revision`; otherwise they fail with
  `stale_revision` and change nothing. Accepted changes are serialized by the single-threaded
  request loop.
- `snapshot.restore` also increments the revision. It never rewinds to the captured revision,
  so a write based on any pre-restore revision (including the snapshot's own) stays stale.
- `request_id` is correlation only; there is no deduplication. Retrying a mutation whose first
  delivery succeeded yields `stale_revision`. `episode.act` takes `expected_episode_id` and
  `expected_tick`: a duplicate yields `stale_tick`, and an action meant for an episode that
  has since been reset, restarted or restored yields `stale_episode`. Episode ids are never
  reused; a restored episode continues under a new id, because tick numbers repeat across
  timelines and would otherwise let a delayed action land on the wrong one.

## Episodes (editor/play separation)

`episode.start` copies the scene. Later edits never affect a running episode, and
`episode.reset` replays the same frozen scene. Turn order is specified in
`agentforge_core::episode` and published by `discover` (`game.tick_resolution_order`):

1. reject if ended, or if the first cell of a move is outside the grid, a wall, a locked
   door, or a one-way tile facing against the move (nothing changes);
2. tick += 1;
3. apply: move resolves the whole displacement (floor and ice crossings, at most one portal
   hop, collecting an uncollected key on the final cell), wait, or forfeit (fails
   immediately);
4. fail immediately if the final cell is a hazard;
5. win if on an exit and the win condition holds;
6. otherwise fail if `tick == max_ticks`.

There are no autonomous entities, timers or randomness, so no simulation work is pending
between ticks. Snapshots therefore capture entities, rules, the ID allocator, tick, player,
inventory, outcome, events and the step log, which is the complete state needed to continue.
They also record the project's save path as metadata, so a restored project saves back to
its own file and never over another project's.

## Determinism

What makes outcomes a pure function of (initial scene, accepted actions):

- integer coordinates and counters only; no floating point anywhere in simulation state;
- `BTreeMap`/`Vec` with explicit orders (entities by id, neighbours by
  `Direction::ALL = [north, east, south, west]`); no hash-map iteration;
- no wall clock, randomness, threads or I/O in the core. The only clock is in `bench`, which
  measures and decides nothing;
- stable entity ids from a monotonic allocator that never reuses ids; allocator state is
  saved, snapshotted and hashed.

**Scope of the guarantee.** Identical hashes for identical inputs are tested within a process
and across processes (save/load, fresh-process replay verification). The golden-hash tests
pin the byte encoding and were cross-checked against an independent Python implementation.
Verification so far ran on macOS (arm64); identity across other platforms or future engine
versions is not claimed. Replays declare `format_version` and `state_hash_version`; mismatches are
rejected explicitly instead of being reported as divergences.

## Canonical hashing

`hash.rs` is the reference. Hashes are SHA-256 over this encoding (version 1), written as
`sha256:<64 lowercase hex>`. Integers are fixed-width little-endian; `str(s)` is the UTF-8
byte length as `u32` followed by the bytes.

**Scene digest**

```
str("agentforge.scene.v1")
u32 width, u32 height
u32 max_ticks, u8 win_condition        (1 reach_exit, 2 collect_all_keys_then_exit)
u32 next_entity_id
u32 entity_count
per entity, ascending id:
  u32 id, u8 kind                      (1 player, 2 wall, 3 key, 4 door, 5 exit,
                                        6 ice, 7 one_way, 8 hazard, 9 portal)
  u8 attribute                         (0 none; key/door 1 + color index;
                                        one_way 1 + direction index; portal 1 + pair id)
  i32 x, i32 y
```

Labels are excluded (presentation metadata).

**State hash** (recorded after every tick)

```
str("agentforge.state.v1")
32 raw bytes of the scene digest
u32 tick
i32 player_x, i32 player_y
u8 inventory bitmask                   (bit i = color index i)
u8 outcome                             (0 in progress, 1 won, 2 failed: max_ticks,
                                       3 failed: forfeited, 4 failed: hazard)
u32 outcome_tick                       (0 while in progress)
```

Each key color appears at most once per scene, so the inventory also determines which keys
were collected. Session metadata (revisions, ids of episodes and snapshots, paths) is never
hashed.

**Recording fingerprint** (identifies a replay; used for content-addressed file names)

```
str("agentforge.recording.v1")
32 raw bytes scene digest, 32 raw bytes initial hash
u32 step_count
per step: u32 tick, u8 action (1-4 move north/east/south/west, 5 wait, 6 forfeit), 32 hash bytes
32 raw bytes final hash
```

Two different routes to the same end state have the same final hash, but always different
fingerprints.

## Replays

A replay contains the frozen initial scene, its digest, the tick-0 hash, every accepted
action with its tick and resulting hash, the outcome and the final hash, plus format, hash and
engine versions. `replay verify` (CLI) and `replay.verify` (protocol) rebuild the scene,
re-execute each action and compare every hash. They report the first divergence precisely
(scene digest, initial hash, rejected action, tick, state hash, outcome, final hash).
Verification never consults a player or model again. With `--project`, it also checks that
the replay starts from that project's scene.

## Analysis

`analysis::reachability` answers two questions separately:

- **geometric**: walls block; doors and terrain are open; keys, win condition and
  `max_ticks` are ignored. It is a walls-only diagnostic, not an oracle: a portal can make
  gameplay reachable while this is not, and a wrong-way one-way can do the reverse.
- **gameplay**: breadth-first search over (cell, keys held) with the real rules, including
  slides, one-way entry, portal hops and fatal hazards. Terrain is static, so it adds no
  state dimension: with at most 64×64 cells and 8 key colors, the search visits at most
  about a million states and is run to exhaustion, so "unreachable" is a proof.
  Non-consumable colored keys are what keep it exhaustive and bounded; consumable keys would
  make the state space grow exponentially with the number of doors.

A move resolves through `movement::plan`, the same function episodes use, so play and
analysis cannot disagree. Entering ice starts a slide that continues until a wall, locked
door, wrong-way one-way, edge or object stops it; objects stop slides, terrain does not.
Portals are one hop and end a displacement; hazards are entered and fail the episode. The
search never expands a hazard or a winning exit state.

Stepping onto an exit while the win condition holds ends the episode, so the search never
continues past such a state. A route to a cell beyond an exit is therefore correctly
unreachable, and a key behind a winning exit is correctly unobtainable.

When gameplay fails, the report names the **blockers**: walls, wrong-way one-way tiles and
hazards next to cells the player can still move from or slide through; doors whose key is
missing or unreachable; required keys that cannot be reached (`collect_all_keys_then_exit`);
and exits that end the episode before a cell target. **Repair candidates** are every entity
whose single removal could change the search result:

- walls, doors, keys, exits, wrong-way one-way tiles and hazards adjacent to active cells
  (the search never tries to enter any other ones);
- ice tiles that an active move enters (removing one changes where a slide stops);
- every key, when the win condition requires all keys (removing a key can only drop a
  requirement or close doors);
- exits reached in a winning state, for cell targets.

Portals are deliberately excluded: a valid portal repair changes both members of a pair, so
it is already a two-entity change outside the single-removal guarantee. Each candidate (up to
64 per query) is verified by re-running the search without it, and only verified repairs are
listed. Because the candidate set covers everything that could matter, "none of the
candidates works" after an exhaustive evaluation proves that at least two changes are
needed. The report never guesses a culprit.

## Schemas and documentation without drift

- Argument and result types derive `JsonSchema` (schemars 1.x) and reject unknown fields.
- `OpId` is the single source of truth: the dispatcher matches on it exhaustively;
  `discover`, `docs/protocol/protocol.schema.json` and `docs/protocol/operations.md` are
  generated from it.
- Tests check that every published example parses into its argument type and that the
  committed docs equal the generator output (`agentforge schema --check`).

## Persistence

- Project files (`agentforge.project` v1) hold named scenes; replay files hold
  `agentforge.replay` v1. Both carry `format` and `format_version`, which are checked
  **before** strict parsing so files from newer versions get a precise "unsupported version"
  error. Content is then fully validated (scene invariants, ids within the allocated range)
  before the session changes.
- Player replay exports are named `<scene>-<outcome>-<ticks>t-<fingerprint>.replay.json`
  inside the launcher's `--replay-dir`. The same recording always gets the same name, and
  different recordings never share one, even when several player processes share the
  directory. Player exports strip authoring labels; labels are not hashed, so verification
  is unaffected.
- Writes are atomic where the OS allows: temporary file in the destination directory →
  `fsync` → rename → best-effort directory `fsync`. Existing file permissions are kept, and
  new files get 0644. A regression test covers this: the first version left files at the
  temporary file's 0600.
- Files are formatted for people and language models: small objects stay on one line
  (one entity per line), larger ones expand, and keys keep struct order.

## Decision log

- **No bevy_ecs.** Bevy entity ids are generational and remapped on scene load, and query
  order depends on archetype layout. Both work against stable ids, canonical hashing and
  exact snapshots, and at about 100 entities an ECS brings no benefit. Plain ordered data
  structures make determinism and persistence straightforward. An ECS can sit behind the same
  protocol later if real-time worlds need one.
- **JSON Lines over stdin/stdout.** No async runtime, HTTP server or database is needed for
  this milestone. Process boundaries give real capability separation for free.
- **Process-per-role.** It avoids trusting role strings, and playtests cannot mutate the
  editor's world.
- **Colored, non-consumed keys; one key per color.** They keep solvability exact and the
  canonical state small.
- **No randomness.** Nothing in the grid engine needs it. A future seeded RNG would be part of
  the snapshot and hash encoding (with a version bump).
- **Rejected moves do not consume ticks.** `legal_actions` is then exactly the set of
  accepted actions, and an illegal action is a no-op with an explanation.

## Physics extension

The separate `agentforge-physics` crate and `agentforge physics` launcher now provide
Rapier rigid-body scenes with the same envelope, role, revision and retry conventions.
The bounded JSON Lines server accepts either session implementation through its `Session`
trait. Physics snapshots rebuild complete solver state from the initial scene and accepted
inputs; grid snapshots still copy their integer simulation state directly. The formats and
hash domains are distinct, preserving existing grid replay hashes. See
[PHYSICS.md](PHYSICS.md) for commands, canonical encoding and tested determinism scope.

## Deferred work

These are extension points only; none is implemented.

- **Modern MCP.** Support the 2026-07-28 per-request-metadata revision (including
  `server/discover`) alongside the legacy `initialize` handshake.
- **Remote transports.** A network transport with real authentication. Today the boundary is
  local process separation.
- **Richer rules.** A bounded declarative rule format for new mechanics (enemies, switches,
  timers), informed by VGDL, PuzzleScript and Griddly GDY rather than arbitrary code.
- **Session-level replays** that include authoring edits; persisted snapshots.
- **Hidden information**: per-role observation filters, with tests that prove nothing leaks.
- **Viewer**: a live read-only visualizer fed by the same observations.
