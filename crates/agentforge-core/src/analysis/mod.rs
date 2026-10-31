//! Scene analysis: geometric reachability, gameplay solvability, verified repairs and
//! validation.
//!
//! Two questions are always answered separately, because they fail for different reasons:
//!
//! * **Geometric reachability** treats walls as the only obstacle (doors, terrain, keys and
//!   tick limits are ignored). It is a walls-only diagnostic, not an oracle.
//! * **Gameplay reachability** applies the real rules: terrain, doors, keys, gate groups,
//!   the objective and `max_ticks`. The search is exhaustive over
//!   (cell, keys held, gate state) states, so an "unreachable" verdict is a proof for the
//!   current scene, not a heuristic. Gate state adds one bit per gate group; the space is
//!   capped at [`MAX_EXHAUSTIVE_STATES`], beyond which the analysis refuses with an error.
//!
//! When gameplay fails, the analysis lists what blocks the reachable region and verifies
//! single-entity repairs by re-running the search with each candidate removed. Candidates
//! are every entity whose removal could change the search: walls, doors, keys, exits,
//! wrong-way one-way tiles, hazards, gates and switches adjacent to cells the player can
//! move from or slide through, entered ice, required keys (for
//! `collect_all_keys_then_exit`), and exits that end the episode before a cell target.
//! Portals are excluded: a valid portal repair changes two entities, so it is already
//! covered by the two-change lower bound. It never names a culprit it has not verified;
//! when no single removal works it says so, which is then a proof that at least two changes
//! are needed.

mod search;
pub mod validation;

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::Serialize;

use crate::board::{Board, Occupant};
use crate::entity::{EntityKind, KeyColor};
use crate::geometry::{Direction, Position};
use crate::ids::EntityId;
use crate::rules::WinCondition;
use crate::scene::Scene;
use search::{GeometricGoal, Goal};

pub use validation::{Issue, IssueCode, Severity, ValidationReport, validate};

/// Most repair candidates verified per query.
pub const MAX_REPAIR_CANDIDATES: usize = 64;

/// Upper bound on states the exhaustive gameplay search may visit. The worst-case space of
/// 4096 cells x 256 key sets x 16 gate masks is exactly `2^22`, so legal scenes never
/// exceed it unless future mechanics grow the dimensions; over this bound
/// [`reachability`] errors instead of searching — there is no heuristic fallback.
pub const MAX_EXHAUSTIVE_STATES: usize = 1 << 22;

/// Assumptions behind every [`ReachabilityReport`], published verbatim.
pub const ASSUMPTIONS: [&str; 8] = [
    "Movement is 4-directional on the integer grid; one accepted move costs one tick, including any slide or portal hop.",
    "Geometric reachability ignores terrain and gate state: walls block; doors, one-way tiles, ice, portals, gates and hazards count as open; keys, switches, win condition and max_ticks are ignored.",
    "Gameplay reachability applies terrain and gates: ice slides in the direction of travel until a wall, locked door, wrong-way one-way tile, closed gate or the grid edge stops it, or until an object (key, door, exit, portal, hazard or switch) is entered; a portal teleports once to its pair; entering a hazard fails the episode; a door is passable only while holding its color's key; a gate is passable only while its group is open; keys are collected on entry and never consumed; entering a switch toggles its gate group after the move; stepping onto an exit while the win condition holds ends the episode.",
    "The gameplay search is exhaustive over (cell, keys held, gate state) states, bounded at 4194304 states, so 'unreachable' is a proof for the current scene, not a heuristic; over the bound the analysis refuses and reports a limit error.",
    "Starting on a key cell counts as holding that key; starting on a switch toggles nothing, because toggles require an entry.",
    "Repair candidates are every wall, door, key, exit, wrong-way one-way tile, hazard, gate and switch adjacent to a cell the search can still move from or slide through, every entered ice tile, all keys when every key is required, and exits that end the episode before a cell target. Each listed repair was verified by re-running the gameplay search without that entity.",
    "Portals are not repair candidates: a valid portal repair changes both members of a pair, which is a two-entity change already covered by the lower bound.",
    "Multi-entity repairs are not searched; when the candidate list was evaluated exhaustively and none works, at least two changes are needed.",
];

/// Overall answer of a reachability query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Objective target: winnable within `max_ticks`.
    Solvable,
    /// Objective target: not winnable within `max_ticks`.
    Unsolvable,
    /// Cell target: a gameplay path exists.
    Reachable,
    /// Cell target: no gameplay path exists.
    Unreachable,
}

/// What a query asked to reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Target {
    /// The scene's win condition (an exit, possibly with all keys).
    Objective {
        /// The scene's win condition.
        win_condition: WinCondition,
    },
    /// A specific cell.
    Cell {
        /// The cell.
        position: Position,
    },
}

/// Why the gameplay search did not reach the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnreachableReason {
    /// No sequence of legal moves reaches the target.
    NoPath,
    /// The objective is reachable, but only in more moves than `max_ticks` allows.
    ExceedsMaxTicks {
        /// Moves needed by the shortest solution.
        shortest_path_length: u32,
        /// The scene's tick limit.
        max_ticks: u32,
    },
}

/// Walls-only reachability. Doors, keys, ice, one-way tiles, portals, hazards and tick
/// limits are ignored; a portal can therefore make gameplay reachable while this is not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct GeometricResult {
    /// Whether the target is reachable when only walls block.
    pub reachable: bool,
    /// Moves on the shortest walls-only path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_length: Option<u32>,
    /// Cells of that path, excluding the start.
    pub path: Vec<Position>,
    /// Cells expanded by the search.
    pub explored_cells: u32,
}

/// Rules-aware reachability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct GameplayResult {
    /// Whether a legal move sequence reaches the target (ignoring `max_ticks`).
    pub reachable: bool,
    /// Present when the target is not reached within the rules.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreachable_reason: Option<UnreachableReason>,
    /// Moves on the shortest legal path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_length: Option<u32>,
    /// The shortest legal move sequence (directions for `move` actions).
    pub moves: Vec<Direction>,
    /// Keys collected along `moves`, in order.
    pub keys_collected_on_path: Vec<KeyColor>,
    /// The scene's tick limit.
    pub max_ticks: u32,
    /// Whether `path_length <= max_ticks`.
    pub within_max_ticks: bool,
    /// Cells reachable under the gameplay rules from the start.
    pub reachable_cells: u32,
    /// Key colors that can be picked up from the start.
    pub obtainable_keys: Vec<KeyColor>,
    /// (cell, keys, gates) states expanded.
    pub explored_states: u32,
}

/// Why an entity blocks the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlockerReason {
    /// A wall next to a cell the player can still move on from.
    Wall,
    /// A door whose key color does not exist in the scene.
    DoorKeyNotInScene,
    /// A door whose key exists but cannot be picked up from the start.
    DoorKeyUnobtainable,
    /// A key the win condition requires that cannot be picked up from the start.
    RequiredKeyUnobtainable,
    /// An exit that ends the episode (as a win) before the cell target can be reached.
    EpisodeEndingExit,
    /// A one-way tile that faces away from every active approach.
    OneWay,
    /// A hazard; entering it ends the episode as a failure.
    Hazard,
    /// A gate of a closed group that blocked at least one move attempted from a reachable
    /// state. The fields name the group and whether the scene has any switch of it.
    ClosedGate {
        /// The gate's group.
        group: u8,
        /// Whether some switch in the scene toggles this group.
        has_switch: bool,
    },
}

/// An entity that stops the player from reaching the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Blocker {
    /// The blocking entity.
    pub entity: EntityId,
    /// Its kind.
    pub kind: EntityKind,
    /// Its cell.
    pub position: Position,
    /// Why it blocks.
    pub reason: BlockerReason,
}

/// A verified single-entity repair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RepairCandidate {
    /// Entity whose removal makes the target reachable (verified).
    pub remove: EntityId,
    /// Its kind.
    pub kind: EntityKind,
    /// Its cell.
    pub position: Position,
    /// Shortest path length after the removal.
    pub resulting_path_length: u32,
}

/// Single-removal repair search results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RepairReport {
    /// Entities whose single removal could change the result.
    pub candidates: u32,
    /// Candidates actually evaluated (at most `MAX_REPAIR_CANDIDATES`).
    pub evaluated: u32,
    /// Whether every candidate was evaluated.
    pub exhaustive: bool,
    /// Verified repairs, shortest resulting path first, then by entity id.
    pub single_removals: Vec<RepairCandidate>,
    /// What the result does and does not establish.
    pub note: String,
}

/// Full answer to a reachability query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReachabilityReport {
    /// The single field to branch on.
    pub verdict: Verdict,
    /// One-paragraph explanation.
    pub summary: String,
    /// Start cell.
    pub from: Position,
    /// What was searched for.
    pub target: Target,
    /// Model assumptions (see [`ASSUMPTIONS`]).
    pub assumptions: Vec<String>,
    /// Walls-only answer.
    pub geometric: GeometricResult,
    /// Rules-aware answer.
    pub gameplay: GameplayResult,
    /// What stops the player (only when gameplay has no path).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<Blocker>,
    /// Verified single-removal repairs (only when gameplay has no path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repairs: Option<RepairReport>,
}

/// Why a reachability query could not run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnalysisError {
    /// `from` omitted and the scene has no player.
    #[error("the scene has no player; pass `from` explicitly or add a player")]
    NoPlayer,
    /// `to` omitted and the scene has no exit.
    #[error("the scene has no exit; pass `to` explicitly or add an exit")]
    NoExit,
    /// A query position lies outside the grid.
    #[error("`{field}` position {position} is outside the grid")]
    OutOfBounds {
        /// `from` or `to`.
        field: &'static str,
        /// The rejected position.
        position: Position,
    },
    /// `from` is inside a wall.
    #[error("`from` position {position} is inside wall {wall}")]
    StartInsideWall {
        /// The rejected position.
        position: Position,
        /// The wall there.
        wall: EntityId,
    },
    /// The exhaustive gameplay search exceeds its state cap, so no verdict was attempted.
    #[error(
        "the exhaustive search would visit {states} (cell, keys, gates) states, over the cap of {cap}; analysis refused to guess"
    )]
    StateSpaceTooLarge {
        /// Worst-case number of states.
        states: u64,
        /// The cap (`MAX_EXHAUSTIVE_STATES`).
        cap: u64,
    },
}

/// Answers whether `to` (default: the scene objective) can be reached from `from`
/// (default: the player spawn), geometrically and under the gameplay rules.
pub fn reachability(
    scene: &Scene,
    from: Option<Position>,
    to: Option<Position>,
) -> Result<ReachabilityReport, AnalysisError> {
    let board = Board::from_scene(scene);
    let size = scene.size();
    let rules = scene.rules();
    let from = match from {
        Some(position) => position,
        None => scene.player().ok_or(AnalysisError::NoPlayer)?.position,
    };
    if !size.contains(from) {
        return Err(AnalysisError::OutOfBounds {
            field: "from",
            position: from,
        });
    }
    if let Some(Occupant {
        id,
        kind: EntityKind::Wall,
    }) = board.at(from)
    {
        return Err(AnalysisError::StartInsideWall {
            position: from,
            wall: id,
        });
    }
    let (target, goal, geometric_goal) = match to {
        Some(position) if !size.contains(position) => {
            return Err(AnalysisError::OutOfBounds {
                field: "to",
                position,
            });
        }
        Some(position) => (
            Target::Cell { position },
            Goal::Cell(position),
            GeometricGoal::Cell(position),
        ),
        None if scene.exits().next().is_none() => return Err(AnalysisError::NoExit),
        None => (
            Target::Objective {
                win_condition: rules.win_condition,
            },
            Goal::Objective,
            GeometricGoal::AnyExit,
        ),
    };
    let objective = matches!(target, Target::Objective { .. });
    let collect_all = rules.win_condition == WinCondition::CollectAllKeysThenExit;

    let walls_only = search::geometric(&board, from, geometric_goal);
    let geometric = GeometricResult {
        reachable: walls_only.path.is_some(),
        path_length: walls_only.path.as_ref().map(|p| len_u32(p.len())),
        path: walls_only.path.unwrap_or_default(),
        explored_cells: walls_only.explored_cells,
    };

    let play = match search::gameplay(&board, from, goal, collect_all, None) {
        Ok(play) => play,
        Err(limit) => {
            return Err(AnalysisError::StateSpaceTooLarge {
                states: limit.states,
                cap: limit.cap,
            });
        }
    };
    let path_length = play.moves.as_ref().map(|m| len_u32(m.len()));
    let within_max_ticks = path_length.is_some_and(|length| length <= rules.max_ticks);
    let unreachable_reason = match path_length {
        None => Some(UnreachableReason::NoPath),
        Some(length) if objective && !within_max_ticks => {
            Some(UnreachableReason::ExceedsMaxTicks {
                shortest_path_length: length,
                max_ticks: rules.max_ticks,
            })
        }
        Some(_) => None,
    };
    let verdict = match (objective, unreachable_reason.is_none()) {
        (true, true) => Verdict::Solvable,
        (true, false) => Verdict::Unsolvable,
        (false, true) => Verdict::Reachable,
        (false, false) => Verdict::Unreachable,
    };

    let (blockers, repairs) = if play.moves.is_none() {
        let (blockers, candidates) =
            blockers_and_candidates(scene, &board, &play, objective, collect_all);
        let search = RepairSearch {
            board: &board,
            from,
            goal,
            collect_all,
            objective,
            max_ticks: rules.max_ticks,
        };
        (blockers, Some(search.evaluate(&candidates)))
    } else {
        (Vec::new(), None)
    };

    let gameplay = GameplayResult {
        reachable: play.moves.is_some(),
        unreachable_reason,
        path_length,
        moves: play.moves.unwrap_or_default(),
        keys_collected_on_path: play.keys_on_path,
        max_ticks: rules.max_ticks,
        within_max_ticks,
        reachable_cells: len_u32(play.reachable_cells.iter().filter(|r| **r).count()),
        obtainable_keys: play.obtainable.colors(),
        explored_states: play.explored_states,
    };
    let summary = summarize(
        verdict,
        from,
        &target,
        &geometric,
        &gameplay,
        &blockers,
        repairs.as_ref(),
    );
    Ok(ReachabilityReport {
        verdict,
        summary,
        from,
        target,
        assumptions: ASSUMPTIONS.iter().map(|a| (*a).to_owned()).collect(),
        geometric,
        gameplay,
        blockers,
        repairs,
    })
}

type Candidates = BTreeMap<EntityId, (EntityKind, Position)>;

/// Classifies what blocks the target and collects every entity whose single removal could
/// change the gameplay search (see the module documentation for why this set is complete).
fn blockers_and_candidates(
    scene: &Scene,
    board: &Board,
    play: &search::GameplaySearch,
    objective: bool,
    collect_all: bool,
) -> (Vec<Blocker>, Candidates) {
    let key_colors = scene.key_colors();
    // Groups with at least one switch: the summary of a closed-gate blocker names whether
    // the group can be toggled at all.
    let switch_groups: BTreeSet<u8> = scene
        .entities()
        .filter_map(|entity| match entity.kind {
            EntityKind::Switch { group } => Some(group),
            _ => None,
        })
        .collect();
    let mut blockers = Vec::new();
    let mut candidates = search::adjacent_entities(board, &play.active_cells);
    for (id, (kind, position)) in &candidates {
        let reason = match kind {
            EntityKind::Wall => Some(BlockerReason::Wall),
            EntityKind::Door { color } if play.obtainable.contains(*color) => None,
            EntityKind::Door { color } if key_colors.contains(*color) => {
                Some(BlockerReason::DoorKeyUnobtainable)
            }
            EntityKind::Door { .. } => Some(BlockerReason::DoorKeyNotInScene),
            EntityKind::OneWay { direction }
                if search::one_way_blocks(board, &play.active_cells, *position, *direction) =>
            {
                Some(BlockerReason::OneWay)
            }
            EntityKind::Hazard => Some(BlockerReason::Hazard),
            EntityKind::Gate { group } if play.blocked_gates.contains(id) => {
                Some(BlockerReason::ClosedGate {
                    group: *group,
                    has_switch: switch_groups.contains(group),
                })
            }
            _ => None,
        };
        if let Some(reason) = reason {
            blockers.push(Blocker {
                entity: *id,
                kind: *kind,
                position: *position,
                reason,
            });
        }
    }
    // Removing an entered ice tile can change where a slide stops, so it is a repair
    // candidate even though it is not a blocker.
    for (index, active) in play.active_cells.iter().enumerate() {
        if !active {
            continue;
        }
        if let Some(Occupant {
            id,
            kind: EntityKind::Ice,
        }) = board.at_index(index)
        {
            candidates
                .entry(id)
                .or_insert((EntityKind::Ice, board.size().position(index)));
        }
    }
    let mut extra = Vec::new();
    if objective && collect_all {
        for key in scene
            .entities()
            .filter(|e| matches!(e.kind, EntityKind::Key { .. }))
        {
            extra.push((key.id, key.kind, key.position));
            if let EntityKind::Key { color } = key.kind
                && !play.obtainable.contains(color)
            {
                blockers.push(Blocker {
                    entity: key.id,
                    kind: key.kind,
                    position: key.position,
                    reason: BlockerReason::RequiredKeyUnobtainable,
                });
            }
        }
    }
    if !objective {
        for exit in scene
            .exits()
            .filter(|e| play.terminal_exits.contains(&e.id))
        {
            extra.push((exit.id, exit.kind, exit.position));
            blockers.push(Blocker {
                entity: exit.id,
                kind: exit.kind,
                position: exit.position,
                reason: BlockerReason::EpisodeEndingExit,
            });
        }
    }
    for (id, kind, position) in extra {
        candidates.insert(id, (kind, position));
    }
    blockers.sort_by_key(|b| b.entity);
    (blockers, candidates)
}

/// Everything needed to re-run the gameplay search with one entity removed.
struct RepairSearch<'a> {
    board: &'a Board,
    from: Position,
    goal: Goal,
    collect_all: bool,
    objective: bool,
    max_ticks: u32,
}

impl RepairSearch<'_> {
    fn evaluate(&self, candidates: &Candidates) -> RepairReport {
        let mut single_removals: Vec<RepairCandidate> = candidates
            .iter()
            .take(MAX_REPAIR_CANDIDATES)
            .filter_map(|(id, (kind, position))| {
                // Removing an entity can only shrink the state space, so the cap error that
                // `Err` represents cannot occur after the original search already ran.
                let moves = search::gameplay(
                    self.board,
                    self.from,
                    self.goal,
                    self.collect_all,
                    Some(*id),
                )
                .ok()?
                .moves?;
                let length = len_u32(moves.len());
                (!self.objective || length <= self.max_ticks).then_some(RepairCandidate {
                    remove: *id,
                    kind: *kind,
                    position: *position,
                    resulting_path_length: length,
                })
            })
            .collect();
        single_removals.sort_by_key(|c| (c.resulting_path_length, c.remove));
        let evaluated = candidates.len().min(MAX_REPAIR_CANDIDATES);
        let exhaustive = evaluated == candidates.len();
        let note = match (single_removals.is_empty(), exhaustive) {
            (false, _) => "Each listed removal was verified by re-running the gameplay search on the scene without that entity. Portals are not offered: a valid portal repair changes both members of a pair (two entities).".to_owned(),
            (true, true) => format!(
                "None of the {evaluated} entities whose single removal could matter makes the target reachable, so at least two changes are needed (multi-entity repairs are not searched; a portal repair is itself at least two entities)."
            ),
            (true, false) => format!(
                "No verified single removal among the first {evaluated} of {} candidates; the rest were not evaluated.",
                candidates.len()
            ),
        };
        RepairReport {
            candidates: len_u32(candidates.len()),
            evaluated: len_u32(evaluated),
            exhaustive,
            single_removals,
            note,
        }
    }
}

fn summarize(
    verdict: Verdict,
    from: Position,
    target: &Target,
    geometric: &GeometricResult,
    gameplay: &GameplayResult,
    blockers: &[Blocker],
    repairs: Option<&RepairReport>,
) -> String {
    let target_text = match target {
        Target::Objective {
            win_condition: WinCondition::ReachExit,
        } => "an exit".to_owned(),
        Target::Objective {
            win_condition: WinCondition::CollectAllKeysThenExit,
        } => "an exit with every key collected".to_owned(),
        Target::Cell { position } => format!("cell {position}"),
    };
    let label = match verdict {
        Verdict::Solvable => "Solvable",
        Verdict::Unsolvable => "Unsolvable",
        Verdict::Reachable => "Reachable",
        Verdict::Unreachable => "Unreachable",
    };
    match (gameplay.path_length, gameplay.unreachable_reason) {
        (Some(length), None) => format!(
            "{label}: the shortest legal route from {from} to {target_text} takes {length} moves (max_ticks {}).",
            gameplay.max_ticks
        ),
        (Some(length), Some(_)) => format!(
            "{label}: the shortest legal route from {from} to {target_text} takes {length} moves, but max_ticks is {}. Raise max_ticks to at least {length} or shorten the route.",
            gameplay.max_ticks
        ),
        (None, _) => {
            let cause = if geometric.reachable {
                let causes: Vec<String> = blockers
                    .iter()
                    .filter_map(|b| match b.reason {
                        BlockerReason::DoorKeyNotInScene => b.kind.color().map(|color| {
                            format!(
                                "door {} at {} needs a {color} key, and the scene has none",
                                b.entity, b.position
                            )
                        }),
                        BlockerReason::DoorKeyUnobtainable => b.kind.color().map(|color| {
                            format!(
                                "door {} at {} needs the {color} key, which cannot be reached",
                                b.entity, b.position
                            )
                        }),
                        BlockerReason::RequiredKeyUnobtainable => b.kind.color().map(|color| {
                            format!(
                                "the required {color} key {} at {} cannot be reached",
                                b.entity, b.position
                            )
                        }),
                        BlockerReason::EpisodeEndingExit => Some(format!(
                            "stepping onto exit {} at {} wins and ends the episode first",
                            b.entity, b.position
                        )),
                        BlockerReason::OneWay => match b.kind {
                            EntityKind::OneWay { direction } => Some(format!(
                                "one-way tile {} at {} only allows entry moving {direction}",
                                b.entity, b.position
                            )),
                            _ => None,
                        },
                        BlockerReason::Hazard => Some(format!(
                            "hazard {} at {} fails the episode on entry",
                            b.entity, b.position
                        )),
                        BlockerReason::ClosedGate { group, has_switch } => Some(format!(
                            "gate {} at {} is closed (gate group {group}); {}",
                            b.entity,
                            b.position,
                            if has_switch {
                                "press that group's switch to open it"
                            } else {
                                "no switch of that group exists, so the gates of this group can never open"
                            }
                        )),
                        BlockerReason::Wall => None,
                    })
                    .collect();
                if causes.is_empty() {
                    "walls alone do not separate it, but the rules do".to_owned()
                } else {
                    format!(
                        "walls alone do not separate it, but the rules do: {}",
                        causes.join("; ")
                    )
                }
            } else {
                "it is not reachable even when only walls are obstacles, so walls separate it from the start"
                    .to_owned()
            };
            let repair = match repairs.and_then(|r| r.single_removals.first().map(|c| (r, c))) {
                Some((report, best)) => format!(
                    "{} verified single-removal repair(s); shortest: remove {} {} at {} ({} moves afterwards).",
                    report.single_removals.len(),
                    best.kind,
                    best.remove,
                    best.position,
                    best.resulting_path_length
                ),
                None => repairs.map_or_else(String::new, |r| r.note.clone()),
            };
            format!("{label}: no legal route from {from} to {target_text}; {cause}. {repair}")
        }
    }
}

/// Lengths here are bounded by the cell count (at most 4096), so they always fit.
fn len_u32(length: usize) -> u32 {
    u32::try_from(length).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;
    use crate::rules::Rules;
    use crate::scene::SceneEdit;

    fn scene(rows: &[&str], max_ticks: u32, win_condition: WinCondition) -> Scene {
        let rows: Vec<String> = rows.iter().map(|row| (*row).to_owned()).collect();
        let parsed = layout::parse(&rows).expect("valid layout");
        let edits: Vec<SceneEdit> = parsed
            .cells
            .into_iter()
            .map(|(position, kind)| SceneEdit::CreateEntity {
                kind,
                position,
                label: None,
            })
            .collect();
        let rules = Rules {
            max_ticks,
            win_condition,
        };
        Scene::new(parsed.size, rules)
            .expect("valid")
            .apply_edits(&edits)
            .expect("valid")
            .0
    }

    #[test]
    fn single_plug_is_the_only_verified_repair() {
        // Double wall with one plugged gap at (3, 2): only removing that wall reconnects.
        let scene = scene(
            &["#######", "#@.##.#", "#..#..#", "#.##.E#", "#######"],
            50,
            WinCondition::ReachExit,
        );
        let report = reachability(&scene, None, None).expect("query runs");
        assert_eq!(report.verdict, Verdict::Unsolvable);
        assert!(!report.geometric.reachable, "walls are the cause");
        let repairs = report.repairs.expect("repairs computed");
        assert!(repairs.exhaustive);
        let removals: Vec<Position> = repairs.single_removals.iter().map(|c| c.position).collect();
        assert_eq!(removals, vec![Position::new(3, 2)]);
        let fixed = scene
            .apply_edits(&[SceneEdit::RemoveEntity {
                id: repairs.single_removals[0].remove,
            }])
            .expect("valid")
            .0;
        let after = reachability(&fixed, None, None).expect("query runs");
        assert_eq!(after.verdict, Verdict::Solvable);
        assert_eq!(
            after.gameplay.path_length,
            Some(repairs.single_removals[0].resulting_path_length)
        );
    }

    #[test]
    fn no_blocker_is_invented_when_two_removals_are_needed() {
        // Two separate wall layers: removing any one wall cannot connect start and exit.
        let scene = scene(
            &["#######", "#@#.#E#", "#######"],
            50,
            WinCondition::ReachExit,
        );
        let report = reachability(&scene, None, None).expect("query runs");
        assert_eq!(report.verdict, Verdict::Unsolvable);
        let repairs = report.repairs.expect("repairs computed");
        assert!(repairs.single_removals.is_empty());
        assert!(repairs.exhaustive);
        assert!(repairs.note.contains("at least two changes"));
        assert!(!report.blockers.is_empty(), "frontier is still reported");
    }

    #[test]
    fn locked_door_is_distinguished_from_walls() {
        let scene = scene(&["@RE"], 50, WinCondition::ReachExit);
        let report = reachability(&scene, None, None).expect("query runs");
        assert!(
            report.geometric.reachable,
            "doors count as open geometrically"
        );
        assert!(!report.gameplay.reachable, "but the red key does not exist");
        assert_eq!(report.blockers[0].reason, BlockerReason::DoorKeyNotInScene);
        assert!(
            report
                .summary
                .contains("needs a red key, and the scene has none"),
            "{}",
            report.summary
        );
    }

    #[test]
    fn keys_open_doors_in_the_gameplay_search() {
        let scene = scene(&["r@R.E"], 50, WinCondition::ReachExit);
        let report = reachability(&scene, None, None).expect("query runs");
        assert_eq!(report.verdict, Verdict::Solvable);
        assert_eq!(report.geometric.path_length, Some(3), "doors count as open");
        assert_eq!(
            report.gameplay.path_length,
            Some(5),
            "detour for the red key"
        );
        assert_eq!(report.gameplay.keys_collected_on_path, vec![KeyColor::Red]);
    }

    #[test]
    fn a_key_behind_its_own_door_is_unobtainable() {
        // The blue key at (1, 5) lies behind the blue door at (1, 3).
        let scene = scene(
            &[
                "#####", "#@.r#", "#.###", "#B..#", "#.#E#", "#b###", "#####",
            ],
            50,
            WinCondition::CollectAllKeysThenExit,
        );
        let report = reachability(&scene, None, None).expect("query runs");
        assert_eq!(report.verdict, Verdict::Unsolvable);
        assert_eq!(report.gameplay.obtainable_keys, vec![KeyColor::Red]);
        assert!(
            report
                .blockers
                .iter()
                .any(|b| b.reason == BlockerReason::DoorKeyUnobtainable)
        );
    }

    /// Regression: the search used to walk through exits that end the episode, and then
    /// "verified" a repair whose route could never be played.
    #[test]
    fn routes_cannot_continue_past_an_exit_that_ends_the_episode() {
        let sealed = scene(&["@E#."], 50, WinCondition::ReachExit);
        let report = reachability(&sealed, None, Some(Position::new(3, 0))).expect("runs");
        assert_eq!(report.verdict, Verdict::Unreachable);
        let repairs = report.repairs.expect("computed");
        assert!(
            repairs.single_removals.is_empty(),
            "removing only the wall or only the exit is not enough: {:?}",
            repairs.single_removals
        );
        assert!(repairs.exhaustive && repairs.note.contains("at least two changes"));
        assert!(
            report
                .blockers
                .iter()
                .any(|b| b.reason == BlockerReason::EpisodeEndingExit)
        );

        let open = scene(&["@E."], 50, WinCondition::ReachExit);
        let report = reachability(&open, None, Some(Position::new(2, 0))).expect("runs");
        assert_eq!(report.verdict, Verdict::Unreachable);
        let removals = report.repairs.expect("computed").single_removals;
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0].kind, EntityKind::Exit);
        assert_eq!(removals[0].resulting_path_length, 2);
        assert!(
            report.summary.contains("ends the episode"),
            "{}",
            report.summary
        );
    }

    /// Regression: a key behind a winning exit was reported as obtainable.
    #[test]
    fn a_key_behind_a_winning_exit_is_unobtainable() {
        let report = validate(&scene(&["@Er"], 50, WinCondition::ReachExit));
        assert!(report.playable);
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.code == IssueCode::KeyUnobtainable)
        );
    }

    /// Regression: an unreachable required key was blamed on (nonexistent) locked doors and
    /// its removal, a valid single repair, was not considered.
    #[test]
    fn an_unreachable_required_key_is_named_and_its_removal_verified() {
        let walled = scene(&["@E##b"], 50, WinCondition::CollectAllKeysThenExit);
        let report = reachability(&walled, None, None).expect("runs");
        assert_eq!(report.verdict, Verdict::Unsolvable);
        assert!(
            report.geometric.reachable,
            "an exit is reachable; the rules are the problem"
        );
        assert!(
            report.summary.contains("required blue key"),
            "{}",
            report.summary
        );
        assert!(!report.summary.contains("door"), "{}", report.summary);
        let removals = report.repairs.expect("computed").single_removals;
        assert_eq!(removals.len(), 1);
        assert_eq!(
            removals[0].kind,
            EntityKind::Key {
                color: KeyColor::Blue
            }
        );
        assert_eq!(removals[0].resulting_path_length, 1);
    }

    #[test]
    fn exceeding_max_ticks_is_reported_without_repairs() {
        let scene = scene(&["@....E"], 3, WinCondition::ReachExit);
        let report = reachability(&scene, None, None).expect("query runs");
        assert_eq!(report.verdict, Verdict::Unsolvable);
        assert_eq!(
            report.gameplay.unreachable_reason,
            Some(UnreachableReason::ExceedsMaxTicks {
                shortest_path_length: 5,
                max_ticks: 3
            })
        );
        assert!(report.repairs.is_none());
    }

    #[test]
    fn collect_all_route_picks_up_every_key() {
        let scene = scene(&["r@.Eb"], 50, WinCondition::CollectAllKeysThenExit);
        let report = reachability(&scene, None, None).expect("query runs");
        assert_eq!(report.verdict, Verdict::Solvable);
        assert_eq!(report.gameplay.keys_collected_on_path.len(), 2);
        // West for red, then east through the exit (requirements unmet) to blue, then back.
        assert_eq!(report.gameplay.path_length, Some(6));
    }

    #[test]
    fn cell_queries_and_input_errors() {
        let scene = scene(&["@.#.E"], 50, WinCondition::ReachExit);
        let report = reachability(&scene, None, Some(Position::new(1, 0))).expect("runs");
        assert_eq!(report.verdict, Verdict::Reachable);
        assert_eq!(report.gameplay.moves, vec![Direction::East]);
        assert_eq!(
            reachability(&scene, Some(Position::new(2, 0)), None),
            Err(AnalysisError::StartInsideWall {
                position: Position::new(2, 0),
                wall: EntityId::new(2)
            })
        );
        assert!(matches!(
            reachability(&scene, None, Some(Position::new(9, 9))),
            Err(AnalysisError::OutOfBounds { field: "to", .. })
        ));
    }

    #[test]
    fn validation_reports_errors_then_warnings() {
        let empty = scene(&["..."], 10, WinCondition::ReachExit);
        let report = validate(&empty);
        assert!(!report.valid && !report.playable);
        let codes: Vec<IssueCode> = report.issues.iter().map(|i| i.code).collect();
        assert_eq!(
            codes,
            vec![IssueCode::MissingPlayer, IssueCode::MissingExit]
        );

        let blocked = scene(&["@#E", "...", "G.."], 10, WinCondition::ReachExit);
        let report = validate(&blocked);
        assert!(report.valid);
        assert!(report.playable, "route goes around the wall");
        assert!(
            report
                .issues
                .iter()
                .any(|i| i.code == IssueCode::DoorWithoutKey)
        );
    }

    #[test]
    fn a_portal_makes_a_walls_disconnected_scene_solvable() {
        let scene = scene(&["@.0#E", "####0"], 50, WinCondition::ReachExit);
        let report = reachability(&scene, None, None).expect("query runs");
        assert!(
            !report.geometric.reachable,
            "the wall at (3, 0) separates start from exit without the portal"
        );
        assert_eq!(report.verdict, Verdict::Solvable);
        assert_eq!(report.gameplay.path_length, Some(3));
        assert_eq!(
            report.gameplay.moves,
            vec![Direction::East, Direction::East, Direction::North]
        );
    }

    #[test]
    fn a_hazard_is_a_blocker_and_its_removal_is_verified() {
        let scene = scene(&["@.!E"], 50, WinCondition::ReachExit);
        let report = reachability(&scene, None, None).expect("query runs");
        assert!(
            report.geometric.reachable,
            "hazards count as open geometrically"
        );
        assert_eq!(report.verdict, Verdict::Unsolvable);
        assert!(
            report
                .blockers
                .iter()
                .any(|b| b.reason == BlockerReason::Hazard)
        );
        assert!(
            report.summary.contains("fails the episode on entry"),
            "{}",
            report.summary
        );
        let repairs = report.repairs.expect("repairs computed");
        let repairs: Vec<_> = repairs
            .single_removals
            .iter()
            .filter(|r| r.kind == EntityKind::Hazard)
            .collect();
        assert_eq!(repairs.len(), 1, "removing the hazard opens the corridor");
        assert_eq!(repairs[0].resulting_path_length, 3);
    }

    #[test]
    fn a_wrong_way_one_way_is_a_blocker_and_its_removal_is_verified() {
        let scene = scene(&["@<E"], 50, WinCondition::ReachExit);
        let report = reachability(&scene, None, None).expect("query runs");
        assert!(
            report.geometric.reachable,
            "one-way tiles count as open geometrically"
        );
        assert_eq!(report.verdict, Verdict::Unsolvable);
        assert!(
            report
                .blockers
                .iter()
                .any(|b| b.reason == BlockerReason::OneWay)
        );
        assert!(
            report.summary.contains("only allows entry moving west"),
            "{}",
            report.summary
        );
        let repairs = report.repairs.expect("repairs computed");
        assert!(repairs.exhaustive);
        let removals: Vec<_> = repairs
            .single_removals
            .iter()
            .filter(|r| {
                r.kind
                    == EntityKind::OneWay {
                        direction: Direction::West,
                    }
            })
            .collect();
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0].resulting_path_length, 2);
    }

    #[test]
    fn entered_ice_is_a_verified_repair_candidate() {
        // The slide crosses (1, 0), so the player cannot stop there to drop into the exit
        // at (1, 1). Removing the ice stops the east move at (1, 0) instead.
        let scene = scene(&["@~.#", "#E##", "####"], 50, WinCondition::ReachExit);
        let report = reachability(&scene, None, None).expect("query runs");
        assert!(report.geometric.reachable, "walls alone allow the exit");
        assert_eq!(report.verdict, Verdict::Unsolvable);
        let repairs = report.repairs.expect("repairs computed");
        assert!(repairs.exhaustive);
        let ice: Vec<_> = repairs
            .single_removals
            .iter()
            .filter(|r| r.kind == EntityKind::Ice)
            .collect();
        assert_eq!(ice.len(), 1, "the entered ice tile is a candidate");
        assert_eq!(ice[0].position, Position::new(1, 0));
        assert_eq!(ice[0].resulting_path_length, 2);
    }

    /// Regression: ice entered only on a fatal slide is still a repair candidate, because
    /// removing it can turn the ice cell into an ordinary stopping cell.
    #[test]
    fn ice_entered_on_a_fatal_slide_is_a_verified_repair() {
        let scene = scene(&["@~!#E"], 50, WinCondition::ReachExit);
        let report = reachability(&scene, None, Some(Position::new(1, 0))).expect("query runs");
        assert_eq!(report.verdict, Verdict::Unreachable);
        let repairs = report.repairs.expect("repairs computed");
        assert!(repairs.exhaustive);
        let ice: Vec<_> = repairs
            .single_removals
            .iter()
            .filter(|r| r.kind == EntityKind::Ice)
            .collect();
        assert_eq!(ice.len(), 1);
        assert_eq!(ice[0].resulting_path_length, 1);
    }

    #[test]
    fn a_gate_without_a_switch_blocks_with_evidence_and_its_removal_verifies() {
        // The gate of group 0 can never open; walls plus the closed gate separate start
        // and exit. The blocker is named only because a move actually failed against it.
        let scene = scene(&["#@)E"], 20, WinCondition::ReachExit);
        let report = reachability(&scene, None, None).expect("query runs");
        assert_eq!(report.verdict, Verdict::Unsolvable);
        assert!(
            report.geometric.reachable,
            "gates count as open geometrically"
        );
        let blocker = report
            .blockers
            .iter()
            .find(|b| matches!(b.kind, EntityKind::Gate { group: 0 }))
            .expect("the closed gate is a blocker");
        assert!(
            matches!(
                blocker.reason,
                BlockerReason::ClosedGate {
                    group: 0,
                    has_switch: false
                }
            ),
            "{blocker:?}"
        );
        let repairs = report.repairs.expect("repairs computed");
        let removals: Vec<EntityKind> = repairs.single_removals.iter().map(|c| c.kind).collect();
        assert_eq!(removals, vec![EntityKind::Gate { group: 0 }]);
    }

    #[test]
    fn a_toggled_gate_connects_a_walled_region() {
        // The gate of group 0 opens by pressing the switch; the search follows the toggles.
        let scene = scene(&["#@()E"], 20, WinCondition::ReachExit);
        let report = reachability(&scene, None, None).expect("query runs");
        assert_eq!(report.verdict, Verdict::Solvable);
        assert_eq!(
            report.gameplay.path_length,
            Some(3),
            "press, walk through the gate, step onto the exit"
        );
        assert_eq!(
            report.gameplay.moves,
            vec![Direction::East, Direction::East, Direction::East]
        );
        assert!(report.blockers.is_empty());
        assert!(report.repairs.is_none(), "solvable: no repair search ran");
        // Removing the gate alone keeps it solvable; removing the switch alone does not
        // (the closed gate can then never open).
        let id_of = |scene: &Scene, kind: EntityKind| {
            scene
                .entities()
                .find(|e| e.kind == kind)
                .map(|e| e.id)
                .expect("entity present")
        };
        let gate_removed = scene
            .apply_edits(&[SceneEdit::RemoveEntity {
                id: id_of(&scene, EntityKind::Gate { group: 0 }),
            }])
            .expect("valid")
            .0;
        assert_eq!(
            reachability(&gate_removed, None, None)
                .expect("query runs")
                .verdict,
            Verdict::Solvable
        );
        let switch_removed = scene
            .apply_edits(&[SceneEdit::RemoveEntity {
                id: id_of(&scene, EntityKind::Switch { group: 0 }),
            }])
            .expect("valid")
            .0;
        assert_eq!(
            reachability(&switch_removed, None, None)
                .expect("query runs")
                .verdict,
            Verdict::Unsolvable
        );
    }

    #[test]
    fn validation_names_unpaired_groups_and_the_state_cap() {
        // Gate with no switch: its gates can never open.
        let without_switch = scene(&["#@)E"], 20, WinCondition::ReachExit);
        let report = validate(&without_switch);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == IssueCode::GateWithoutSwitch),
            "{report:?}"
        );
        // Switch with no gate: pressing it is a no-op; standing beside one is fine, since
        // toggles require an entry.
        let without_gate = scene(&["#@.(E"], 20, WinCondition::ReachExit);
        let report = validate(&without_gate);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == IssueCode::SwitchWithoutGate),
            "{report:?}"
        );
        assert!(
            !report
                .issues
                .iter()
                .any(|issue| issue.code == IssueCode::PlayerOnSwitch),
            "the player stands next to the switch, not on it"
        );
    }

    #[test]
    fn an_over_cap_state_space_refuses_instead_of_guessing() {
        // 64x64 with 8 key colors and all 4 gate groups: 4096 x 2^8 x 2^4 = 2^24 states,
        // over the 2^22 cap. The analysis refuses rather than guessing.
        let mut first = String::with_capacity(64);
        first.push('@');
        first.push_str("rgbypocw");
        first.push_str("([{=");
        first.push_str(")]}=");
        for _ in 17..63 {
            first.push('.');
        }
        first.push('E');
        let rows: Vec<String> = std::iter::once(first)
            .chain(std::iter::repeat_n(".".repeat(64), 63))
            .collect();
        let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
        let scene = scene(&refs, 100, WinCondition::ReachExit);
        let error = reachability(&scene, None, None).expect_err("over the cap");
        let AnalysisError::StateSpaceTooLarge { states, cap } = error else {
            panic!("unexpected error: {error:?}");
        };
        assert_eq!(states, 1 << 24);
        assert_eq!(cap, MAX_EXHAUSTIVE_STATES as u64);
        let report = validate(&scene);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == IssueCode::AnalysisLimit),
            "{report:?}"
        );
    }
}
