//! One authoritative execution path: validate, apply one input, advance one fixed tick.
use crate::{
    hash,
    scene::{PhysicsScene, PhysicsSceneDocument, Vec2},
    world::{RawState, World},
};
use agentforge_core::{hash::StateHash, ids::EntityId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Wait,
    Impulse { impulse: Vec2 },
    Forfeit,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    InProgress,
    Won,
    TickLimit,
    LeftBounds,
    Forfeited,
    NumericalFailure,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BodyState {
    pub id: EntityId,
    pub position: Vec2,
    /// Unit complex rotation: cosine and sine (avoids platform atan2 in canonical state).
    pub rotation: [f64; 2],
    pub linear_velocity: Vec2,
    pub angular_velocity: f64,
    pub sleeping: bool,
}
impl From<(EntityId, RawState)> for BodyState {
    fn from((id, s): (EntityId, RawState)) -> Self {
        Self {
            id,
            position: s.position,
            rotation: [s.rotation.0, s.rotation.1],
            linear_velocity: s.linear_velocity,
            angular_velocity: s.angular_velocity,
            sleeping: s.sleeping,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    ContactStarted { a: EntityId, b: EntityId },
    ContactEnded { a: EntityId, b: EntityId },
    ZoneEntered { zone: EntityId, body: EntityId },
    ZoneLeft { zone: EntityId, body: EntityId },
    ImpulseApplied { body: EntityId, impulse: Vec2 },
    Finished { outcome: Outcome },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    pub tick: u32,
    pub action: Action,
    pub hash: StateHash,
}
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Observation {
    pub tick: u32,
    pub outcome: Outcome,
    pub bodies: Vec<BodyState>,
    pub settings: crate::scene::PhysicsSettings,
    /// Fully observable scene; authoring labels removed.
    pub scene: PhysicsSceneDocument,
    pub impulses_used: u32,
    pub can_impulse: bool,
    pub state_hash: StateHash,
    pub contacts: Vec<(EntityId, EntityId)>,
    pub overlaps: Vec<(EntityId, EntityId)>,
}
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TickResult {
    pub tick: u32,
    pub outcome: Outcome,
    pub state_hash: StateHash,
    pub events: Vec<Event>,
}
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EpisodeError {
    #[error("scene needs a control body and objective zone")]
    NotPlayable,
    #[error("episode is finished: {0:?}")]
    Finished(Outcome),
    #[error("impulse must be finite and have magnitude at most {max}")]
    InvalidImpulse { max: f64 },
    #[error("the episode's impulse budget has been exhausted")]
    ImpulseBudget,
}
/// Rapier caches are retained while playing. Checkpoints persist initial scene and all
/// accepted inputs; restoring re-executes them, rebuilding solver caches exactly.
pub struct Episode {
    scene: PhysicsScene,
    world: World,
    tick: u32,
    outcome: Outcome,
    impulses_used: u32,
    contacts: BTreeSet<(EntityId, EntityId)>,
    overlaps: BTreeSet<(EntityId, EntityId)>,
    steps: Vec<Step>,
    last_events: Vec<Event>,
}
impl Episode {
    pub fn start(scene: &PhysicsScene) -> Result<Self, EpisodeError> {
        if scene.settings().control.is_none() || scene.settings().objective.is_none() {
            return Err(EpisodeError::NotPlayable);
        }
        Ok(Self {
            scene: scene.clone(),
            world: World::build(scene),
            tick: 0,
            outcome: Outcome::InProgress,
            impulses_used: 0,
            contacts: BTreeSet::new(),
            overlaps: BTreeSet::new(),
            steps: Vec::new(),
            last_events: Vec::new(),
        })
    }
    pub fn tick(&self) -> u32 {
        self.tick
    }
    pub fn outcome(&self) -> Outcome {
        self.outcome
    }
    pub fn scene(&self) -> &PhysicsScene {
        &self.scene
    }
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }
    pub fn last_events(&self) -> &[Event] {
        &self.last_events
    }
    pub fn states(&self) -> Vec<BodyState> {
        self.world
            .states()
            .into_iter()
            .map(BodyState::from)
            .collect()
    }
    pub fn state_hash(&self) -> StateHash {
        hash::state(self)
    }
    pub fn impulses_used(&self) -> u32 {
        self.impulses_used
    }
    pub fn contacts(&self) -> &BTreeSet<(EntityId, EntityId)> {
        &self.contacts
    }
    pub fn overlaps(&self) -> &BTreeSet<(EntityId, EntityId)> {
        &self.overlaps
    }
    fn can_impulse(&self) -> bool {
        self.outcome == Outcome::InProgress
            && self
                .scene
                .settings()
                .control
                .is_some_and(|c| c.max_impulses == 0 || self.impulses_used < c.max_impulses)
    }
    pub fn observe(&self) -> Observation {
        let mut scene = self.scene.to_document();
        for b in &mut scene.bodies {
            b.label = None;
        }
        Observation {
            tick: self.tick,
            outcome: self.outcome,
            bodies: self.states(),
            settings: *self.scene.settings(),
            scene,
            impulses_used: self.impulses_used,
            can_impulse: self.can_impulse(),
            state_hash: self.state_hash(),
            contacts: self.contacts.iter().copied().collect(),
            overlaps: self.overlaps.iter().copied().collect(),
        }
    }
    pub fn apply(&mut self, action: Action) -> Result<TickResult, EpisodeError> {
        if self.outcome != Outcome::InProgress {
            return Err(EpisodeError::Finished(self.outcome));
        }
        let control = self
            .scene
            .settings()
            .control
            .ok_or(EpisodeError::NotPlayable)?;
        if let Action::Impulse { impulse } = action {
            if !impulse.is_finite() || impulse.length() > control.max_impulse {
                return Err(EpisodeError::InvalidImpulse {
                    max: control.max_impulse,
                });
            }
            if !self.can_impulse() {
                return Err(EpisodeError::ImpulseBudget);
            }
        }
        let mut events = Vec::new();
        if let Action::Impulse { impulse } = action {
            self.world.apply_impulse(control.body, impulse);
            self.impulses_used += 1;
            events.push(Event::ImpulseApplied {
                body: control.body,
                impulse,
            });
        }
        self.world.step();
        self.tick += 1;
        let contacts = self.world.contacts();
        let sensors = self
            .scene
            .bodies()
            .filter(|b| b.sensor)
            .map(|b| b.id)
            .collect();
        let overlaps = self.world.overlaps(&sensors);
        for &(a, b) in contacts.difference(&self.contacts) {
            events.push(Event::ContactStarted { a, b });
        }
        for &(a, b) in self.contacts.difference(&contacts) {
            events.push(Event::ContactEnded { a, b });
        }
        for &(zone, body) in overlaps.difference(&self.overlaps) {
            events.push(Event::ZoneEntered { zone, body });
        }
        for &(zone, body) in self.overlaps.difference(&overlaps) {
            events.push(Event::ZoneLeft { zone, body });
        }
        self.contacts = contacts;
        self.overlaps = overlaps;
        let objective = self
            .scene
            .settings()
            .objective
            .ok_or(EpisodeError::NotPlayable)?;
        let states = self.states();
        let finite = states.iter().all(|b| {
            b.position.is_finite()
                && b.linear_velocity.is_finite()
                && b.angular_velocity.is_finite()
                && b.rotation.iter().all(|v| v.is_finite())
        });
        self.outcome = if !finite {
            Outcome::NumericalFailure
        } else if action == Action::Forfeit {
            Outcome::Forfeited
        } else if self.overlaps.contains(&(objective.zone, objective.body)) {
            Outcome::Won
        } else if states.iter().any(|b| {
            (b.id == objective.body || b.id == control.body)
                && !self.scene.settings().bounds.contains(b.position)
        }) {
            Outcome::LeftBounds
        } else if self.tick >= self.scene.settings().max_ticks {
            Outcome::TickLimit
        } else {
            Outcome::InProgress
        };
        if self.outcome != Outcome::InProgress {
            events.push(Event::Finished {
                outcome: self.outcome,
            });
        }
        let hash = self.state_hash();
        self.steps.push(Step {
            tick: self.tick,
            action,
            hash,
        });
        self.last_events = events.clone();
        Ok(TickResult {
            tick: self.tick,
            outcome: self.outcome,
            state_hash: hash,
            events,
        })
    }
}
