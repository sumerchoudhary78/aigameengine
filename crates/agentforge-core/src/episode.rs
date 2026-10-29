//! Episodes: deterministic play-throughs of a frozen scene.
//!
//! An episode copies the scene it starts from, so authoring edits made later never affect
//! it (editor / play-mode separation). Time advances only through accepted actions: each
//! accepted [`Action`] advances exactly one tick, and a rejected action changes nothing.
//!
//! Resolution order within a tick:
//! 1. Reject the action if the episode has ended, or if a move would leave the grid,
//!    enter a wall, or enter a door whose key the player does not hold.
//! 2. Advance the tick by one.
//! 3. Apply the action: `move` relocates the player and collects an uncollected key on the
//!    target cell; `wait` does nothing; `forfeit` ends the episode as failed immediately.
//! 4. If the player stands on an exit and the win condition holds, the episode is won.
//! 5. Otherwise, if the tick has reached `max_ticks`, the episode fails.
//!
//! Events of a tick are recorded in that same order.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::analysis::validation::{self, Issue};
use crate::board::{Board, Occupant};
use crate::entity::{EntityKind, KeyColor, KeySet};
use crate::geometry::{Direction, GridSize, Position};
use crate::hash::{self, StateHash};
use crate::ids::EntityId;
use crate::layout::{self, LegendEntry};
use crate::replay::{REPLAY_FORMAT, REPLAY_FORMAT_VERSION, Replay, ReplayStep};
use crate::rules::WinCondition;
use crate::scene::Scene;

/// A player action. Serialized with a `type` tag, e.g. `{"type":"move","direction":"east"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Move one cell.
    Move {
        /// Where to move.
        direction: Direction,
    },
    /// Let one tick pass without moving.
    Wait,
    /// Give up; the episode fails immediately.
    Forfeit,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Move { direction } => write!(f, "move {direction}"),
            Action::Wait => f.write_str("wait"),
            Action::Forfeit => f.write_str("forfeit"),
        }
    }
}

/// Why an episode failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    /// `max_ticks` elapsed without a win.
    MaxTicksReached,
    /// The player forfeited.
    Forfeited,
}

/// State of an episode's result. Serialized with a `status` tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    /// Still running.
    InProgress,
    /// The win condition was met.
    Won {
        /// Tick at which the episode was won.
        tick: u32,
    },
    /// The episode ended without a win.
    Failed {
        /// Tick at which the episode failed.
        tick: u32,
        /// Why it failed.
        reason: FailureReason,
    },
}

impl Outcome {
    /// Whether the episode has ended.
    pub const fn is_finished(self) -> bool {
        !matches!(self, Outcome::InProgress)
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::InProgress => f.write_str("in progress"),
            Outcome::Won { tick } => write!(f, "won at tick {tick}"),
            Outcome::Failed {
                tick,
                reason: FailureReason::MaxTicksReached,
            } => write!(f, "failed at tick {tick}: max_ticks reached"),
            Outcome::Failed {
                tick,
                reason: FailureReason::Forfeited,
            } => write!(f, "failed at tick {tick}: forfeited"),
        }
    }
}

/// Everything about an episode that changes from tick to tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpisodeState {
    /// Ticks elapsed (0 before the first action).
    pub tick: u32,
    /// Current player position.
    pub player: Position,
    /// Key colors held. Keys are never consumed.
    pub inventory: KeySet,
    /// Result so far.
    pub outcome: Outcome,
}

/// A recorded consequence of an accepted action (or of the episode start, at tick 0).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Event {
    /// Tick the event belongs to.
    pub tick: u32,
    /// What happened.
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Kinds of [`Event`]. Serialized with a `type` tag next to `tick`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    /// The episode started with the player at its spawn point.
    Started {
        /// Spawn position.
        player: Position,
    },
    /// The player moved.
    Moved {
        /// Previous cell.
        from: Position,
        /// New cell.
        to: Position,
        /// Direction moved.
        direction: Direction,
    },
    /// The player waited.
    Waited,
    /// The player picked up a key.
    KeyCollected {
        /// The key entity.
        key: EntityId,
        /// Its color.
        color: KeyColor,
    },
    /// The player entered a door cell while holding its key.
    DoorPassed {
        /// The door entity.
        door: EntityId,
        /// Its color.
        color: KeyColor,
    },
    /// The player stepped onto an exit.
    ExitReached {
        /// The exit entity.
        exit: EntityId,
    },
    /// The player stepped onto an exit without satisfying the win condition.
    ExitRequirementsUnmet {
        /// The exit entity.
        exit: EntityId,
        /// Keys still required.
        missing_keys: Vec<KeyColor>,
    },
    /// The episode was won.
    Won,
    /// The episode failed.
    Failed {
        /// Why.
        reason: FailureReason,
    },
}

/// Why an action was rejected. A rejected action never changes the episode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionError {
    /// The episode already ended.
    #[error("the episode has already ended ({outcome})")]
    EpisodeFinished {
        /// The final outcome.
        outcome: Outcome,
    },
    /// The move would leave the grid.
    #[error("moving {direction} from {from} would leave the grid")]
    OutOfBounds {
        /// Player position.
        from: Position,
        /// Requested direction.
        direction: Direction,
        /// The cell outside the grid.
        target: Position,
    },
    /// A wall occupies the target cell.
    #[error("moving {direction} from {from} is blocked by wall {blocker} at {target}")]
    Blocked {
        /// Player position.
        from: Position,
        /// Requested direction.
        direction: Direction,
        /// The blocked cell.
        target: Position,
        /// The wall entity.
        blocker: EntityId,
    },
    /// A door occupies the target cell and the player lacks its key.
    #[error("door {door} at {target} is locked; it requires the {required} key")]
    DoorLocked {
        /// Player position.
        from: Position,
        /// Requested direction.
        direction: Direction,
        /// The door cell.
        target: Position,
        /// The door entity.
        door: EntityId,
        /// The key color it requires.
        required: KeyColor,
    },
}

/// The scene is not complete enough to play (see [`validation::completeness_errors`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("scene is not playable: {}", .issues.iter().map(|i| i.message.as_str()).collect::<Vec<_>>().join("; "))]
pub struct StartError {
    /// The blocking validation errors.
    pub issues: Vec<Issue>,
}

/// One accepted action and the state hash after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepRecord {
    /// Tick reached by the action.
    pub tick: u32,
    /// The accepted action.
    pub action: Action,
    /// State hash after the action.
    pub hash: StateHash,
}

/// Result of an accepted action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    /// Tick reached.
    pub tick: u32,
    /// Events of this tick, in resolution order.
    pub events: Vec<Event>,
    /// Outcome after the tick.
    pub outcome: Outcome,
    /// State hash after the tick.
    pub hash: StateHash,
}

/// What the win condition currently requires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ObjectiveStatus {
    /// The scene's win condition.
    pub win_condition: WinCondition,
    /// Plain-language statement of the objective.
    pub description: String,
    /// Keys that must be held to win (empty for `reach_exit`).
    pub keys_required: Vec<KeyColor>,
    /// Required keys not yet held.
    pub keys_missing: Vec<KeyColor>,
    /// Whether the episode has been won.
    pub complete: bool,
}

/// An entity as a player sees it: no authoring labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct VisibleEntity {
    /// Stable id (useful to interpret diagnostics and events).
    pub id: EntityId,
    /// What it is.
    pub kind: EntityKind,
    /// Where it is.
    pub position: Position,
}

/// Everything a player may observe. The game is fully observable, so this is the complete
/// world state minus authoring metadata (labels, revisions, allocator state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Observation {
    /// Ticks elapsed.
    pub tick: u32,
    /// Tick limit of the scene.
    pub max_ticks: u32,
    /// Ticks left before `max_ticks` is reached.
    pub ticks_remaining: u32,
    /// Result so far.
    pub outcome: Outcome,
    /// What winning requires right now.
    pub objective: ObjectiveStatus,
    /// Grid dimensions.
    pub grid: GridSize,
    /// Current player position.
    pub player: Position,
    /// Key colors held.
    pub inventory: Vec<KeyColor>,
    /// Entities on the board, excluding the player's spawn marker and collected keys.
    pub entities: Vec<VisibleEntity>,
    /// The board as text rows (see `legend`).
    pub ascii: Vec<String>,
    /// Glyph legend for `ascii`.
    pub legend: Vec<LegendEntry>,
    /// Actions that would be accepted now, in canonical order.
    pub legal_actions: Vec<Action>,
}

/// One play-through of a frozen scene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    scene: Scene,
    board: Board,
    required_keys: KeySet,
    scene_digest: StateHash,
    initial_hash: StateHash,
    current_hash: StateHash,
    state: EpisodeState,
    steps: Vec<StepRecord>,
    events: Vec<Event>,
}

impl Episode {
    /// Starts an episode on a copy of `scene`. Fails if the scene lacks a player or exit.
    pub fn start(scene: &Scene) -> Result<Self, StartError> {
        let issues = validation::completeness_errors(scene);
        let spawn = match (issues.is_empty(), scene.player()) {
            (true, Some(player)) => player.position,
            _ => return Err(StartError { issues }),
        };
        let required_keys = match scene.rules().win_condition {
            WinCondition::ReachExit => KeySet::EMPTY,
            WinCondition::CollectAllKeysThenExit => scene.key_colors(),
        };
        let scene_digest = hash::scene_digest(scene);
        let state = EpisodeState {
            tick: 0,
            player: spawn,
            inventory: KeySet::EMPTY,
            outcome: Outcome::InProgress,
        };
        let initial_hash = hash::state_hash(&scene_digest, &state);
        Ok(Self {
            scene: scene.clone(),
            board: Board::from_scene(scene),
            required_keys,
            scene_digest,
            initial_hash,
            current_hash: initial_hash,
            state,
            steps: Vec::new(),
            events: vec![Event {
                tick: 0,
                kind: EventKind::Started { player: spawn },
            }],
        })
    }

    /// The frozen scene this episode plays.
    pub const fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Current runtime state.
    pub const fn state(&self) -> &EpisodeState {
        &self.state
    }

    /// Ticks elapsed.
    pub const fn tick(&self) -> u32 {
        self.state.tick
    }

    /// Result so far.
    pub const fn outcome(&self) -> Outcome {
        self.state.outcome
    }

    /// Digest of the frozen scene.
    pub const fn scene_digest(&self) -> StateHash {
        self.scene_digest
    }

    /// State hash at tick 0.
    pub const fn initial_hash(&self) -> StateHash {
        self.initial_hash
    }

    /// State hash now.
    pub const fn current_hash(&self) -> StateHash {
        self.current_hash
    }

    /// Accepted actions so far, with the hash after each.
    pub fn steps(&self) -> &[StepRecord] {
        &self.steps
    }

    /// All events so far, oldest first.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Checks whether `action` would be accepted, without changing anything.
    pub fn check(&self, action: Action) -> Result<(), ActionError> {
        self.resolve(action).map(|_| ())
    }

    /// Applies `action`, advancing exactly one tick. On error nothing changes.
    pub fn apply(&mut self, action: Action) -> Result<StepResult, ActionError> {
        // All fallible work happens in `resolve`; the commit below cannot fail, so an
        // episode is never left half-updated.
        let (next, events) = self.resolve(action)?;
        let hash = hash::state_hash(&self.scene_digest, &next);
        self.state = next;
        self.current_hash = hash;
        self.events.extend(events.iter().cloned());
        self.steps.push(StepRecord {
            tick: next.tick,
            action,
            hash,
        });
        Ok(StepResult {
            tick: next.tick,
            events,
            outcome: next.outcome,
            hash,
        })
    }

    /// Actions that would be accepted now: legal moves in canonical direction order, then
    /// `wait` and `forfeit`. Empty once the episode has ended.
    pub fn legal_actions(&self) -> Vec<Action> {
        if self.state.outcome.is_finished() {
            return Vec::new();
        }
        Direction::ALL
            .into_iter()
            .filter(|direction| {
                self.check_move(self.state.player, *direction, self.state.inventory)
                    .is_ok()
            })
            .map(|direction| Action::Move { direction })
            .chain([Action::Wait, Action::Forfeit])
            .collect()
    }

    /// The objective and how far the player is from meeting it.
    pub fn objective(&self) -> ObjectiveStatus {
        let win_condition = self.scene.rules().win_condition;
        let keys_required = self.required_keys.colors();
        let description = match win_condition {
            WinCondition::ReachExit => "Step onto any exit (E).".to_owned(),
            WinCondition::CollectAllKeysThenExit if keys_required.is_empty() => {
                "Collect all keys (this scene has none), then step onto an exit (E).".to_owned()
            }
            WinCondition::CollectAllKeysThenExit => format!(
                "Collect all keys ({}), then step onto an exit (E).",
                keys_required
                    .iter()
                    .map(|color| color.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        ObjectiveStatus {
            win_condition,
            description,
            keys_missing: self.required_keys.difference(self.state.inventory).colors(),
            keys_required,
            complete: matches!(self.state.outcome, Outcome::Won { .. }),
        }
    }

    /// The player's view of the current state.
    pub fn observation(&self) -> Observation {
        let max_ticks = self.scene.rules().max_ticks;
        let entities: Vec<VisibleEntity> = self
            .scene
            .entities()
            .filter(|entity| self.is_visible(entity.kind))
            .map(|entity| VisibleEntity {
                id: entity.id,
                kind: entity.kind,
                position: entity.position,
            })
            .collect();
        let glyphs = entities
            .iter()
            .map(|entity| (entity.position, entity.kind.glyph()))
            .chain([(self.state.player, layout::PLAYER)]);
        Observation {
            tick: self.state.tick,
            max_ticks,
            ticks_remaining: max_ticks.saturating_sub(self.state.tick),
            outcome: self.state.outcome,
            objective: self.objective(),
            grid: self.scene.size(),
            player: self.state.player,
            inventory: self.state.inventory.colors(),
            ascii: layout::render(self.scene.size(), glyphs),
            entities,
            legend: layout::legend(),
            legal_actions: self.legal_actions(),
        }
    }

    /// The replay of this episode so far.
    pub fn to_replay(&self, scene_name: Option<String>) -> Replay {
        Replay {
            format: REPLAY_FORMAT.to_owned(),
            format_version: REPLAY_FORMAT_VERSION,
            state_hash_version: hash::STATE_HASH_VERSION,
            engine_version: crate::ENGINE_VERSION.to_owned(),
            scene_name,
            scene: self.scene.to_document(),
            scene_digest: self.scene_digest,
            initial_hash: self.initial_hash,
            steps: self
                .steps
                .iter()
                .map(|step| ReplayStep {
                    tick: step.tick,
                    action: step.action,
                    hash: step.hash,
                })
                .collect(),
            outcome: self.state.outcome,
            final_hash: self.current_hash,
        }
    }

    /// Player spawn markers are not shown (the live player is drawn instead) and keys
    /// disappear once collected.
    fn is_visible(&self, kind: EntityKind) -> bool {
        match kind {
            EntityKind::Player => false,
            EntityKind::Key { color } => !self.state.inventory.contains(color),
            EntityKind::Wall | EntityKind::Door { .. } | EntityKind::Exit => true,
        }
    }

    fn check_move(
        &self,
        from: Position,
        direction: Direction,
        inventory: KeySet,
    ) -> Result<(Position, Option<Occupant>), ActionError> {
        let target = from.step(direction);
        if !self.board.size().contains(target) {
            return Err(ActionError::OutOfBounds {
                from,
                direction,
                target,
            });
        }
        let occupant = self.board.at(target);
        match occupant {
            Some(Occupant {
                id,
                kind: EntityKind::Wall,
            }) => Err(ActionError::Blocked {
                from,
                direction,
                target,
                blocker: id,
            }),
            Some(Occupant {
                id,
                kind: EntityKind::Door { color },
            }) if !inventory.contains(color) => Err(ActionError::DoorLocked {
                from,
                direction,
                target,
                door: id,
                required: color,
            }),
            _ => Ok((target, occupant)),
        }
    }

    /// Computes the state and events that `action` would produce, without committing.
    fn resolve(&self, action: Action) -> Result<(EpisodeState, Vec<Event>), ActionError> {
        let current = self.state;
        if current.outcome.is_finished() {
            return Err(ActionError::EpisodeFinished {
                outcome: current.outcome,
            });
        }
        // While in progress, tick < max_ticks <= 10_000, so this cannot overflow.
        let tick = current.tick + 1;
        let mut next = EpisodeState { tick, ..current };
        let mut events = Vec::new();
        let mut push = |kind| events.push(Event { tick, kind });

        match action {
            Action::Wait => push(EventKind::Waited),
            Action::Forfeit => {
                let reason = FailureReason::Forfeited;
                next.outcome = Outcome::Failed { tick, reason };
                push(EventKind::Failed { reason });
                return Ok((next, events));
            }
            Action::Move { direction } => {
                let (target, occupant) =
                    self.check_move(current.player, direction, current.inventory)?;
                next.player = target;
                push(EventKind::Moved {
                    from: current.player,
                    to: target,
                    direction,
                });
                match occupant {
                    Some(Occupant {
                        id,
                        kind: EntityKind::Key { color },
                    }) if !next.inventory.contains(color) => {
                        next.inventory.insert(color);
                        push(EventKind::KeyCollected { key: id, color });
                    }
                    Some(Occupant {
                        id,
                        kind: EntityKind::Door { color },
                    }) => push(EventKind::DoorPassed { door: id, color }),
                    Some(Occupant {
                        id,
                        kind: EntityKind::Exit,
                    }) => {
                        push(EventKind::ExitReached { exit: id });
                        let missing = self.required_keys.difference(next.inventory);
                        if !missing.is_empty() {
                            push(EventKind::ExitRequirementsUnmet {
                                exit: id,
                                missing_keys: missing.colors(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }

        let on_exit = matches!(
            self.board.at(next.player),
            Some(Occupant {
                kind: EntityKind::Exit,
                ..
            })
        );
        if on_exit && next.inventory.is_superset(self.required_keys) {
            next.outcome = Outcome::Won { tick };
            push(EventKind::Won);
        } else if tick >= self.scene.rules().max_ticks {
            let reason = FailureReason::MaxTicksReached;
            next.outcome = Outcome::Failed { tick, reason };
            push(EventKind::Failed { reason });
        }
        Ok((next, events))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout;
    use crate::rules::Rules;
    use crate::scene::SceneEdit;

    pub(crate) fn scene_from(rows: &[&str], rules: Rules) -> Scene {
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
        Scene::new(parsed.size, rules)
            .expect("valid size")
            .apply_edits(&edits)
            .expect("valid edits")
            .0
    }

    fn mv(direction: Direction) -> Action {
        Action::Move { direction }
    }

    const EAST: Direction = Direction::East;

    fn rules(max_ticks: u32, win_condition: WinCondition) -> Rules {
        Rules {
            max_ticks,
            win_condition,
        }
    }

    #[test]
    fn reaching_the_exit_wins_and_further_actions_are_rejected() {
        let scene = scene_from(&["@.E"], rules(10, WinCondition::ReachExit));
        let mut episode = Episode::start(&scene).expect("playable");
        assert_eq!(
            episode.apply(mv(EAST)).expect("legal").outcome,
            Outcome::InProgress
        );
        let step = episode.apply(mv(EAST)).expect("legal");
        assert_eq!(step.outcome, Outcome::Won { tick: 2 });
        assert!(matches!(
            step.events.last(),
            Some(Event {
                kind: EventKind::Won,
                ..
            })
        ));
        assert!(episode.legal_actions().is_empty());
        assert!(matches!(
            episode.apply(Action::Wait),
            Err(ActionError::EpisodeFinished { .. })
        ));
        assert_eq!(episode.tick(), 2);
    }

    #[test]
    fn walls_and_bounds_reject_moves_without_side_effects() {
        let scene = scene_from(&["@#E"], rules(10, WinCondition::ReachExit));
        let mut episode = Episode::start(&scene).expect("playable");
        let before = (
            episode.state,
            episode.current_hash(),
            episode.events().len(),
        );
        let blocked = episode.apply(mv(EAST)).expect_err("wall");
        assert!(
            matches!(blocked, ActionError::Blocked { blocker, .. } if blocker == EntityId::new(2))
        );
        assert!(matches!(
            episode.apply(mv(Direction::North)),
            Err(ActionError::OutOfBounds { .. })
        ));
        assert_eq!(
            (
                episode.state,
                episode.current_hash(),
                episode.events().len()
            ),
            before
        );
        assert_eq!(episode.legal_actions(), vec![Action::Wait, Action::Forfeit]);
    }

    #[test]
    fn doors_need_their_key_and_keys_are_not_consumed() {
        let scene = scene_from(&["r@RR.E"], rules(20, WinCondition::ReachExit));
        let mut episode = Episode::start(&scene).expect("playable");
        let locked = episode.apply(mv(EAST)).expect_err("door is locked");
        assert!(matches!(
            locked,
            ActionError::DoorLocked {
                required: KeyColor::Red,
                ..
            }
        ));

        let collect = episode.apply(mv(Direction::West)).expect("key cell");
        assert!(collect.events.iter().any(|e| matches!(
            e.kind,
            EventKind::KeyCollected {
                color: KeyColor::Red,
                ..
            }
        )));
        for _ in 0..4 {
            episode
                .apply(mv(EAST))
                .expect("both doors open with one key");
        }
        assert_eq!(
            episode.apply(mv(EAST)).expect("exit").outcome,
            Outcome::Won { tick: 6 }
        );
    }

    #[test]
    fn collect_all_keys_condition_blocks_early_exit() {
        let scene = scene_from(&["@Eb"], rules(20, WinCondition::CollectAllKeysThenExit));
        let mut episode = Episode::start(&scene).expect("playable");
        let early = episode.apply(mv(EAST)).expect("exit cell is enterable");
        assert_eq!(early.outcome, Outcome::InProgress);
        assert!(early.events.iter().any(|e| matches!(
            &e.kind,
            EventKind::ExitRequirementsUnmet { missing_keys, .. } if missing_keys == &vec![KeyColor::Blue]
        )));
        episode.apply(mv(EAST)).expect("collect blue");
        let win = episode.apply(mv(Direction::West)).expect("back to exit");
        assert_eq!(win.outcome, Outcome::Won { tick: 3 });
    }

    #[test]
    fn max_ticks_fails_the_episode_but_a_win_on_the_last_tick_counts() {
        let scene = scene_from(&["@.E"], rules(2, WinCondition::ReachExit));
        let mut won = Episode::start(&scene).expect("playable");
        won.apply(mv(EAST)).expect("legal");
        assert_eq!(
            won.apply(mv(EAST)).expect("legal").outcome,
            Outcome::Won { tick: 2 }
        );

        let mut lost = Episode::start(&scene).expect("playable");
        lost.apply(Action::Wait).expect("legal");
        let step = lost.apply(Action::Wait).expect("legal");
        assert_eq!(
            step.outcome,
            Outcome::Failed {
                tick: 2,
                reason: FailureReason::MaxTicksReached
            }
        );
    }

    #[test]
    fn forfeit_ends_the_episode_immediately() {
        let scene = scene_from(&["@.E"], rules(5, WinCondition::ReachExit));
        let mut episode = Episode::start(&scene).expect("playable");
        let step = episode.apply(Action::Forfeit).expect("legal");
        assert_eq!(
            step.outcome,
            Outcome::Failed {
                tick: 1,
                reason: FailureReason::Forfeited
            }
        );
    }

    #[test]
    fn identical_inputs_produce_identical_hash_sequences() {
        let scene = scene_from(
            &["#####", "#@.r#", "#.#R#", "#..E#", "#####"],
            rules(30, WinCondition::ReachExit),
        );
        let script = [
            mv(EAST),
            mv(EAST),
            mv(Direction::South),
            mv(Direction::South),
        ];
        let run = || {
            let mut episode = Episode::start(&scene).expect("playable");
            let mut hashes = vec![episode.current_hash()];
            for action in script {
                hashes.push(episode.apply(action).expect("legal").hash);
            }
            (hashes, episode.outcome())
        };
        let (first, outcome) = run();
        assert_eq!(run(), (first.clone(), outcome));
        assert_eq!(outcome, Outcome::Won { tick: 4 });
        let unique: std::collections::BTreeSet<_> = first.iter().collect();
        assert_eq!(unique.len(), first.len(), "every tick has a distinct state");
    }

    #[test]
    fn observation_hides_labels_spawn_marker_and_collected_keys() {
        let scene = scene_from(&["@rE"], rules(10, WinCondition::CollectAllKeysThenExit));
        let mut episode = Episode::start(&scene).expect("playable");
        episode.apply(mv(EAST)).expect("collect");
        let observation = episode.observation();
        assert_eq!(observation.ascii, vec![".@E".to_owned()]);
        assert_eq!(observation.inventory, vec![KeyColor::Red]);
        assert!(
            observation
                .entities
                .iter()
                .all(|e| e.kind == EntityKind::Exit)
        );
        let json = serde_json::to_string(&observation).expect("serializable");
        assert!(!json.contains("label"));
        assert!(observation.objective.keys_missing.is_empty());
    }

    #[test]
    fn incomplete_scenes_cannot_start() {
        let scene = scene_from(&["..E"], rules(10, WinCondition::ReachExit));
        let error = Episode::start(&scene).expect_err("no player");
        assert_eq!(error.issues.len(), 1);
    }
}
