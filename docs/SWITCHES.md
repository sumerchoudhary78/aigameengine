# Switches and persistent gates

This note specifies the first **stateful** mechanic: **switches** that toggle **gate
groups**, where the open/closed state of every gate survives ticks. It records the model,
the exact toggle timing, the encoding, the analysis treatment, the rejected alternatives and
the test matrix. It is the reference the code matches, and it extends the terrain model of
[TERRAIN.md](TERRAIN.md).

## 1. Scope

In scope:

- two new entities: a **switch** (a pressure plate on a cell) and a **gate** (a barrier on a
  cell), both belonging to a numbered **group**;
- entering a switch toggles its group: every gate of that group flips closed ↔ open;
- gate state persists across ticks, is part of the episode state, the state hash and the
  exhaustive analysis search.

Out of scope:

- one-shot (latching) switches, switches that stay pressed, timers, monster-activated
  plates, objects other than the player triggering switches;
- more than four gate groups per scene (§3);
- sliding or movable gates (a gate is at one cell, like every entity).

## 2. Model

A scene stays a finite grid `V = [0,w) × [0,h)`, `|V| ≤ 4096`, with a static partial map
`cell : V ⇀ Entity`. There is now a set `G` of **gate groups present in the scene** (a group
is present if it labels at least one switch or gate), with `|G| ≤ 4` (§3). The episode state
becomes

```
S = (tick, player ∈ V, inventory ⊆ K, gates ⊆ G, outcome)
```

`gates` is the set of groups whose gates are currently **open**. A new episode starts with
`gates = ∅`: every gate starts closed. There is no per-gate initial state to author and no
way for two gates of one group to disagree — the state is exactly one bit per group.

The transition function is `δ : S × Actions ⇀ S`, and for `move(d)`:

```
plan     := movement::plan(board, player, d, inventory, gates)   — reads gates(t)
outcome  := plan result (move, stop effect, hazard, exit, max_ticks)
gates'   := gates Δ {g}   if the plan's plan.stop was a switch of group g, else gates
```

The important formal rule is the **toggle timing** (§5): the plan reads gates as of the
start of the action, and the toggle applies only afterwards. One action never opens a gate
*and* walks through it.

## 3. Why at most four groups, and an analysis state cap

Toggling gates is a reversible process; "Switches"-style puzzles are PSPACE-complete in
general (§13), so no fixed group budget makes the general problem easy. What the budget
buys is a bound that is *checkable and stated up front*:

- the gameplay search now explores `(cell, keys held, gate state)`, i.e. at most
  `4096 × 2^8 × 2^g` states for `g` gate groups;
- with `g ≤ 4` the worst factor is ×16 over the terrain-era bound;
- a scene whose product exceeds `analysis::MAX_EXHAUSTIVE_STATES` (= 2^22) is **not
  searched**: `analysis.reachability` fails with a limit error and `scene.validate` reports
  an `analysis_limit` warning. The engine never trades the exhaustiveness proof for a
  heuristic verdict — better an explicit refusal than a guessed "unreachable".

Four groups is also an authoring budget that fits the glyph table (§7): groups are
identified by paired punctuation glyphs in ASCII layouts, and misreading `1` as a portal
pair id vs. a gate group was the alternative to avoid. Raising the limit later is a
documented, hash-version-bumped change, not a silent one.

| mechanic | extra state | effect on exhaustive search |
|---|---|---|
| static terrain | none | `V × 2^K` unchanged |
| switch/gate groups | 1 bit per group | × `2^g`, `g ≤ 4`, with a hard state cap |
| movable block | block position | × `V` (not in this milestone) |
| per-gate independent state | 1 bit per gate | × `2^(#gates)` (rejected, §12) |

## 4. Cell classes

The terrain table of TERRAIN.md §4 gains two rows. "Stop" = enter, apply effect, end the
displacement; "Pass" = enter and, if sliding, keep going.

| entity | enterable | on entry | continues a slide |
|---|---|---|---|
| gate, group g, **closed** | no (like a wall) | — | — |
| gate, group g, **open** | yes | nothing | yes (terrain, not an object) |
| switch, group g | yes | toggle group g's bit | no (object) |

Consequences:

1. **A switch is an object; an open gate is terrain.** This preserves the terrain-era
   invariant that objects stop slides, and a slide therefore crosses open gates but stops on
   a switch. A slide cannot trigger a switch *and* continue, which is what makes the toggle
   timing (§5) unambiguous.
2. **A closed gate is an obstacle, not a hazard.** Moves into it are rejected (first target)
   or stop a slide before entry, exactly like a wall. It never fails the episode.

## 5. Resolution order and toggle timing

The tick pipeline of TERRAIN.md §5 becomes:

1. **Reject** if the episode has ended, or the first move target is outside the grid, a
   wall, a locked door, a one-way tile against its direction, or a **closed gate**.
   Rejection changes nothing.
2. **Advance the tick** by one.
3. **Resolve the displacement** exactly as before. The plan evaluates gate passability
   against `gates(t)` — the state as of the start of this action.
4. **Apply the switch toggle**, if the plan's `stop` was a switch: `gates := gates Δ {group}`
   and emit `SwitchPressed { switch, group, open }` (after `Moved`, before any hazard/exit
   handling; a switch cell is neither a hazard nor an exit, so those never co-occur).
5. Terminal entry effects (hazard fail, exit win), then `max_ticks`, as before.

Events for a move are unchanged plus at most one new event:

```
Moved { from, to }
[SwitchPressed { switch, group, open }]   open = the state after the toggle
[PortalUsed | KeyCollected | DoorPassed | ExitReached | ExitRequirementsUnmet]
[HazardEntered | Won | Failed]
```

Three invariants are worth stating as theorems, because they are the reason the timing is
safe:

1. **At most one toggle per action.** A switch is an object, so any entry into a switch ends
   the displacement. A slide cannot pass over a switch.
2. **An action cannot both toggle and traverse.** The plan reads `gates(t)`; the toggle
   writes `gates(t+1)`. A gate opened by this move is passable from the next move on.
3. **Nothing is inside a gate when it closes.** A group's bit flips only when the player
   *enters a switch*, so after any toggle the player stands on the switch, never on a gate
   of that group. There are no other moving entities.

`wait` toggles nothing. Rejected moves toggle nothing. One authoring caveat is enforced in
§9: a player cannot spawn on a switch or on a gate, so the start state is always well
defined with `gates = ∅`.

## 6. Movement plan

`movement::plan(board, from, direction, inventory, gates, removed, on_enter)` gains the
`gates` argument; a closed gate behaves exactly like a wall in both the reject-on-first-step
and stop-a-slide paths:

```
occupant match (before entry):
    wall                    -> reject/stop
    door without key        -> reject/stop
    one-way wrong way       -> reject/stop
    gate {group} with group ∉ gates -> reject/stop (MoveBlock::ClosedGate)
stop match (after entry):
    switch {group}          -> stop, Stop::Switch (episode toggles afterwards)
    key / door / exit / portal / hazard -> as before
passability match (terrain):
    ice                     -> sliding = true
    floor / one-way / open gate -> {}
```

Termination, the one-hop portal rule and straight-ray slides are unchanged.

## 7. Encoding

### 7.1 ASCII glyphs

Groups are `0..=3`. A layout glyph sits in a switch/gate pair so authoring mistakes are
visible at a glance:

| glyph | meaning |
|---|---|
| `(` `[` `{` `-` | switch of group 0, 1, 2, 3 |
| `)` `]` `}` `=` | gate of the same group |

The pairs are the opening/closing halves of the common brackets, plus the keyboard-neighbour
pair `-`/`=`. All are plain ASCII, none needs escaping in JSON layout rows, and none
collides with the terrain, portal, key or door glyphs. The legend (`discover` and
observations) is generated from the same table, so it cannot drift.

### 7.2 Entity kinds

```json
{"type":"switch","group":0}
{"type":"gate","group":0}
```

`group` is in `0..=3`; `Scene` rejects anything else (`invalid_gate_group`), symmetric to
`pair` for portals. Multiple switches and gates may share a group; they all flip together.

### 7.3 Canonical hashing

The scene digest keeps its layout and gains two kind codes and an attribute rule:

```
u8 kind      ..., 9 portal, 10 switch, 11 gate
u8 attribute switch/gate: 1 + group index
```

Pre-existing entities hash exactly as before, so **existing scene digests keep their
values** even though `STATE_HASH_VERSION` moves on (§ below).

The **state hash changes**, so `STATE_HASH_VERSION` is bumped to `2` and the state domain
becomes `agentforge.state.v2`:

```
str("agentforge.state.v2")
32 raw bytes of the scene digest
u32 tick
i32 player_x, i32 player_y
u8 inventory bitmask
u8 gates bitmask                       (bit i = group i open)      ← new
u8 outcome, u32 outcome_tick
```

Rationale: gate state is simulation state that survives ticks, so it must be hashed, and
appending a byte changes every digest; there is no encoding under which v1 state hashes
survive. Keeping v1 alive in parallel would mean a second `state_hash` implementation and a
version-dispatched verifier for zero user benefit — replays are short-lived, local
artifacts. v1 replays are therefore **rejected explicitly** (`replay.state_hash_version 1 is
not supported (supported: 2)`), never silently misverified. Scene digests are unchanged, so
the scene-digest golden tests and the strict-extension property of the terrain milestone
still hold; the state-hash golden tests pin the new byte.

## 8. Analysis

### 8.1 Gameplay search

The BFS explores `(cell, keys held, gate state)`. The gate dimension is packed only over
groups that have **at least one gate** (a switch whose group has no gate cannot change any
transition, so its toggles are a no-op for reachability and add no bit). The start state has
`gates = ∅`. Every transition still comes from `movement::plan`, so play and proof cannot
drift. A switch stop toggles that group's bit in the successor state.

Before allocating, the search checks the state bound:

```
cells << (key_bits + gate_bits) ≤ MAX_EXHAUSTIVE_STATES      (2^22)
```

Over the bound it returns a limit error instead of searching; `scene.validate` turns that
into an `analysis_limit` warning. When a search runs, it runs to exhaustion: "unreachable"
remains a proof. `reachable_cells`, `obtainable_keys` and `explored_states` keep their
meanings; `explored_states` now counts `(cell, keys, gates)` states.

The search also records which gates ever reported `MoveBlock::ClosedGate` from a reachable
state, so gate blockers are evidence-based (§8.3), and the path reconstruction replays the
same toggles so `keys_collected_on_path` stays correct.

### 8.2 Geometric search

Unchanged: gates count as open (they are movable obstacles like doors), switches count as
open. It stays the walls-only diagnostic.

### 8.3 Blockers

The list gains one reason, and like every reason it is emitted only for entities that the
search actually ran into:

- `closed_gate`: a gate adjacent to an active cell that blocked at least one attempted move
  in some reachable state. The summary names the group and whether the scene has a switch of
  that group.

Switches are not blockers (they never stop a needed passage).

### 8.4 Repairs and the candidate completeness argument

Repair candidates gain:

- every **gate** adjacent to an active cell — closed ones are obstacles, and removing an
  open one can change things only via state, which the same adjacency captures;
- every **switch** adjacent to an active cell — removing it can prevent or remove toggles
  (for example, deleting a switch that would close the gate you need open).

Everything else (ice, keys, exits, walls, doors, one-ways, hazards) is as in TERRAIN.md, and
portals remain excluded (a valid portal repair is a two-entity change).

Completeness argument, updated for the new kinds. A single removal can change the search
result only if some transition reads the removed entity. Every transition is attempted from
an active cell (a `(cell, keys, gates)` state, or a cell entered on a slide path). Gates and
switches are read as the immediate target of such a move or slide step, so any relevant one
is adjacent to — or sits on — an active cell, and `adjacent_entities` covers both
(entered switches and gates are themselves active cells, and the cell before them is active
too). Walls, doors, one-ways, hazards, keys, exits, entered ice behave as before. Each
candidate is verified by re-running the bounded search without it; with ≤ 4 gate groups the
repair re-search is no heavier than the original.

## 9. Validation

New completeness errors (they prevent `episode.start`):

| code | condition |
|---|---|
| `player_on_gate` | the player spawn shares a cell with some gate (all gates start closed, so the episode could not begin) |
| `player_on_switch` | the player spawn shares a cell with some switch (would make the start depend on entry-vs-occupancy semantics) |

New warnings

| code | condition |
|---|---|
| `gate_without_switch` | a gate group has no switch; its gates can never open (a deliberate "decorative wall" should use a wall) |
| `switch_without_gate` | a switch group has no gate; pressing it is a no-op |
| `analysis_limit` | the solvability search refused to run over the state cap; no proof was attempted |

Starting **on** a switch during an `analysis.reachability` query (custom `from`) is allowed
and toggles nothing: toggles require an *entry*. This is the documented asymmetry with keys,
where starting on a key counts as holding it.

## 10. Failure and edge cases

- **Move into a closed gate**: rejected with `gate_closed` (first target) or stops a slide
  before entry. No tick, no events, no hash change; `legal_actions` excludes it.
- **Open gate under a slide**: crossed like floor; the slide does not stop there.
- **Switch at the end of a slide**: the slide stops on the switch and the toggle applies —
  one event `SwitchPressed`, one tick.
- **Two switches of one group in two consecutive moves**: each toggles; doing it twice
  restores the previous state.
- **`wait`**: nothing toggles.
- **Winning or dying on the same move as a switch press**: impossible; a switch is not an
  exit or a hazard.
- **Gate cell as portal partner**: impossible (one entity per cell).
- **Group with gates but no reachable switch**: the gates behave as permanent walls;
  `scene.validate` warns `gate_without_switch`.
- **State cap exceeded**: `analysis.reachability` errors with `limit_exceeded`; play,
  replays and snapshots are unaffected.

## 11. Test matrix

| area | test |
|---|---|
| layout | all 8 glyphs parse and render round-trip; legend contains them; unknown group glyphs rejected |
| scene | groups outside 0..=3 rejected for switch and gate; snapshot round-trips gate state |
| episode | move into a closed gate is rejected with `gate_closed`; slide stops before a closed gate |
| episode | entering a switch toggles: gate opens next move; pressing again closes it |
| episode | a slide onto a switch stops and toggles (one tick, one event) |
| episode | a slide crosses an open gate as terrain; cannot cross a closed one |
| episode | an action cannot open and pass a gate; wait toggles nothing; observation `gates` tracks state |
| episode | decomposition: spawn on switch/gate rejected; toggles survive snapshot restore |
| hash | state-hash golden values change to the v2 encoding and are pinned; scene digests for non-switch scenes unchanged; replays of hash v1 rejected explicitly |
| replay | a gate episode round-trips through `Replay` and re-executes identically |
| analysis | a closed gate is a `closed_gate` blocker; its (or its switch's) removal is a verified repair |
| analysis | a switch behind a closed gate is still found as a repair candidate |
| analysis | toggling makes a walls-disconnected route solvable and un-toggling closes it again (parity) |
| analysis | state cap over 2^22 returns a limit error; validation reports `analysis_limit` |
| analysis | assumption list documents the gate state dimension |
| render | switch and gate produce SVG fragments and id labels |
| protocol | generated schema and docs contain the new kinds, glyph legend, `max_gate_groups`, `gate_closed`, `invalid_gate_group`, `switch_pressed` event |

## 12. Rejected alternatives

- **Per-gate independent state (1 bit per gate).** The classic "toggle wall" needs gates to
  agree, and per-gate state multiplies the analysis space by `2^(#gates)` and invites
  contradictory gates. Groups keep the state small and the authoring story simple.
- **Switches as latches (press once, stays open).** Monotone, but it removes the ability to
  *close* a passage (danger gates, parity puzzles), which is the point of persistent state.
  It would also need a separate "pressed" set that is distinct from gate state; the toggle
  is the smaller model.
- **Switch triggers on occupancy, not entry (incl. at tick 0).** A player spawning on a
  switch would then start with the group already toggled, and `episode.start` would depend
  on entry semantics for a state that must be well defined. Rejected via the
  `player_on_switch` completeness error instead.
- **Gate passability evaluated mid-action (toggle takes effect immediately).** Allows one
  action to flip a switch and then slide through the just-opened gate, needs the plan to
  track mutations during a ray, and ends with weird questions (slide crossing a *closing*
  gate). The start-of-action snapshot is total and terminating like the rest of `plan`.
- **Open gate stops a slide (object).** Makes an open gate a specific kind of stopper and
  breaks "objects stop slides only where they must"; an opened gate is a cleared doorway.
- **Gates open by default / per-gate initial states.** Per-gate initial state doubles the
  authoring surface and creates states where a group is partially open. `gates = ∅` at tick
  0 is one rule that covers every gate.
- **More glyph styles (letters for groups).** The free letters are non-contiguous in the
  alphabet and the bracket pairs keep switch/gate visually paired, which matters because
  layouts are the primary authoring surface.

## 13. References

- Gabor's *Switches* and Anzagira, Beyenne & Demaine, [Switches are PSPACE-Complete]
  (https://researchgate.net/publication/334806457_Switches_are_PSPACE-Complete): the
  toggle-barriers-by-walking-over-a-switch mechanism, its PSPACE-completeness at height ≥ 3,
  and the polynomial 1-row case. This is the formal reason the state cap exists.
- Anzagira, Demaine et al., [PSPACE-Completeness of Reversible Deterministic Systems]
  (https://arxiv.org/abs/2207.07229): the general frame for reversible toggling gadgets —
  the model class the gate mask lives in.
- Chip's Challenge Wiki, [Green button](https://chipschallenge.fandom.com/wiki/Green_button)
  and *Toggle wall*: the canonical "press toggles every wall of its kind" convention, and
  the reason group state is shared.
- PuzzleScript, [Rules](https://www.puzzlescript.net/Documentation/rules.html): rule-driven
  state changes (SwitchOn/SwitchOff rewrites) that motivate the switch-press event model.
- Robert Munafo, [Chip's Challenge hints](http://www.mrob.com/pub/vidgames/lynx-chips-challenge.html):
  practical parity/knowledge puzzles built from toggle walls and buttons.

## 14. Implementation checklist

- [x] `entity.rs`: `Switch`, `Gate`, `GateSet`, `MAX_GATE_GROUPS`, glyphs and names.
- [x] `layout.rs`: glyph table, legend, parser.
- [x] `scene.rs`: `group ≤ 3` check.
- [x] `movement.rs`: `gates` parameter, `ClosedGate` block, `Stop::Switch`.
- [x] `episode.rs`: gate state, toggle resolution, `SwitchPressed`, `GateClosed` error, observation `gates`.
- [x] `hash.rs`: kind/attribute codes, state v2 encoding, version bump.
- [x] `analysis/search.rs`: 3-dimensional state, state cap, toggles in transitions and path replay.
- [x] `analysis/mod.rs`: blockers, candidates, assumptions, prose, limit error.
- [x] `analysis/validation.rs`: `player_on_gate`, `player_on_switch`, warnings, `analysis_limit`.
- [x] `render.rs`: switch and gate shapes.
- [x] `catalog.rs`, `errors.rs`, `ops.rs`: publish kinds, constraints, error codes, events.
- [x] `docs/ARCHITECTURE.md`, `docs/PROTOCOL.md`, `README.md`: publish.
- [x] tests, generated schema and docs; `schema --check` and `physics schema --check` pass.
