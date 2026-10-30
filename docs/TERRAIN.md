# Terrain and movement

This note specifies the static terrain mechanics added in this milestone: **paired
portals**, **one-way tiles**, **ice**, and **hazards**. It records the model, the exact
resolution order, the encoding, the analysis proof, the rejected alternatives, and the test
matrix. It is the reference the code matches.

## 1. Scope

In scope:

- a new static cell type is enterable, changes how an action displaces the player, or ends
  the episode;
- terrain is **memoryless**: nothing about it changes during an episode;
- every mechanic is reachable through the existing `move` action, so the tick model,
  replays, state hashes and exhaustive search keep their meaning.

Out of scope:

- switches, doors that stay open, movable blocks, ice that melts, portal cooldowns;
- multiple layers per cell (a cell still holds exactly one entity);
- enemies, timers, randomness, real-time motion.

Those are stateful or multi-layer features and are deferred for the reasons in §3.

## 2. Existing model, restated

A scene is a finite grid `V = [0,w) × [0,h)`, `|V| ≤ 4096`, with a static partial map
`cell : V ⇀ Entity`. An episode state is

```
S = (tick, player ∈ V, inventory ⊆ K, outcome)
```

where `K` is the set of key colors present in the scene, `|K| ≤ 8`. A `move(d)` action is a
partial transition

```
δ : S × {north, east, south, west} ⇀ S
```

One accepted action is one tick. Rejected actions are no-ops. The implementation before this
milestone was the special case where a move enters exactly one cell.

The analysis searches the product space `V × 2^K` exhaustively. With `|V| ≤ 4096` and
`|K| ≤ 8` there are at most `2^20` states, which is why "unreachable" is a proof rather than
a heuristic. **The central constraint of this milestone is to add terrain without adding a
state dimension.**

## 3. Why static terrain

A stateful mechanic adds a component to `S`: a switch bit, a block position, a melted-ice
set. Each component multiplies the search space, and some make the problem much harder:

| mechanic | extra state | effect on exhaustive search |
|---|---|---|
| static terrain | none | `V × 2^K` unchanged |
| switch | 1 bit | × 2 (still tractable) |
| movable block | block position | × `V`, and pushes couple blocks |
| melting ice | visited set | × `2^V` (not tractable) |
| portal cooldown | 1 bit per portal | × `2^p` |

The milestone therefore takes the largest expressive step that preserves the proof:
**terrain is a total, memoryless modification of the transition function**. The formal
statement is that the new transition is still a function of `(cell, inventory)` and the
static scene:

```
δ'(v, k, d) = (v', k')    with no other state read or written
```

Everything below is an instance of that schema.

## 4. Cell classes

Every cell is classified by what happens when the player enters it. "Blocked" means the move
is rejected and nothing changes. "Stop" means the player enters, the effect is applied, and
the displacement ends. "Pass" means the player enters and, if sliding, keeps going.

| entity | enterable | on entry | continues a slide |
|---|---|---|---|
| floor | yes | nothing | yes |
| ice (`~`) | yes | set `sliding = true` | yes (and starts one) |
| one-way (`^ > v <`) | only while moving in its direction | nothing | yes |
| key | yes | collect if not held | no (object) |
| door | only with the matching key | record passage | no (object) |
| exit | yes | win if the condition holds | no (object) |
| portal (`0`–`9`) | yes | teleport to its paired portal | no (object) |
| hazard (`!`) | yes | fail the episode immediately | no (fatal object) |
| wall | no | — | — |
| out of bounds | no | — | — |

Two consequences are worth stating explicitly:

1. **Objects stop slides; terrain does not.** Floor, ice and an aligned one-way are terrain.
   Keys, doors, exits, portals and hazards are objects. This makes a slide's final cell the
   first object it meets, and keeps at most one object interaction per action.
2. **A hazard is entered, not stopped in front of.** If it merely stopped the slide, it would
   be a free wall. Entering it is what makes it a hazard. A slide into a hazard fails the
   episode at that cell.

## 5. Resolution order

The tick pipeline becomes:

1. **Reject** if the episode has ended, or the move is blocked: out of bounds, wall, locked
   door, or a one-way entered against its direction. Rejection changes nothing: no tick, no
   events, no hash.
2. **Advance the tick** by one.
3. **Resolve the displacement** (exactly one of):
   - wait: nothing;
   - forfeit: fail immediately;
   - move: the movement plan of §6, including any slide and at most one portal hop.
4. **Apply the terminal entry effect**, if any:
   - hazard: fail immediately (even if the tick also equals `max_ticks`);
   - otherwise, if the player stands on an exit and the win condition holds: win;
   - otherwise, if the tick reached `max_ticks`: fail.
5. Emit events in that order.

Events for a move are:

```
Moved { from, to }                  to = last cell entered before any portal
[PortalUsed { portal, pair, from, to }]
[KeyCollected { key, color }]
[DoorPassed { door, color }]
[ExitReached { exit }]
[ExitRequirementsUnmet { exit, missing_keys }]
[HazardEntered { hazard, position }]
[Won] | [Failed { reason }]
```

`Moved.to` is the last cell physically entered. For a slide that is the cell where the slide
stopped; for a portal it is the portal cell. The player's final position after a portal is
the event's `PortalUsed.to`, and the observation always reports the final position. There is
no event per slide cell: a slide is one action, and the final position plus the static board
determines the path. The event log is not hashed, so this does not affect replay identity.

## 6. Movement plan

`movement::plan(board, from, direction, inventory, removed, on_enter)` is the single
transition function used by both `Episode::apply` and the analysis BFS. The analysis does not
reimplement movement; a divergence between play and proof is therefore a compile-time or
test-time failure, not a silent behavioural difference.

```
position   := from
sliding    := false
entered    := 0
loop:
    target := position + direction
    if target outside grid:
        if entered == 0: reject OutOfBounds
        else: stop
    occupant := board[target] unless removed
    match occupant:
        wall                  -> if entered == 0: reject Blocked else stop
        door without key      -> if entered == 0: reject DoorLocked else stop
        one-way wrong way     -> if entered == 0: reject OneWay else stop
        otherwise             -> enter target; entered += 1; position := target
    match occupant:
        key                   -> stop (collect)
        door                  -> stop (pass)
        exit                  -> stop (win check later)
        portal                -> stop; if paired: position := partner
        hazard                -> stop (fail later)
        ice                   -> sliding := true
        floor / one-way       -> {}
    if not sliding: stop
```

Properties:

- **Termination.** After the first step, every iteration moves one cell in the same
  direction. The grid is finite, so the loop runs at most `max(w,h) + 1` iterations. There is
  no cycle, so no visited set and no iteration cap is needed.
- **One portal hop.** Entering a portal jumps to its partner and ends the displacement.
  Re-entering the partner is impossible within the same action, so paired portals cannot
  ping-pong. This is a deliberate choice: momentum-preserving portals would need cycle
  detection or a state bit.
- **Slides are straight rays.** The direction never changes, so a slide cannot revisit a
  cell and cannot turn a corner.
- **A slide starts only by entering ice.** Starting an episode on an ice cell does not force
  a slide; the first move decides. This keeps spawn semantics simple.
- **One-way is an entry constraint.** Leaving a one-way cell in any direction is allowed.
  This makes one-way tiles directed edges (`u → v` may exist while `v → u` does not) rather
  than a stateful "locked" flag.
- **Portals are an involution.** `partner(partner(v)) = v`; the map is static and hashed
  with the scene. A portal pair is a quotient edge between two cells, which is why analysis
  must use graph search rather than a symmetric flood fill.
- **Hazards are absorbing failure edges.** The player enters the cell, the state becomes
  failed, and the search never expands from it. The hazard cell is therefore not a reachable
  *state* for analysis purposes, even though it is a reachable *position* at the moment of
  death.

## 7. Encoding

### 7.1 ASCII glyphs

| glyph | meaning |
|---|---|
| `~` | ice |
| `^` `>` `v` `<` | one-way allowing entry when moving north/east/south/west |
| `!` | hazard |
| `0`–`9` | portal with pair id 0–9 |

All other glyphs are unchanged. The legend published by `discover` and observations is
generated from the same table, so it cannot drift.

### 7.2 Entity kinds

```json
{"type":"ice"}
{"type":"one_way","direction":"east"}
{"type":"hazard"}
{"type":"portal","pair":0}
```

`pair` is in `0..=9`; the schema publishes the range and `Scene` rejects anything else.
A scene may hold at most two portals per pair. A pair with one portal is a completeness
error (the scene cannot start), because the enter effect would be undefined. A pair with
three or more is also a completeness error (the partner would be ambiguous). Pairing is
deliberately not a structural `Scene` invariant, so an editor can add the first portal and
the second in separate batches.

### 7.3 Canonical hashing

The scene digest keeps the same layout, with the entity kind byte extended and the
attribute byte generalized:

```
u8 kind      1 player, 2 wall, 3 key, 4 door, 5 exit,
             6 ice, 7 one_way, 8 hazard, 9 portal
u8 attribute 0 none
             key/door: 1 + color index
             one_way:  1 + direction index (north/east/south/west)
             portal:   1 + pair id
```

For every pre-existing entity the attribute byte is exactly the old `color` byte
(`0` or `1 + color index`), and kinds 1–5 are unchanged. The encoding is therefore a strict
extension: **existing scene digests, state hashes and replays keep their values**, so
`STATE_HASH_VERSION` stays 1. The golden-hash tests pin this.

The state hash is unchanged because terrain adds no state. `outcome_code` gains
`4 = failed: hazard`; codes 0–3 keep their meaning.

## 8. Analysis

### 8.1 Gameplay search

The BFS still explores `(cell, keys held)`. For each state and each of the four directions it
calls `movement::plan` and pushes the destination with the inventory update. Slides and
portals are macro edges: they cost one BFS edge and one tick, not one edge per cell. The
state bound and the exhaustiveness proof are unchanged.

`reachable_cells` counts cells that occur as BFS states. Cells crossed only during a slide
are not states, because the player cannot stop or choose there. `active_cells` additionally
records every cell entered by any legal move from a reachable state; it is used only for
repair candidates.

Hazard entries are not pushed: they are failure edges. A cell target that is a hazard is
therefore unreachable under the gameplay rules, which matches "the player cannot stand
there alive".

### 8.2 Geometric search

Geometric reachability remains the walls-only diagnostic: doors are open, and terrain is
ignored. It is a lower bound on movement, not an oracle. In particular a portal can make a
scene solvable while the walls-only graph is disconnected. The report's prose is adjusted
accordingly; the field continues to mean exactly "walls alone".

### 8.3 Blockers

When the gameplay search fails, blockers are:

- walls, doors and wrong-way one-way tiles adjacent to an active cell;
- hazards adjacent to an active cell;
- doors whose key is missing or unobtainable;
- required keys that cannot be obtained (`collect_all_keys_then_exit`);
- exits that end the episode before a cell target.

New `BlockerReason` values: `OneWay` and `Hazard`.

### 8.4 Repairs and the lower bound

A valid single-entity repair removes one entity and leaves a valid scene. Candidates are
every entity whose removal could change the transition function:

- walls, doors, keys, exits, wrong-way one-way tiles and hazards adjacent to an active cell;
- ice tiles that an active move enters (removing one changes where a slide stops);
- every key when all keys are required;
- episode-ending exits for cell targets.

**Portals are not candidates.** A valid portal edit must remove or move both members of the
pair; removing one leaves the scene invalid. A portal repair is therefore a two-entity
change. This is consistent with the lower bound: if no listed single removal works, at least
two changes are needed. The report note states this explicitly.

Candidate completeness argument: a removal can only change a transition that reads the
removed entity. Every such transition is attempted from an active cell (a BFS state or a
cell on a slide path, including a fatal one); a barrier, stopper or hazard is read as the
immediate target of such a cell, and an ice tile is read when entered. Keys and exits are
also stoppers, and all keys are added when the win condition requires them. Therefore every
single removal that could matter is in the list. Each candidate is verified by re-running
the search without it, so the report never guesses.

## 9. Validation

New completeness errors (they prevent `episode.start`):

| code | condition |
|---|---|
| `portal_unpaired` | a pair id occurs exactly once |
| `portal_overpaired` | a pair id occurs more than twice |

No new structural `Scene` errors beyond the `pair ≤ 9` range check. Hazards, dead-end
one-ways and unreachable ice are warnings at most; this milestone does not invent warnings
for them, because a hazard can be an intentional reset and a one-way can be decorative until
the scene is finished.

## 10. Failure and edge cases

- **Rejected move**: out of bounds, wall, locked door, wrong-way one-way. No tick, no events,
  no hash change. `legal_actions` excludes it.
- **Hazard**: accepted move, tick advances, `HazardEntered`, `Failed { reason: Hazard }`.
  Further actions are rejected as `EpisodeFinished`. Hazard failure takes precedence over
  `max_ticks` on the same tick.
- **Portal without a partner**: rejected at `episode.start` by validation. The analysis,
  which may run on an incomplete scene, treats it as an ordinary object that stops movement
  and does not teleport.
- **Three portals with one pair**: rejected at `episode.start`. The analysis treats all of
  them as non-teleporting.
- **Portal into a hazard/key/exit**: impossible while one entity occupies a cell.
- **Spawn on ice**: no forced slide until the first move enters another cell.
- **Slide through the player's spawn marker**: the spawn entity is just floor after the
  episode starts; a slide may cross it.
- **Slide into a portal**: the portal stops the slide, the teleport is the last displacement,
  and the slide does not resume at the partner.
- **Slide into a hazard**: the player enters the hazard and fails.
- **Tick limit**: a slide or portal costs one tick regardless of distance.
- **Dead-end one-way**: a one-way tile can be entered but not left? No: leaving is always
  allowed, so a one-way can create a trap only when surrounded by walls.

## 11. Test matrix

| area | test |
|---|---|
| layout | every terrain glyph parses and renders round-trip; legend contains them |
| hash | existing golden hashes unchanged; new kinds/attributes change the digest; portal pair and one-way direction are hashed |
| episode | slide stops before a wall; slide crosses floor; slide stops on an object; slide into a portal ends there; slide into a hazard fails |
| episode | portal shortcut; unpaired portal prevents start; three-way pair prevents start |
| episode | one-way allows matching entry and rejects the opposite direction with a precise error |
| episode | hazard fails with `Hazard`, no legal actions afterwards |
| replay | a terrain episode round-trips through `Replay`, verifies every hash and re-executes identically |
| analysis | portal makes a walls-disconnected scene solvable |
| analysis | hazard blocker and verified removal; wrong-way one-way blocker and verified removal |
| analysis | ice removal is offered as a repair candidate when it changes reachability |
| analysis | exhaustive flag and "at least two changes" note still hold |
| render | every terrain kind produces an SVG fragment and an id label |
| protocol | generated schema and docs contain the new kinds and legend |

## 12. Rejected alternatives

- **Ice stops at the first non-ice cell.** Simpler (only consecutive ice moves), but it makes
  ice a conveyor rather than a slide and contradicts the "slide until blocked" brief. It also
  makes hazards on the far side of ice unreachable, which weakens the hazard.
- **One tick per slide cell.** Breaks "one action = one tick", inflates `max_ticks` semantics
  and makes the analysis edge cost non-uniform. Rejected: the forced displacement is part of
  the action.
- **Ice as a state bit.** Would require a state dimension and a hash version bump for a
  property that is fully determined by the last displacement. Not needed: the slide is
  resolved eagerly within the action.
- **Portals preserve momentum through the partner.** Can cycle (`A → B → A`) when the pair is
  aligned along the slide axis. Fixing that needs a visited set or a state bit. One hop is
  total and terminating.
- **Portal pairs as edges between adjacent cells.** An edge table doubles the encoding and
  makes pairing validation harder. A cell-pair involution is the same reachability relation
  with one entity per cell.
- **One-way as an edge attribute.** Equivalent for 4-neighbour movement, but it would require
  storing up to four booleans per cell. Storing one allowed entry direction on the cell is
  smaller and matches the glyph.
- **Hazards as walls with a warning.** Then a level could not punish a wrong route; the
  hazard must be entered to matter.
- **Multi-layer cells (terrain under objects).** A larger model change (layers, hashing,
  observation). Deferred; the single-layer model keeps one entity per cell and makes every
  cell's rule total.

## 13. References

- PuzzleScript, [Rule Execution Order](https://www.puzzlescript.net/Documentation/executionorder.html)
  and [Directions](https://www.puzzlescript.net/Documentation/directions.html): pattern
  rewriting, movement direction, and why rule order must be specified globally.
- Bamford et al., [Griddly: A platform for AI research in games](https://arxiv.org/abs/2011.06363):
  GDY description language and deterministic forward models for grid worlds.
- Schaul et al., [Towards a Video Game Description Language](https://drops.dagstuhl.de/entities/document/10.4230/DFU.Vol6.12191.85):
  declarative game description and the separation of rules from simulation.
- Dorbec et al., *Ice sliding games* (HAL `hal-01170310`): the standard "slide until an
  obstacle" model and its computational complexity.
- Bofill et al., [On Grid Graph Reachability and Puzzle Games](https://arxiv.org/abs/2310.01378):
  reachability encodings for grid puzzles.
- Chip's Challenge Wiki, *Ice* and *Sliding tile*: practical conventions for ice, teleports
  and forced movement.

## 14. Implementation checklist

- [x] `entity.rs`: `Ice`, `OneWay`, `Hazard`, `Portal`, glyphs and names.
- [x] `layout.rs`: glyph table, legend, parser, constants.
- [x] `scene.rs`: `pair ≤ 9` check, exhaustive placement match.
- [x] `movement.rs`: `plan`, `MoveBlock`, `Stop`, portal partner lookup.
- [x] `episode.rs`: plan-based resolve, new events, hazard failure.
- [x] `hash.rs`: kind/attribute codes, hazard outcome code, docs.
- [x] `board.rs`: portal partner index.
- [x] `analysis/search.rs`: plan-based BFS, active cells, key reconstruction.
- [x] `analysis/mod.rs`: blockers, candidates, assumptions, prose.
- [x] `analysis/validation.rs`: portal pairing errors.
- [x] `render.rs`: terrain shapes.
- [x] `catalog.rs`, `docs/ARCHITECTURE.md`, `docs/PROTOCOL.md`, `README.md`: publish.
- [x] tests, protocol docs and generated schema; `schema --check` and `physics schema --check` pass.
