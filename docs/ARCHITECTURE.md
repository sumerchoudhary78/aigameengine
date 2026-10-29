# Architecture

AgentForge milestone 1 is a deterministic simulation kernel with a machine-first protocol.
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
  revisions, and reads and writes files.
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
  scene the launcher bound. A player cannot edit scenes or rules, spawn entities, set state,
  capture or restore snapshots, load or save projects, verify replays from files, choose
  output paths, or start another scene. A `role` field in a request is rejected as an
  unknown envelope field.
- Player processes see the author's work only through saved project files. That is the
  play-mode boundary: a playtest never shares mutable state with the editor.

This is **local capability separation between processes**, not authentication. Anyone who
can launch the binary can launch an author session. Remote access would need a transport
with real authentication, which is out of scope.

## Revisions, concurrency and retries

- The session revision starts at 0 (or 1 when a project is preloaded). Every accepted
  authoring mutation increments it, and every response reports it.
- Mutations require `expected_revision == revision`; otherwise they fail with
  `stale_revision` and change nothing. Accepted changes are serialized by the single-threaded
  request loop.
- `snapshot.restore` also increments the revision. It never rewinds to the captured revision,
  so a write based on any pre-restore revision (including the snapshot's own) stays stale.
- `request_id` is correlation only; there is no deduplication. Retrying a mutation whose first
  delivery succeeded yields `stale_revision`. Retrying `episode.act` with `expected_tick`
  yields `stale_tick` instead of moving twice.

## Episodes (editor/play separation)

`episode.start` copies the scene. Later edits never affect a running episode, and
`episode.reset` replays the same frozen scene. Turn order is specified in
`agentforge_core::episode` and published by `discover` (`game.tick_resolution_order`):

1. reject if ended, out of bounds, wall, or locked door (nothing changes);
2. tick += 1;
3. apply: move (collect an uncollected key on entry), wait, or forfeit (fails immediately);
4. win if on an exit and the win condition holds;
5. otherwise fail if `tick == max_ticks`.

There are no autonomous entities, timers or randomness, so no simulation work is pending
between ticks. Snapshots therefore capture entities, rules, the ID allocator, tick, player,
inventory, outcome, events and the step log, which is the complete state needed to continue.

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
pin the byte encoding, were cross-checked against an independent Python implementation, and
run in CI on Linux and macOS. Identity across other platforms or future engine versions is
not claimed. Replays declare `format_version` and `state_hash_version`; mismatches are
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
  u32 id, u8 kind                      (1 player, 2 wall, 3 key, 4 door, 5 exit)
  u8 color                             (0 none, else 1 + index in red, green, blue,
                                        yellow, purple, orange, cyan, white)
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
u8 outcome                             (0 in progress, 1 won, 2 failed: max_ticks, 3 failed: forfeited)
u32 outcome_tick                       (0 while in progress)
```

Each key color appears at most once per scene, so the inventory also determines which keys
were collected. Session metadata (revisions, ids of episodes and snapshots, paths) is never
hashed.

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

- **geometric**: walls block; doors are open; keys, win condition and `max_ticks` are
  ignored. If this fails, walls are the cause.
- **gameplay**: breadth-first search over (cell, keys held) with the real rules. With at most
  64×64 cells and 8 key colors, the search visits at most about a million states and is run
  to exhaustion, so "unreachable" is a proof. Non-consumable colored keys are what keep it
  exhaustive and bounded; consumable keys would make the state space grow exponentially with
  the number of doors.

When gameplay fails, the **frontier** (walls and never-openable doors adjacent to the
reachable region) is reported. Removing anything not on the frontier cannot enlarge the
region, so single-removal repairs only consider frontier entities, up to 64 per query. Each
candidate is verified by re-running the search without it, and only verified candidates are
listed. If none works, the report says that at least two changes are needed. It never
guesses a culprit.

## Schemas and documentation without drift

- Argument and result types derive `JsonSchema` (schemars 1.x) and reject unknown fields.
- `OpId` is the single source of truth: the dispatcher matches on it exhaustively;
  `discover`, `docs/protocol/protocol.schema.json` and `docs/protocol/operations.md` are
  generated from it.
- Tests check that every published example parses into its argument type and that the
  committed docs equal the generator output (`agentforge schema --check`, also run in CI).

## Persistence

- Project files (`agentforge.project` v1) hold named scenes; replay files hold
  `agentforge.replay` v1. Both carry `format` and `format_version`, which are checked
  **before** strict parsing so files from newer versions get a precise "unsupported version"
  error. Content is then fully validated (scene invariants, ids within the allocated range)
  before the session changes.
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
- **No randomness.** Nothing in milestone 1 needs it. A future seeded RNG would be part of
  the snapshot and hash encoding (with a version bump).
- **Rejected moves do not consume ticks.** `legal_actions` is then exactly the set of
  accepted actions, and an illegal action is a no-op with an explanation.

## Deferred work

These are extension points only; none is implemented.

- **MCP adapter.** Expose the same `OpId` dispatcher as MCP tools over stdio, so interactive
  agents can call it without holding a pipe themselves.
- **Physics worlds.** A second world kind backed by Rapier with its determinism feature,
  behind the same session, revision, snapshot and replay contract. Hash encoding v2 would
  cover its state.
- **Richer rules.** A bounded declarative rule format for new mechanics (enemies, switches,
  timers), informed by VGDL, PuzzleScript and Griddly GDY rather than arbitrary code.
- **Session-level replays** that include authoring edits; persisted snapshots.
- **Hidden information**: per-role observation filters, with tests that prove nothing leaks.
- **Viewer**: a live read-only visualizer fed by the same observations.
