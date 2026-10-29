# AgentForge

A deterministic game engine whose users are AI agents. Agents create scenes, configure
rules, inspect state, run and test games, diagnose failures, repair them and save runnable
projects entirely through a versioned **JSON Lines protocol** on stdin/stdout. No visual
editor, GUI, GPU, account, credentials or model API is involved, and no language model runs
inside the simulation loop: the engine executes the rules itself.

> **Status: milestone 1.** A small, complete vertical slice: a turn-based 2D integer-grid
> dungeon (players, walls, colored keys, matching doors, exits). It is a foundation, not a
> general-purpose engine: there is no physics, real-time simulation, rendering pipeline or
> scripting language yet (see [Roadmap](#roadmap)). "AgentForge" is a temporary internal
> working name, not a claim of trademark availability.

## What works

- **Authoring through the protocol**: projects with multiple scenes; scenes from ASCII layouts
  or entity-by-entity; create/inspect/update/remove entities; rule parameters (`max_ticks`,
  `win_condition`) are data; atomic edit batches; optimistic concurrency via
  `expected_revision`.
- **Play through the protocol**: episodes on a frozen copy of a scene, full observations
  (structured + ASCII + legal actions), one accepted action = exactly one logical tick,
  rejected actions change nothing and explain why (`door_locked` names the key and where it
  is), exactly-once retries via `expected_episode_id` + `expected_tick`, events, assertions.
- **Diagnostics that say what is actually wrong**: geometric (walls-only) vs gameplay
  (keys, doors, exits that end the episode, win condition, `max_ticks`) reachability,
  computed by exhaustive search, so "unsolvable" is a proof; named blockers (walls, doors
  whose key is missing or unreachable, unreachable required keys); single-removal repairs
  the engine has *verified* by re-solving, drawn from every entity whose removal could
  matter, so "no single removal works" is also a proof.
- **Capability-separated sessions**: the launcher fixes each process's role
  (`--role author|player`). Player processes get a saved project and one scene; they cannot
  edit, restore, choose files or switch scenes. Requests cannot change a role.
- **Determinism and persistence**: integer-only state, ordered collections, no clocks or
  randomness; canonical SHA-256 state hashes over a documented byte encoding (pinned by
  golden tests cross-checked with an independent implementation); snapshots; atomic,
  versioned project and replay files; replays that re-execute recorded inputs and compare
  every per-tick hash.
- **Self-description**: `discover` returns the game model, conventions, limits, every
  callable operation with an example, and JSON Schemas generated from the Rust types.
- **Visual inspection**: `scene.render` writes an SVG with entity ids labelled (no GUI needed).

## Quick start

Requirements: a Rust toolchain ≥ 1.99. The exact toolchain is pinned in
`rust-toolchain.toml`; everything below was verified with rustc 1.99.0 on macOS (arm64).
Continuous integration is not active yet (see [Limitations](#limitations)).

```sh
cargo build --workspace --release

# The full loop: author creates a dungeon with an unreachable exit -> a player process fails
# -> the engine diagnoses it -> the author applies the engine-verified repair -> a fresh
# player process wins -> a second scene is won through the same commands -> a fresh process
# re-verifies the winning replay.
cargo run --release -p agentforge-cli -- demo --out out/demo

# Re-verify the winning replay yourself in another fresh process. Replay file names are
# content-addressed (scene, outcome, ticks, recording fingerprint), so this one is stable:
cargo run --release -p agentforge-cli -- replay verify \
  out/demo/replays/blocked_dungeon-won-14t-5afdce3c1ed150fd.replay.json \
  --project out/demo/project.agentforge.json
# or, with jq:  replay verify "$(jq -r .second_attempt.replay out/demo/summary.json)" --project ...
```

The demo writes `out/demo/` with the saved project, `replays/`, before/after SVGs,
`summary.json`, and `transcript.jsonl`: every request and response that crossed the wire.

**Honesty note:** the demo's builder and player are deterministic *protocol clients*, not
language models. Their success shows that the protocol supports the whole loop without
touching engine internals (the client crate does not even depend on the engine crates, and
talks to `agentforge serve` child processes). It is not evidence that an LLM solved
anything. The builder does not hard-code the repair: it applies the first repair the
engine's diagnosis reports as verified.

## Driving the engine (for agents and their developers)

```sh
agentforge serve --role author                       # trusted authoring session
agentforge serve --role player --project p.agentforge.json --scene NAME --replay-dir DIR
```

Write one JSON request per line; read one JSON response per line. Start with `discover`:

```json
{"request_id":"1","protocol_version":1,"operation":"discover","arguments":{}}
{"request_id":"2","protocol_version":1,"operation":"project.create","arguments":{"name":"demo","expected_revision":0}}
{"request_id":"3","protocol_version":1,"operation":"scene.create","arguments":{"name":"hall","expected_revision":1,"layout":["#######","#r@R.E#","#######"],"rules":{"max_ticks":20,"win_condition":"reach_exit"}}}
{"request_id":"4","protocol_version":1,"operation":"episode.start","arguments":{"scene":"hall"}}
{"request_id":"5","protocol_version":1,"operation":"episode.act","arguments":{"action":{"type":"move","direction":"west"},"expected_tick":0}}
```

A whole session can be piped: `agentforge serve --role author < session.jsonl`. See
[docs/PROTOCOL.md](docs/PROTOCOL.md) for conventions (revisions, retries, errors, roles) and
[docs/protocol/operations.md](docs/protocol/operations.md) for every operation with an
example. Machine-readable schemas: [docs/protocol/protocol.schema.json](docs/protocol/protocol.schema.json).

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                               # unit, protocol-boundary and process-level tests
cargo run -p agentforge-cli -- schema --check        # generated docs match the code
```

## Benchmark

```sh
cargo run --release -p agentforge-cli -- bench --worlds 200 --ticks 2000 --process-worlds 1
```

It reports the workload, build profile, world count, ticks and elapsed time for each layer
separately. One run on an Apple Silicon laptop (release build, single thread) measured:

| Layer | µs per tick |
|---|---|
| `simulation` (rule resolution + SHA-256 state hash) | 0.15 |
| `protocol_in_process` (JSON parse, dispatch, serialize; no observation) | 3.2 |
| `protocol_in_process_with_observation` (~5.5 KB observation per response) | 38.6 |
| `process_pipe` (child process over pipes, one request in flight) | 42 |

These are measurements of one run on one machine, not targets. They do show where the cost
is: building and serializing full observations dominates, so clients that do not need an
observation after every move should pass `"include_observation": false`.

## Repository layout

| Path | Contents |
|---|---|
| `crates/agentforge-core` | Pure deterministic simulation: scenes, rules, episodes, analysis, hashing, replays, SVG. No I/O. |
| `crates/agentforge-host` | Protocol types and catalog, generated schemas, capability-scoped sessions, revisions, snapshots, atomic storage, JSON Lines server. |
| `crates/agentforge-cli` | The `agentforge` binary: `serve`, `demo`, `replay verify`, `schema`, `bench`. |
| `crates/agentforge-demo` | Scripted protocol clients (builder, player). Depends on no engine crate. |
| `fixtures/` | Two scene fixtures (different layouts and rule parameters), created through the protocol. |
| `docs/` | Architecture notes, protocol guide, generated operation reference and JSON Schema. |

## Limitations

- **Fully observable only.** Observations contain the whole board; there is no hidden
  information and no hidden-information protection.
- **Turn-based integer grid only.** No physics, continuous space, real-time ticking,
  autonomous entities or randomness.
- **Fixed rule vocabulary.** Five entity kinds and two rule parameters; agents can build
  levels and tune rules, not invent new mechanics yet.
- **Local capability separation, not authentication.** Roles are fixed per process by
  whoever launches it. There is no network transport and no remote auth.
- **Interactive use needs a held-open pipe.** Coding agents whose shell runs one command at
  a time must either pipe whole scripts or keep a process open themselves. An MCP adapter
  would remove this friction; it is not built yet.
- **Reproducibility scope.** Replays verify against the same replay format and hash-encoding
  version, and are tested with this engine version on macOS (arm64). Other cross-version
  or cross-platform identity is not claimed.
- **No CI yet.** All checks were run locally. A GitHub Actions workflow (fmt, clippy,
  tests, demo and replay verification on Linux and macOS) is prepared but not yet published,
  because publishing workflow files needs a token with the `workflow` scope.
- **Session scope.** One active episode per session; snapshots live in memory only; replays
  cover one episode and do not include authoring edits.

## Roadmap

These are not implemented; the [architecture notes](docs/ARCHITECTURE.md#deferred-work)
explain where each would fit:
an MCP server adapter over the same dispatcher; deterministic physics as a second world kind
(via [Rapier](https://rapier.rs/)'s cross-platform determinism mode rather than custom
physics); a richer declarative rule format (informed by VGDL, PuzzleScript and Griddly's
GDY); persisted snapshots and session-level replays that include edits; a live viewer;
hidden-information games.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option. Unless you explicitly state otherwise, any
contribution intentionally submitted for inclusion in this work, as defined in the
Apache-2.0 license, shall be dual licensed as above, without any additional terms or
conditions.
