//! Scene analysis: geometric reachability, gameplay solvability, verified repairs and
//! validation.
//!
//! Two questions are always answered separately, because they fail for different reasons:
//!
//! * **Geometric reachability** treats walls as the only obstacle (doors count as open,
//!   keys and tick limits are ignored). If this fails, walls are the cause.
//! * **Gameplay reachability** applies the real rules: doors need their key, keys are
//!   collected on entry and never consumed, and the objective enforces the win condition
//!   and `max_ticks`. The search is exhaustive over (cell, keys held) states, so an
//!   "unreachable" verdict is a proof for the current scene, not a heuristic.
//!
//! When gameplay fails, the analysis lists the blockers bordering the reachable region and
//! verifies single-entity repairs by re-running the search with each blocker removed. It
//! never names a culprit it has not verified; if no single removal works it says so.

mod search;
pub mod validation;

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

/// Most frontier blockers evaluated as single-removal repairs per query.
pub const MAX_REPAIR_CANDIDATES: usize = 64;

/// Assumptions behind every [`ReachabilityReport`], published verbatim.
pub const ASSUMPTIONS: [&str; 6] = [
    "Movement is 4-directional on the integer grid; one move costs one tick.",
    "Geometric reachability: walls block; doors count as open; keys, win condition and max_ticks are ignored.",
    "Gameplay reachability: walls block; a door is passable only while holding its color's key; keys are collected on entry and never consumed; the objective target also requires the win condition and a path length of at most max_ticks.",
    "The gameplay search is exhaustive over (cell, keys held) states, so 'unreachable' is a proof for the current scene, not a heuristic.",
    "Starting on a key cell counts as holding that key.",
    "Repairs consider removing exactly one wall or never-openable door adjacent to the gameplay-reachable region; each listed repair was verified by re-running the gameplay search. Multi-entity repairs are not searched.",
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

/// Walls-only reachability.
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
    /// (cell, keys) states expanded.
    pub explored_states: u32,
}

/// Why an entity blocks the reachable region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlockerReason {
    /// A wall.
    Wall,
    /// A door whose key color does not exist in the scene.
    DoorKeyNotInScene,
    /// A door whose key exists but cannot be picked up from the start.
    DoorKeyUnobtainable,
}

/// An entity bordering the gameplay-reachable region that stops further progress.
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
    /// Blockers bordering the reachable region.
    pub frontier_size: u32,
    /// Blockers actually evaluated (at most `MAX_REPAIR_CANDIDATES`).
    pub evaluated: u32,
    /// Whether every frontier blocker was evaluated.
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
    /// Blockers bordering the reachable region (only when gameplay has no path).
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
            Goal::Objective {
                collect_all_keys: rules.win_condition == WinCondition::CollectAllKeysThenExit,
            },
            GeometricGoal::AnyExit,
        ),
    };
    let objective = matches!(target, Target::Objective { .. });

    let walls_only = search::geometric(&board, from, geometric_goal);
    let geometric = GeometricResult {
        reachable: walls_only.path.is_some(),
        path_length: walls_only.path.as_ref().map(|p| len_u32(p.len())),
        path: walls_only.path.unwrap_or_default(),
        explored_cells: walls_only.explored_cells,
    };

    let play = search::gameplay(&board, from, goal, None);
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
        let frontier = search::frontier(&board, &play.reachable_cells, play.obtainable);
        let key_colors = scene.key_colors();
        let blockers = frontier
            .iter()
            .map(|(id, (kind, position))| Blocker {
                entity: *id,
                kind: *kind,
                position: *position,
                reason: match kind {
                    EntityKind::Door { color } if key_colors.contains(*color) => {
                        BlockerReason::DoorKeyUnobtainable
                    }
                    EntityKind::Door { .. } => BlockerReason::DoorKeyNotInScene,
                    _ => BlockerReason::Wall,
                },
            })
            .collect();
        let repairs = evaluate_repairs(&board, from, goal, objective, rules.max_ticks, &frontier);
        (blockers, Some(repairs))
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

fn evaluate_repairs(
    board: &Board,
    from: Position,
    goal: Goal,
    objective: bool,
    max_ticks: u32,
    frontier: &std::collections::BTreeMap<EntityId, (EntityKind, Position)>,
) -> RepairReport {
    let mut single_removals: Vec<RepairCandidate> = frontier
        .iter()
        .take(MAX_REPAIR_CANDIDATES)
        .filter_map(|(id, (kind, position))| {
            let moves = search::gameplay(board, from, goal, Some(*id)).moves?;
            let length = len_u32(moves.len());
            (!objective || length <= max_ticks).then_some(RepairCandidate {
                remove: *id,
                kind: *kind,
                position: *position,
                resulting_path_length: length,
            })
        })
        .collect();
    single_removals.sort_by_key(|c| (c.resulting_path_length, c.remove));
    let evaluated = frontier.len().min(MAX_REPAIR_CANDIDATES);
    let exhaustive = evaluated == frontier.len();
    let note = match (single_removals.is_empty(), exhaustive) {
        (false, _) => "Each listed removal was verified by re-running the gameplay search on the scene without that entity.".to_owned(),
        (true, true) => "No single removal of a frontier wall or door makes the target reachable; at least two changes are needed (multi-entity repairs are not searched).".to_owned(),
        (true, false) => format!(
            "No verified single removal among the first {evaluated} of {} frontier blockers; the rest were not evaluated.",
            frontier.len()
        ),
    };
    RepairReport {
        frontier_size: len_u32(frontier.len()),
        evaluated: len_u32(evaluated),
        exhaustive,
        single_removals,
        note,
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
                let doors: Vec<String> = blockers
                    .iter()
                    .filter_map(|b| {
                        b.kind.color().map(|color| {
                            format!("door {} at {} needs the {color} key", b.entity, b.position)
                        })
                    })
                    .collect();
                if doors.is_empty() {
                    "the target is geometrically reachable, so locked doors are the obstacle"
                        .to_owned()
                } else {
                    format!(
                        "the target is geometrically reachable, so locked doors are the obstacle ({}), and those keys cannot be obtained",
                        doors.join("; ")
                    )
                }
            } else {
                "it is not geometrically reachable either, so walls separate it from the start"
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
        assert!(report.summary.contains("locked doors"));
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
}
