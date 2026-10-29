//! Exhaustive breadth-first searches over a [`Board`].
//!
//! Neighbours are expanded in [`Direction::ALL`] order and the frontier is a FIFO queue, so
//! every search is deterministic and returns a shortest path.

use std::collections::{BTreeMap, VecDeque};

use crate::board::{Board, Occupant};
use crate::entity::{EntityKind, KeyColor, KeySet};
use crate::geometry::{Direction, Position};
use crate::ids::EntityId;

/// Target of a gameplay search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Goal {
    /// Stand on this cell.
    Cell(Position),
    /// Stand on an exit; with `collect_all_keys`, while holding every key on the board.
    Objective { collect_all_keys: bool },
}

/// Result of [`gameplay`].
#[derive(Debug, Clone)]
pub(crate) struct GameplaySearch {
    /// Shortest move sequence reaching the goal, if any.
    pub moves: Option<Vec<Direction>>,
    /// Key colors picked up along `moves`, in pickup order.
    pub keys_on_path: Vec<KeyColor>,
    /// Cells reachable in at least one (cell, keys) state.
    pub reachable_cells: Vec<bool>,
    /// Key colors held in at least one reachable state.
    pub obtainable: KeySet,
    /// Number of (cell, keys) states expanded.
    pub explored_states: u32,
}

/// Searches (cell, keys-held) states under the gameplay rules: walls block, doors need
/// their key, keys are collected on entry and never consumed. The whole reachable state
/// space is explored (it is at most 4096 cells x 256 key sets), so a `None` path proves
/// the goal unreachable, and `reachable_cells`/`obtainable` describe the full closure.
///
/// `removed` names an entity to treat as absent, which is how repairs are verified.
pub(crate) fn gameplay(
    board: &Board,
    start: Position,
    goal: Goal,
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

    let is_goal = |index: usize, mask: usize| match goal {
        Goal::Cell(position) => size.index(position) == Some(index),
        Goal::Objective { collect_all_keys } => {
            matches!(
                occupant(index),
                Some(Occupant {
                    kind: EntityKind::Exit,
                    ..
                })
            ) && (!collect_all_keys || mask == full_mask)
        }
    };

    const UNVISITED: usize = usize::MAX;
    let mut parent = vec![UNVISITED; cells << bits];
    let mut via = vec![0u8; cells << bits];
    let mut obtainable_mask = 0usize;
    let mut goal_state = None;
    let start_state = (start_index << bits) | start_mask;
    parent[start_state] = start_state;
    let mut queue = VecDeque::from([start_state]);

    while let Some(state) = queue.pop_front() {
        result.explored_states += 1;
        let (index, mask) = (state >> bits, state & full_mask);
        result.reachable_cells[index] = true;
        obtainable_mask |= mask;
        if goal_state.is_none() && is_goal(index, mask) {
            goal_state = Some(state);
        }
        let here = size.position(index);
        for (direction_index, direction) in Direction::ALL.into_iter().enumerate() {
            let Some(next_index) = size.index(here.step(direction)) else {
                continue;
            };
            let mut next_mask = mask;
            match occupant(next_index).map(|o| o.kind) {
                Some(EntityKind::Wall) => continue,
                Some(EntityKind::Door { color }) => match bit_of(color) {
                    Some(bit) if mask & (1 << bit) != 0 => {}
                    _ => continue,
                },
                Some(EntityKind::Key { color }) => {
                    if let Some(bit) = bit_of(color) {
                        next_mask |= 1 << bit;
                    }
                }
                Some(EntityKind::Player | EntityKind::Exit) | None => {}
            }
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
fn keys_along(
    board: &Board,
    start: Position,
    moves: &[Direction],
    removed: Option<EntityId>,
) -> Vec<KeyColor> {
    let mut held = KeySet::EMPTY;
    let mut order = Vec::new();
    let mut position = start;
    for cell in std::iter::once(start).chain(moves.iter().map(|direction| {
        position = position.step(*direction);
        position
    })) {
        if let Some(Occupant {
            id,
            kind: EntityKind::Key { color },
        }) = board.at(cell)
            && Some(id) != removed
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

/// Searches cells with walls as the only obstacle: doors count as open and keys, win
/// conditions and tick limits are ignored.
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

/// Walls and never-openable doors adjacent to the gameplay closure, keyed by entity id.
/// Removing anything not in this set cannot enlarge the closure, so single-removal repairs
/// only need to consider these entities.
pub(crate) fn frontier(
    board: &Board,
    reachable_cells: &[bool],
    obtainable: KeySet,
) -> BTreeMap<EntityId, (EntityKind, Position)> {
    let size = board.size();
    let mut blockers = BTreeMap::new();
    for (index, _) in reachable_cells.iter().enumerate().filter(|(_, r)| **r) {
        let here = size.position(index);
        for direction in Direction::ALL {
            let neighbour = here.step(direction);
            if let Some(Occupant { id, kind }) = board.at(neighbour) {
                let blocks = match kind {
                    EntityKind::Wall => true,
                    EntityKind::Door { color } => !obtainable.contains(color),
                    EntityKind::Player | EntityKind::Key { .. } | EntityKind::Exit => false,
                };
                if blocks {
                    blockers.insert(id, (kind, neighbour));
                }
            }
        }
    }
    blockers
}
