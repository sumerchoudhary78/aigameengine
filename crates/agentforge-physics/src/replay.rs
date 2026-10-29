//! Replay checkpoints rebuild complete Rapier state by recording inputs from tick zero.
use crate::{
    episode::{Episode, Outcome, Step},
    hash,
    scene::{PhysicsScene, PhysicsSceneDocument},
};
use agentforge_core::hash::{CanonicalEncoder, StateHash};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PHYSICS_COMPATIBILITY: &str = "rapier2d-f64-0.36.1-enhanced-determinism-v1";
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Replay {
    pub format: String,
    pub format_version: u32,
    pub engine_version: String,
    pub physics_version: String,
    pub scene: PhysicsSceneDocument,
    pub scene_hash: StateHash,
    pub initial_hash: StateHash,
    pub steps: Vec<Step>,
    pub final_hash: StateHash,
    pub outcome: Outcome,
}
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Verification {
    pub verified: bool,
    pub steps_checked: u32,
    pub outcome: Outcome,
    pub final_hash: StateHash,
}
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("incompatible physics replay format, engine, or Rapier version")]
    Incompatible,
    #[error("invalid replay: {0}")]
    Invalid(String),
    #[error("replay diverged at tick {tick}: {message}")]
    Divergence { tick: u32, message: String },
}
impl Replay {
    pub fn capture(episode: &Episode) -> Self {
        let mut scene = episode.scene().to_document();
        for b in &mut scene.bodies {
            b.label = None;
        }
        let initial =
            Episode::start(episode.scene()).expect("an existing episode has playable settings");
        Self {
            format: "agentforge.physics.replay".into(),
            format_version: 1,
            engine_version: ENGINE_VERSION.into(),
            physics_version: PHYSICS_COMPATIBILITY.into(),
            scene,
            scene_hash: hash::scene(episode.scene()),
            initial_hash: initial.state_hash(),
            steps: episode.steps().to_vec(),
            final_hash: episode.state_hash(),
            outcome: episode.outcome(),
        }
    }
    pub fn fingerprint(&self) -> StateHash {
        let mut e = CanonicalEncoder::new("agentforge.physics.recording.v1");
        e.bytes(self.scene_hash.as_bytes());
        e.bytes(self.initial_hash.as_bytes());
        e.u32(self.steps.len() as u32);
        for s in &self.steps {
            e.u32(s.tick);
            hash::action(&mut e, s.action);
            e.bytes(s.hash.as_bytes());
        }
        e.bytes(self.final_hash.as_bytes());
        e.finish()
    }
    pub fn restore(&self) -> Result<Episode, ReplayError> {
        if self.format != "agentforge.physics.replay"
            || self.format_version != 1
            || self.engine_version != ENGINE_VERSION
            || self.physics_version != PHYSICS_COMPATIBILITY
        {
            return Err(ReplayError::Incompatible);
        }
        let scene = PhysicsScene::from_document(self.scene.clone())
            .map_err(|e| ReplayError::Invalid(e.to_string()))?;
        if self.steps.len() > scene.settings().max_ticks as usize {
            return Err(ReplayError::Invalid("too many steps".into()));
        }
        let mut episode =
            Episode::start(&scene).map_err(|e| ReplayError::Invalid(e.to_string()))?;
        let mismatch = |tick, message: &str| ReplayError::Divergence {
            tick,
            message: message.into(),
        };
        if hash::scene(&scene) != self.scene_hash || episode.state_hash() != self.initial_hash {
            return Err(mismatch(0, "initial scene/state hash"));
        }
        for s in &self.steps {
            if s.tick != episode.tick() + 1 {
                return Err(mismatch(s.tick, "non-sequential tick"));
            }
            let result = episode
                .apply(s.action)
                .map_err(|e| ReplayError::Divergence {
                    tick: s.tick,
                    message: e.to_string(),
                })?;
            if result.state_hash != s.hash {
                return Err(mismatch(s.tick, "state hash"));
            }
        }
        if episode.state_hash() != self.final_hash || episode.outcome() != self.outcome {
            return Err(mismatch(episode.tick(), "final hash or outcome"));
        }
        Ok(episode)
    }
    pub fn verify(&self) -> Result<Verification, ReplayError> {
        let e = self.restore()?;
        Ok(Verification {
            verified: true,
            steps_checked: e.tick(),
            outcome: e.outcome(),
            final_hash: e.state_hash(),
        })
    }
}
