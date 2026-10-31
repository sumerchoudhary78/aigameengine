//! Terrain- and gate-aware movement resolution.
//!
//! One `move` action resolves to a [`MovePlan`]: a straight displacement that may cross
//! floor, ice and open gates, stop on an object, follow at most one portal hop, and end in
//! a hazard. Episodes and the analysis BFS both call [`plan`], so play and proof cannot
//! drift.
//!
//! The plan is total and terminating: after the first step every iteration advances one cell
//! in the same direction, and the grid is finite. A portal hop ends the displacement, so
//! paired portals cannot ping-pong. Gate passability is evaluated against the `gates` set as
//! of the start of the action; a switch press (which the caller applies after the plan)
//! affects only later actions.

use crate::board::{Board, Occupant};
use crate::entity::{EntityKind, GateSet, KeyColor, KeySet};
use crate::geometry::{Direction, Position};
use crate::ids::EntityId;

/// Why a move is rejected. The episode adds the action context (`from`, `direction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoveBlock {
    /// The first target lies outside the grid.
    OutOfBounds {
        /// The outside cell.
        target: Position,
    },
    /// A wall occupies the first target.
    Wall {
        /// The wall cell.
        target: Position,
        /// The wall entity.
        blocker: EntityId,
    },
    /// The first target is a door whose key is not held.
    LockedDoor {
        /// The door cell.
        target: Position,
        /// The door entity.
        door: EntityId,
        /// The required color.
        required: KeyColor,
    },
    /// The first target is a one-way tile that only allows another entry direction.
    OneWay {
        /// The one-way cell.
        target: Position,
        /// The tile entity.
        tile: EntityId,
        /// The only allowed entry direction.
        allowed: Direction,
    },
    /// The first target is a gate of a group that is closed.
    ClosedGate {
        /// The gate cell.
        target: Position,
        /// The gate entity.
        gate: EntityId,
        /// Its group.
        group: u8,
    },
}

/// The object that ended a displacement, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// No object: the player stopped on terrain.
    None,
    /// An uncollected key.
    Key {
        /// The key entity.
        id: EntityId,
        /// Its color.
        color: KeyColor,
    },
    /// A door entered with its key.
    Door {
        /// The door entity.
        id: EntityId,
        /// Its color.
        color: KeyColor,
    },
    /// An exit.
    Exit {
        /// The exit entity.
        id: EntityId,
    },
    /// A portal; `to` is the partner (equal to the portal cell when unpaired).
    Portal {
        /// The portal entity.
        id: EntityId,
        /// Its pair id.
        pair: u8,
        /// The partner cell.
        to: Position,
    },
    /// A hazard. Entering it fails the episode.
    Hazard {
        /// The hazard entity.
        id: EntityId,
    },
    /// A switch; the caller toggles its group's gates after the plan returns.
    Switch {
        /// The switch entity.
        id: EntityId,
        /// The gate group it toggles.
        group: u8,
    },
}

/// The result of a legal move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MovePlan {
    /// The last cell physically entered, before any portal hop.
    pub last: Position,
    /// The player's position after the action (the partner cell after a portal).
    pub destination: Position,
    /// The object that ended the displacement.
    pub stop: Stop,
}

/// Resolves one move under the terrain and gate rules.
///
/// The player enters `from + direction`, then keeps moving in the same direction while the
/// displacement is sliding. Floor, one-way tiles and open gates are crossed; ice starts a
/// slide; keys, doors, exits, portals, hazards and switches stop it; walls, locked doors,
/// wrong-way one-way tiles, closed gates and the grid edge stop it *before* entry. The
/// first target must be enterable, otherwise the move is rejected and `on_enter` is never
/// called.
///
/// `gates` must be the gate state as of the start of the action; a press applied by the
/// caller afterwards affects only later actions. `removed` treats one entity as absent,
/// which is how verified repairs are evaluated. `on_enter` sees every entered cell in
/// order; the analysis uses it to record the movement footprint without allocating a path
/// vector per move.
pub(crate) fn plan(
    board: &Board,
    from: Position,
    direction: Direction,
    inventory: KeySet,
    gates: GateSet,
    removed: Option<EntityId>,
    mut on_enter: impl FnMut(Position),
) -> Result<MovePlan, MoveBlock> {
    let size = board.size();
    let mut position = from;
    let mut sliding = false;
    let mut entered = 0u32;

    loop {
        let target = position.step(direction);
        if !size.contains(target) {
            if entered == 0 {
                return Err(MoveBlock::OutOfBounds { target });
            }
            break;
        }
        let occupant = board.at(target).filter(|o| Some(o.id) != removed);
        if let Some(Occupant { id, kind }) = occupant {
            match kind {
                EntityKind::Wall => {
                    if entered == 0 {
                        return Err(MoveBlock::Wall {
                            target,
                            blocker: id,
                        });
                    }
                    break;
                }
                EntityKind::Door { color } if !inventory.contains(color) => {
                    if entered == 0 {
                        return Err(MoveBlock::LockedDoor {
                            target,
                            door: id,
                            required: color,
                        });
                    }
                    break;
                }
                EntityKind::OneWay { direction: allowed } if allowed != direction => {
                    if entered == 0 {
                        return Err(MoveBlock::OneWay {
                            target,
                            tile: id,
                            allowed,
                        });
                    }
                    break;
                }
                EntityKind::Gate { group } if !gates.contains(group) => {
                    if entered == 0 {
                        return Err(MoveBlock::ClosedGate {
                            target,
                            gate: id,
                            group,
                        });
                    }
                    break;
                }
                _ => {}
            }
        }

        entered += 1;
        position = target;
        on_enter(target);

        let stop = match occupant {
            Some(Occupant {
                id,
                kind: EntityKind::Key { color },
            }) => Some(Stop::Key { id, color }),
            Some(Occupant {
                id,
                kind: EntityKind::Door { color },
            }) => Some(Stop::Door { id, color }),
            Some(Occupant {
                id,
                kind: EntityKind::Exit,
            }) => Some(Stop::Exit { id }),
            Some(Occupant {
                id,
                kind: EntityKind::Portal { pair },
            }) => {
                let to = board.portal_partner(position).unwrap_or(position);
                Some(Stop::Portal { id, pair, to })
            }
            Some(Occupant {
                id,
                kind: EntityKind::Hazard,
            }) => Some(Stop::Hazard { id }),
            Some(Occupant {
                id,
                kind: EntityKind::Switch { group },
            }) => Some(Stop::Switch { id, group }),
            _ => None,
        };
        if let Some(stop) = stop {
            let destination = match stop {
                Stop::Portal { to, .. } => to,
                _ => position,
            };
            return Ok(MovePlan {
                last: position,
                destination,
                stop,
            });
        }

        if let Some(Occupant {
            kind: EntityKind::Ice,
            ..
        }) = occupant
        {
            sliding = true;
        }
        if !sliding {
            break;
        }
    }

    Ok(MovePlan {
        last: position,
        destination: position,
        stop: Stop::None,
    })
}
