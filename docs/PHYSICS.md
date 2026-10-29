# 2D rigid-body physics

The physics extension uses Rapier 0.36.1 (`rapier2d-f64`, `enhanced-determinism`, no parallel
solver) behind an independent core crate. It runs headlessly through the same versioned
JSON Lines envelopes and bounded stream reader as the grid engine. Physics has a separate
launcher and project format so existing grid projects, commands, and replays keep their
meaning.

```sh
cargo build --workspace --release --locked
cargo run --release -p agentforge-cli -- physics demo --out out/physics
cargo run --release -p agentforge-cli -- physics replay verify out/physics/replay.json
cargo run --release -p agentforge-cli -- physics bench --worlds 20 --ticks 600
cargo run -p agentforge-cli -- physics schema --check
```

The demo uses deterministic scripted **protocol clients**. An author moves a goal sensor
above a horizontal launch trajectory and saves a project. A separate player launches the
ball and fails. The author inspects the observed trajectory, moves the goal onto it through
an atomic edit batch, saves, and launches a fresh player that wins. A fresh process verifies
the exported replay. A second fixture runs a falling ball under gravity. This demonstrates
agent access to actual simulated motion; it does not show an LLM solving the game.

## Agent access

```sh
cargo run -p agentforge-cli -- physics serve --role author
cargo run -p agentforge-cli -- physics serve --role player \
  --project out/physics/launch.physics.json
```

Send one JSON request per line. Logs go to stderr. For example:

```json
{"request_id":"caps","protocol_version":1,"operation":"discover","arguments":{}}
{"request_id":"play","protocol_version":1,"operation":"episode.start","arguments":{}}
{"request_id":"push","protocol_version":1,"operation":"episode.act","arguments":{"expected_episode_id":1,"expected_tick":0,"action":{"type":"impulse","impulse":{"x":1.0,"y":0.0}}}}
```

`discover` publishes generated input/output schemas, example arguments and results, and
limits for the operations allowed by the launcher-fixed role. The committed
[physics schema](physics.schema.json) contains an example for every supported operation.
`physics schema` regenerates it; `physics schema --check` detects drift.

| Operations | Capability and behavior |
| --- | --- |
| `discover`, `session.status` | Both roles; capabilities and session state |
| `scene.set`, `scene.get`, `scene.apply_batch` | Author; create/replace, inspect, edit bodies/settings |
| `scene.validate`, `scene.svg` | Author; validate configured control/objective, preview initial poses |
| `scene.save`, `scene.load` | Author; atomic save and validated load of a versioned physics project |
| `episode.start`, `episode.reset`, `episode.observe` | Both; one active episode, structured observation |
| `episode.act`, `episode.events` | Both; exactly one fixed tick per accepted action, latest tick's events |
| `snapshot.capture`, `snapshot.restore` | Author; checkpoint and restore a complete continuation |
| `replay.export` | Both; inline recording, optional author-only file path |
| `replay.verify`, `replay.verify_file` | Author; verify each recorded input/hash inline or from a bounded file |

Authoring writes require `expected_revision`. Failed batches leave bodies, settings, ID
allocator, and revision untouched. Scene changes are rejected while that host's episode is
running; finish or forfeit it first. Player hosts bind their saved scene at launch and
cannot author, load files, restore snapshots, verify arbitrary replays, or choose output
paths. This is local capability separation; it is not remote authentication. Observations
are fully observable and omit authoring labels. Player replay exports omit labels too.

Recordings may grow beyond the 1 MiB request limit. Export them to an author-chosen file
and use `replay.verify_file` with `{ "path": "episode.physics.replay.json" }`; file reads
are bounded to 64 MiB and validate compatibility before verification. Players return
recordings inline; a trusted host/client can persist them for an author to verify.

Request IDs correlate responses and do not deduplicate. `episode.act` requires both
`expected_episode_id` and `expected_tick`: duplicates fail with `stale_tick`, and actions
from another timeline fail with `stale_episode`. Start, reset, and restore issue fresh
episode IDs. Restore advances the authoring revision rather than rewinding it. After a
lost response, observe state before deciding whether another action is needed.

## Simulation contract

- SI units: meters, seconds, kilograms; density is kg/m²; impulses are N·s; rotation is
  counterclockwise radians. Physics Y points **up** (grid Y points down).
- Shapes: ball and rotated box. Bodies: fixed or dynamic, one collider each. Sensors detect
  overlap without solid response. Density, friction, restitution, damping, gravity,
  bounds, control impulse limits, and objective references are data.
- Actions: `wait`, `impulse` (applied to the configured controlled body at its center of
  mass), `forfeit`. An accepted action advances one timestep of `1 / ticks_per_second`.
- Update order: validate action and impulse budget; apply impulse and wake body; step
  Rapier; gather sorted contact/overlap differences; evaluate outcome; hash and record.
- Outcome precedence: numerical failure, explicit forfeit, objective overlap/win, controlled
  or objective body's center outside bounds, tick limit, otherwise in progress. Other
  bodies leaving bounds do not fail the episode. Wins are sampled after a step, including
  initially overlapping scenes. Sensor crossing between samples is not a guaranteed win.
- Continuous collision detection is enabled on dynamic solid bodies; it does not provide
  continuous sensor-trigger gameplay semantics.
- Limits: 256 bodies, 30–240 Hz, at most 36,000 ticks, 32 session snapshots, 256 edits per
  batch, 1 MiB input lines. Scene numeric bounds and further constraints are enforced by
  `scene.rs` and exposed in its generated type descriptions.
- No random numbers, wall-clock simulation, external callbacks, or model in the tick loop.
  Rapier bodies are inserted in stable ID order. Contact and overlap pairs use sorted sets.

There is no physics path solver. Validation checks data/reference invariants and whether
control/objective are configured. It does not prove a scene winnable. Demo diagnosis
compares its measured trajectory with the sensor position; a different impulse could have
a different result.

## Checkpoints, replays, determinism

Rapier contains solver caches, sleeping state, contact manifolds and broad-phase state.
Serializing only transforms and velocities would lose continuation state. Instead,
checkpoints capture the initial validated scene plus **every accepted input** and per-tick
hash. Restore builds the same world and executes those inputs again, reconstructing all
solver state; it never calls a model. Restoring therefore takes O(recorded ticks) simulation
work. The authoring scene, allocator and save path are captured separately as well.
Snapshots live in memory; replay files are durable checkpoints.

Physics replays declare format/version, engine version and physics compatibility ID
`rapier2d-f64-0.36.1-enhanced-determinism-v1`. Incompatible files and hash divergence are
rejected. `Cargo.lock` pins transitive dependencies. Floating-point JSON uses round-trip
parsing. Reproducibility is tested on macOS arm64 with Rust 1.99.0 and this dependency set;
other platforms, compiler settings, dependency upgrades, and future versions are unverified.
The `enhanced-determinism` feature alone is not evidence of cross-platform identity.

Canonical SHA-256 input uses domain strings with a little-endian u32 byte length, fixed
little-endian integers, and raw IEEE-754 f64 bits. Signed zero retains its bit pattern.
Bodies are ordered by ID, pairs lexicographically. Labels and host revision/episode IDs
are excluded. See [hash.rs](../crates/agentforge-physics/src/hash.rs) for the exact field
order and tag values, pinned by independent Python-derived golden tests:

- Scene domain `agentforge.physics.scene.v1`: settings, optional objective/control tags,
  allocator, body count, bodies with their kind, shape, initial pose/velocity/material/
  damping/sensor fields.
- State domain `agentforge.physics.state.v1`: scene digest, tick, outcome, impulses used,
  ordered body positions, unit-complex rotations, velocities and sleeping flags, then
  counted contact and overlap pairs. This is a visible-state diagnostic hash; continuation
  additionally depends on solver history reconstructed by the checkpoint.
- Recording domain `agentforge.physics.recording.v1`: scene and initial hashes, step count,
  then tick/action/hash for each step, then final hash. Distinct routes can share a final
  state while retaining different recording fingerprints.

`physics bench` reports build mode, architecture, workload, world and tick counts, and
elapsed time. The direct simulation interval includes rule resolution and hashing; the
separate protocol interval includes JSON parse/dispatch/response serialization with full
observations. Initialization is excluded. Process/pipe overhead is measured separately by
the existing grid `bench` command, not included in these physics timings.

## Remaining scope

This extension supports small scenes of rigid balls and boxes, center impulses and sensor
goals. Joints, articulated bodies, kinematic controllers, arbitrary scripted rules,
continuous sensor sweeps, 3D, a live viewer and remote transports remain deferred.
