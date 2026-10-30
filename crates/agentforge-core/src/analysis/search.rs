//! Exhaustive breadth-first searches over a [`Board`].
//!
//! Neighbours are expanded in [`Direction::ALL`] order and the frontier is a FIFO queue, so
//! every search is deterministic and returns a shortest path. Gameplay transitions come from
//! [`crate::movement::plan`], the same function episodes use, so terrain cannot make play and
//! analysis disagree.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::board::{Board, Occupant};
use crate::entity::{EntityKind, KeyColor, KeySet};
use crate::geometry::{Direction, Position};
use crate::ids::EntityId;
use crate::movement::{self, Stop};

/// Target of a gameplay search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Goal {
    /// Stand on this cell.
    Cell(Position),
    /// Win: stand on an exit while the win condition holds.
    Objective,
}

/// Result of [`gameplay`].
#[derive(Debug, Clone)]
pub(crate) struct GameplaySearch {
    /// Shortest move sequence reaching the goal, if any.
    pub moves: Option<Vec<Direction>>,
    /// Key colors picked up along `moves`, in pickup order.
    pub keys_on_path: Vec<KeyColor>,
    /// Cells occupied in at least one reachable (cell, keys) state.
    pub reachable_cells: Vec<bool>,
    /// Cells from which a move was attempted, plus every cell a legal move entered. Used to
    /// decide which barriers and ice tiles could matter as repair candidates.
    pub active_cells: Vec<bool>,
    /// Exits reached in a winning state, which ends the episode there.
    pub terminal_exits: BTreeSet<EntityId>,
    /// Key colors held in at least one reachable state.
    pub obtainable: KeySet,
    /// Number of (cell, keys) states expanded.
    pub explored_states: u32,
}

/// Searches (cell, keys-held) states under the gameplay rules: terrain follows
/// [`movement::plan`], walls block, doors need their key, keys are collected on entry and
/// never consumed, hazards are failure edges and are never expanded, and stepping onto an
/// exit while the win condition holds ends the episode, so such states are never expanded.
/// The whole reachable state space is explored (at most 4096 cells x 256 key sets), so a
/// `None` path proves the goal unreachable and the closure fields are complete.
///
/// `collect_all` selects the win condition; `removed` names an entity to treat as absent,
/// which is how repairs are verified.
pub(crate) fn gameplay(
    board: &Board,
    start: Position,
    goal: Goal,
    collect_all: bool,
    removed: Option<EntityId>,
) -> GameplaySearch {
    let size = board.size();
    let cells = size.cell_count();
    let occupant = |index: usize| board.at_index(index).filter(|o| Some(o.id) != removed);

    // Compact key bits: only colors present on the board get a bit.
    let mut present: Vec<KeyColor> = (0..cells)
        .filter_map(|index| match occupant(index) {
            Some(Occupant {
                kind: EntityKind::Key { color },
                ..
            }) => Some(color),
            _ => None,
        })
        .collect();
    present.sort();
    present.dedup();
    let bits = present.len();
    let full_mask = (1usize << bits) - 1;
    let bit_of = |color: KeyColor| present.iter().position(|c| *c == color);

    let mut result = GameplaySearch {
        moves: None,
        keys_on_path: Vec::new(),
        reachable_cells: vec![false; cells],
        active_cells: vec![false; cells],
        terminal_exits: BTreeSet::new(),
        obtainable: KeySet::EMPTY,
        explored_states: 0,
    };
    let Some(start_index) = size.index(start) else {
        return result;
    };
    let mut start_mask = 0usize;
    if let Some(Occupant {
        kind: EntityKind::Key { color },
        ..
    }) = occupant(start_index)
        && let Some(bit) = bit_of(color)
    {
        start_mask |= 1 << bit;
    }

    // An exit wins (and ends the episode) when the win condition holds for `mask`.
    let winning_exit = |index: usize, mask: usize| match occupant(index) {
        Some(Occupant {
            id,
            kind: EntityKind::Exit,
        }) if !collect_all || mask == full_mask => Some(id),
        _ => None,
    };
    let is_goal = |index: usize, mask: usize| match goal {
        Goal::Cell(position) => size.index(position) == Some(index),
        Goal::Objective => winning_exit(index, mask).is_some(),
    };

    const UNVISITED: usize = usize::MAX;
    let mut parent = vec![UNVISITED; cells << bits];
    let mut via = vec![0u8; cells << bits];
    let mut obtainable_mask = 0usize;
    let mut goal_state = None;
    let start_state = (start_index << bits) | start_mask;
    parent[start_state] = start_state;
    result.active_cells[start_index] = true;
    let mut queue = VecDeque::from([start_state]);

    while let Some(state) = queue.pop_front() {
        result.explored_states += 1;
        let (index, mask) = (state >> bits, state & full_mask);
        result.reachable_cells[index] = true;
        obtainable_mask |= mask;
        if goal_state.is_none() && is_goal(index, mask) {
            goal_state = Some(state);
        }
        // Episodes never start on an exit, so the start state is always expandable; any
        // other state on a winning exit is terminal.
        if state != start_state
            && let Some(exit) = winning_exit(index, mask)
        {
            result.terminal_exits.insert(exit);
            continue;
        }
        result.active_cells[index] = true;
        let here = size.position(index);
        let mut held = KeySet::EMPTY;
        for (bit, color) in present.iter().enumerate() {
            if mask & (1 << bit) != 0 {
                held.insert(*color);
            }
        }
        for (direction_index, direction) in Direction::ALL.into_iter().enumerate() {
            // A straight displacement enters at most one cell per grid column/row; 65 is a
            // safe stack bound for a 64-cell dimension.
            let mut entered = [Position::new(0, 0); 65];
            let mut entered_count = 0usize;
            let plan = movement::plan(board, here, direction, held, removed, |position| {
                if let Some(slot) = entered.get_mut(entered_count) {
                    *slot = position;
                    entered_count += 1;
                }
            });
            let Ok(plan) = plan else {
                continue;
            };
            // Hazard moves are not expansions, but their entered cells still matter: removing
            // ice or a barrier on a fatal route can turn an early cell into a stop.
            for position in entered.iter().take(entered_count) {
                if let Some(index) = size.index(*position) {
                    result.active_cells[index] = true;
                }
            }
            if matches!(plan.stop, Stop::Hazard { .. }) {
                continue;
            }
            let mut next_mask = mask;
            if let Stop::Key { color, .. } = plan.stop
                && let Some(bit) = bit_of(color)
            {
                next_mask |= 1 << bit;
            }
            let Some(next_index) = size.index(plan.destination) else {
                continue;
            };
            let next_state = (next_index << bits) | next_mask;
            if parent[next_state] == UNVISITED {
                parent[next_state] = state;
                // Direction index is 0..4, so it always fits in a u8.
                via[next_state] = direction_index as u8;
                queue.push_back(next_state);
            }
        }
    }

    result.obtainable = present
        .iter()
        .enumerate()
        .filter(|(bit, _)| obtainable_mask & (1 << bit) != 0)
        .map(|(_, color)| *color)
        .collect();

    if let Some(mut state) = goal_state {
        let mut moves = Vec::new();
        while parent[state] != state {
            moves.push(Direction::ALL[usize::from(via[state])]);
            state = parent[state];
        }
        moves.reverse();
        result.keys_on_path = keys_along(board, start, &moves, removed);
        result.moves = Some(moves);
    }
    result
}

/// Key colors collected, in order, when walking `moves` from `start`.
///
/// Paths come from the same search, so every step is legal; a failure would be a bug, not an
/// input error. The loop stops rather than guessing if that invariant is ever broken.
fn keys_along(
    board: &Board,
    start: Position,
    moves: &[Direction],
    removed: Option<EntityId>,
) -> Vec<KeyColor> {
    let mut held = KeySet::EMPTY;
    let mut order = Vec::new();
    if let Some(Occupant {
        id,
        kind: EntityKind::Key { color },
    }) = board.at(start)
        && Some(id) != removed
        && !held.contains(color)
    {
        held.insert(color);
        order.push(color);
    }
    let mut position = start;
    for direction in moves {
        let Ok(plan) = movement::plan(board, position, *direction, held, removed, |_| {}) else {
            break;
        };
        position = plan.destination;
        if let Stop::Key { color, .. } = plan.stop
            && !held.contains(color)
        {
            held.insert(color);
            order.push(color);
        }
    }
    order
}

/// Target of a geometric search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GeometricGoal {
    /// Stand on this cell.
    Cell(Position),
    /// Stand on any exit.
    AnyExit,
}

/// Result of [`geometric`].
#[derive(Debug, Clone)]
pub(crate) struct GeometricSearch {
    /// Cells of a shortest path (excluding the start), if the goal is reachable.
    pub path: Option<Vec<Position>>,
    /// Cells expanded before the goal was found (or all reachable cells).
    pub explored_cells: u32,
}

/// Searches cells with walls as the only obstacle: doors, keys, ice, one-way tiles, portals,
/// hazards, win conditions and tick limits are ignored. This is a walls-only diagnostic, so
/// it can disagree with gameplay in either direction (for example, a portal can bridge a
/// wall while a one-way tile can block a doorless corridor).
pub(crate) fn geometric(board: &Board, start: Position, goal: GeometricGoal) -> GeometricSearch {
    let size = board.size();
    let mut result = GeometricSearch {
        path: None,
        explored_cells: 0,
    };
    let Some(start_index) = size.index(start) else {
        return result;
    };
    const UNVISITED: usize = usize::MAX;
    let mut parent = vec![UNVISITED; size.cell_count()];
    parent[start_index] = start_index;
    let mut queue = VecDeque::from([start_index]);
    while let Some(index) = queue.pop_front() {
        result.explored_cells += 1;
        let here = size.position(index);
        let reached = match goal {
            GeometricGoal::Cell(position) => position == here,
            GeometricGoal::AnyExit => matches!(
                board.at_index(index),
                Some(Occupant {
                    kind: EntityKind::Exit,
                    ..
                })
            ),
        };
        if reached {
            let mut path = Vec::new();
            let mut cursor = index;
            while parent[cursor] != cursor {
                path.push(size.position(cursor));
                cursor = parent[cursor];
            }
            path.reverse();
            result.path = Some(path);
            return result;
        }
        for direction in Direction::ALL {
            let Some(next) = size.index(here.step(direction)) else {
                continue;
            };
            let blocked = matches!(
                board.at_index(next),
                Some(Occupant {
                    kind: EntityKind::Wall,
                    ..
                })
            );
            if !blocked && parent[next] == UNVISITED {
                parent[next] = index;
                queue.push_back(next);
            }
        }
    }
    result
}

/// Every entity adjacent to a cell in `active_cells` that could matter as a blocker or as a
/// single-removal repair, keyed by entity id: walls, doors, one-way tiles, hazards, keys and
/// exits. Entities elsewhere are never the target of an attempted move, so removing them
/// cannot change the search.
pub(crate) fn adjacent_entities(
    board: &Board,
    active_cells: &[bool],
) -> BTreeMap<EntityId, (EntityKind, Position)> {
    let size = board.size();
    let mut candidates = BTreeMap::new();
    for (index, _) in active_cells.iter().enumerate().filter(|(_, e)| **e) {
        let here = size.position(index);
        for direction in Direction::ALL {
            let neighbour = here.step(direction);
            if let Some(Occupant { id, kind }) = board.at(neighbour)
                && matches!(
                    kind,
                    EntityKind::Wall
                        | EntityKind::Door { .. }
                        | EntityKind::Key { .. }
                        | EntityKind::Exit
                        | EntityKind::OneWay { .. }
                        | EntityKind::Hazard
                )
            {
                candidates.insert(id, (kind, neighbour));
            }
        }
    }
    candidates
}

/// Whether a one-way tile at `position` stops every active approach.
///
/// The tile is a blocker only when at least one active cell faces it against its allowed
/// direction and no active cell faces it along the allowed direction. An aligned one-way is
/// traversable and is merely a repair candidate.
pub(crate) fn one_way_blocks(
    board: &Board,
    active_cells: &[bool],
    position: Position,
    allowed: Direction,
) -> bool {
    let size = board.size();
    let mut blocked = false;
    let mut enterable = false;
    for entry in Direction::ALL {
        let from = position.step(entry.opposite());
        let Some(index) = size.index(from) else {
            continue;
        };
        if !active_cells.get(index).copied().unwrap_or(false) {
            continue;
        }
        if entry == allowed {
            enterable = true;
        } else {
            blocked = true;
        }
    }
    blocked && !enterable
}
