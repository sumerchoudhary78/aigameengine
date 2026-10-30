# Protocol guide (version 1)

The complete operation reference with an example for every operation is generated:
[protocol/operations.md](protocol/operations.md). Argument and result JSON Schemas:
[protocol/protocol.schema.json](protocol/protocol.schema.json). A running engine describes
itself through `discover`. This guide covers the conventions those references assume.

## Transport

- Launch `agentforge serve --role author` (trusted) or
  `agentforge serve --role player --project FILE --scene NAME [--replay-dir DIR]` (restricted).
- Write one JSON object per line to stdin (UTF-8, at most 1 MiB per line). Read exactly one
  JSON response line per request from stdout, in order. Blank lines are ignored.
- Logs go only to stderr (`AGENTFORGE_LOG=info|debug` to see them). Closing stdin ends the
  session.
- A malformed line gets a structured error and the session continues. An oversized line is
  skipped up to its newline and answered with `request_too_large`.

## Envelopes

Request:

```json
{"request_id": "7", "protocol_version": 1, "operation": "episode.act", "arguments": {"action": {"type": "move", "direction": "west"}, "expected_tick": 0}}
```

- `request_id`: 1-128 characters, echoed back. It is **for correlation only**; the engine
  does not deduplicate.
- `protocol_version`: must be supported (`1`).
- `arguments`: may be omitted for operations without arguments. Unknown fields are rejected
  at every level, so typos fail loudly.

Success (real output):

```json
{"request_id": "6", "protocol_version": 1, "operation": "episode.act", "status": "ok", "revision": 2, "result": {"episode_id": 1, "tick": 1, "action": {"type": "move", "direction": "west"}, "events": [{"tick": 1, "type": "moved", "from": {"x": 2, "y": 1}, "to": {"x": 1, "y": 1}, "direction": "west"}, {"tick": 1, "type": "key_collected", "key": 9, "color": "red"}], "outcome": {"status": "in_progress"}, "state_hash": "sha256:b93fc5d8946569c8306d34d9faad196aa43b471df04bd0d421bd7270e5827991"}}
```

Error (real output): a stable `code`, a concise `message`, relevant `details`, and a `hint`
when a recovery is known.

```json
{"request_id": "5", "protocol_version": 1, "operation": "episode.act", "status": "error", "revision": 2, "error": {"code": "door_locked", "message": "door #11 at (3, 1) is locked; it requires the red key", "details": {"from": {"x": 2, "y": 1}, "direction": "east", "target": {"x": 3, "y": 1}, "door": 11, "required_key": "red", "legal_actions": [{"type": "move", "direction": "west"}, {"type": "wait"}, {"type": "forfeit"}], "tick": 0}, "hint": "Collect the red key first: entity #9 at (1, 1)."}}
```

`revision` is in every response. `request_id` is `null` only when it could not be read
(for example, malformed JSON).

## Roles

The launcher fixes the role per process; nothing in a request can change it. Player
sessions may call `discover`, `session.status`, `episode.start|reset|observe|act|events|assert`
and `replay.export`, only for their bound scene. `discover` lists only the operations the
current session may call. Calling anything else returns `permission_denied`. This is local
process separation, not authentication.

## Revisions and retries

- Every accepted authoring mutation increments `revision`. Mutating operations
  (`mutates: true` in `discover`) require `expected_revision` equal to the current revision.
  Otherwise they fail with `stale_revision` and nothing changes:

  ```json
  {"request_id": "3", "protocol_version": 1, "operation": "entity.create", "status": "error", "revision": 2, "error": {"code": "stale_revision", "message": "expected_revision 1 does not match the current revision 2; nothing was changed", "details": {"expected_revision": 1, "current_revision": 2}, "hint": "Re-read the state you depend on (session.status, scene.get), then retry with the current revision if your change still applies."}}
  ```

- `snapshot.restore` increments the revision too, so revisions from before a restore stay
  stale.
- **Retrying safely.** If a mutation's response was lost, resending it with the same
  `expected_revision` returns `stale_revision` when the first attempt already applied; check
  the state instead of resending blindly. For `episode.act`, pass `expected_episode_id` and
  `expected_tick`. An action meant for an episode that was since reset, restarted or
  restored returns `stale_episode` (episode ids are never reused, and a restored episode
  gets a new one). A duplicate returns `stale_tick` instead of moving twice (real output):

  ```json
  {"request_id": "7", "protocol_version": 1, "operation": "episode.act", "status": "error", "revision": 2, "error": {"code": "stale_tick", "message": "expected_tick 0 does not match the current tick 1; the action was not applied", "details": {"expected_tick": 0, "current_tick": 1}, "hint": "If a previous attempt of this action succeeded, do not resend it; call episode.observe."}}
  ```

- `scene.apply_batch` is atomic. If edit *i* fails, the error carries `details.edit_index = i`
  and **no** edit of the batch is applied.

## Typical flows

**Author** (`--role author`): `discover` → `project.create` or `project.load` →
`scene.create` (from an ASCII `layout`) → `scene.validate` / `analysis.reachability` → edit
with `entity.*` or `scene.apply_batch` → `project.save` → (optionally) `scene.render` to SVG.

**Player** (`--role player`): `episode.start` → read `observation.legal_actions`, `ascii`,
`objective` → `episode.act` with `expected_episode_id` and `expected_tick` until the outcome
leaves `in_progress` → `replay.export` (written to the launcher's `--replay-dir` under a
content-addressed name, without authoring labels).

**Diagnosis**: `analysis.reachability` returns `verdict`, a one-paragraph `summary`, the
walls-only (`geometric`) and rules-aware (`gameplay`) answers with paths, frontier `blockers`,
and `repairs.single_removals`. Every listed removal has been verified by re-solving, and
the candidates cover every entity whose single removal could matter. When
`repairs.exhaustive` is true and the list is empty, at least two changes are needed.
`assumptions` lists the model behind the answer.

## Game model summary

Grid coordinates are integers: x grows east, y grows south, and (0, 0) is the north-west
corner. Each cell holds at most one entity. Entity kinds are `player` (one spawn), `wall`,
`key` (colored, collected on entry, never consumed, at most one per color), `door`
(passable while holding the matching key), `exit`, `ice`, `one_way` (entry direction),
`hazard` (entering fails the episode) and `portal` (pair id 0-9; exactly two per pair).
Entering ice starts a slide; the whole slide is part of the move and costs one tick. Win
conditions are `reach_exit` and `collect_all_keys_then_exit`. Failure happens at
`max_ticks`, on `forfeit`, or on entering a hazard. ASCII glyphs: `#` wall, `@` player,
`E` exit, `~` ice, `!` hazard, `^ > v <` one-way entry directions, `0`-`9` portals,
lowercase `r g b y p o c w` keys, uppercase `R G B Y P O C W` doors, `.` floor. The game is
fully observable.

## Limits

Published by `discover` under `constraints`: grids up to 64×64, `max_ticks` up to 10000,
8 key colors, 10 portal pair ids, 4096 edits per batch, labels and names up to 64
characters, 64 snapshots per session, 1000 events per query, 256 assertions per call, 64
verified repair candidates per reachability query, and 1 MiB per request line.
