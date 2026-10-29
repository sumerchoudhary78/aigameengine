//! Replays: the initial scene, every accepted action and the state hash after each.
//!
//! Verification re-executes the recorded actions on a fresh episode and compares every
//! hash. Nothing is re-decided: a replay never consults a player, model or clock. Replays
//! with a different format or hash-encoding version are rejected explicitly.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::episode::{Action, Episode, Outcome, StartError};
use crate::hash::{STATE_HASH_VERSION, StateHash};
use crate::scene::{Scene, SceneDocument, SceneError};

/// Value of [`Replay::format`].
pub const REPLAY_FORMAT: &str = "agentforge.replay";

/// Supported [`Replay::format_version`].
pub const REPLAY_FORMAT_VERSION: u32 = 1;

/// A recorded episode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Replay {
    /// Always `"agentforge.replay"`.
    pub format: String,
    /// Replay file format version.
    pub format_version: u32,
    /// Canonical hash encoding version used for every hash in this file.
    pub state_hash_version: u32,
    /// Engine version that recorded the replay (informational).
    pub engine_version: String,
    /// Name of the scene in its project, if known (informational).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_name: Option<String>,
    /// The frozen scene the episode started from.
    pub scene: SceneDocument,
    /// Digest of `scene`.
    pub scene_digest: StateHash,
    /// State hash at tick 0.
    pub initial_hash: StateHash,
    /// Accepted actions in order.
    pub steps: Vec<ReplayStep>,
    /// Outcome after the last step.
    pub outcome: Outcome,
    /// State hash after the last step.
    pub final_hash: StateHash,
}

/// One accepted action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplayStep {
    /// Tick reached by the action.
    pub tick: u32,
    /// The action.
    pub action: Action,
    /// State hash after the action.
    pub hash: StateHash,
}

/// A replay that cannot be verified at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReplayError {
    /// Wrong `format` string.
    #[error("not an AgentForge replay: format is {found:?}, expected {REPLAY_FORMAT:?}")]
    UnsupportedFormat {
        /// The format found.
        found: String,
    },
    /// Unsupported `format_version`.
    #[error("replay format_version {found} is not supported (supported: {REPLAY_FORMAT_VERSION})")]
    UnsupportedFormatVersion {
        /// The version found.
        found: u32,
    },
    /// Unsupported `state_hash_version`.
    #[error("replay state_hash_version {found} is not supported (supported: {STATE_HASH_VERSION})")]
    UnsupportedHashVersion {
        /// The version found.
        found: u32,
    },
    /// The embedded scene violates scene invariants.
    #[error("replay scene is invalid: {0}")]
    InvalidScene(#[from] SceneError),
    /// The embedded scene cannot start an episode.
    #[error("replay scene cannot start an episode: {0}")]
    NotPlayable(#[from] StartError),
}

/// Where re-execution first disagreed with the recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Divergence {
    /// The embedded scene does not hash to the recorded digest.
    SceneDigest {
        /// Recorded value.
        expected: StateHash,
        /// Recomputed value.
        actual: StateHash,
    },
    /// The tick-0 state hash differs.
    InitialHash {
        /// Recorded value.
        expected: StateHash,
        /// Recomputed value.
        actual: StateHash,
    },
    /// A step records a tick that re-execution did not reach.
    Tick {
        /// Zero-based step index.
        step_index: u32,
        /// Recorded tick.
        expected: u32,
        /// Tick reached.
        actual: u32,
    },
    /// A recorded action was rejected on re-execution.
    ActionRejected {
        /// Zero-based step index.
        step_index: u32,
        /// The rejected action.
        action: Action,
        /// Why it was rejected.
        error: String,
    },
    /// The hash after a step differs.
    StateHash {
        /// Zero-based step index.
        step_index: u32,
        /// Tick of the step.
        tick: u32,
        /// Recorded value.
        expected: StateHash,
        /// Recomputed value.
        actual: StateHash,
    },
    /// The final outcome differs.
    Outcome {
        /// Recorded value.
        expected: Outcome,
        /// Reproduced value.
        actual: Outcome,
    },
    /// The final hash differs.
    FinalHash {
        /// Recorded value.
        expected: StateHash,
        /// Recomputed value.
        actual: StateHash,
    },
}

/// Result of [`verify`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReplayVerification {
    /// Every hash and the outcome matched.
    pub verified: bool,
    /// Steps re-executed with matching hashes.
    pub steps_checked: u32,
    /// Steps in the recording.
    pub steps_recorded: u32,
    /// Outcome stored in the replay.
    pub recorded_outcome: Outcome,
    /// Outcome reached by re-execution.
    pub reproduced_outcome: Outcome,
    /// State hash reached by re-execution.
    pub reproduced_final_hash: StateHash,
    /// Digest of the replay's scene.
    pub scene_digest: StateHash,
    /// Engine version that recorded the replay.
    pub engine_version_recorded: String,
    /// Engine version that verified it.
    pub engine_version_current: String,
    /// First disagreement, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub divergence: Option<Divergence>,
}

/// Rejects replays this build cannot interpret.
pub fn check_compatibility(replay: &Replay) -> Result<(), ReplayError> {
    if replay.format != REPLAY_FORMAT {
        return Err(ReplayError::UnsupportedFormat {
            found: replay.format.clone(),
        });
    }
    if replay.format_version != REPLAY_FORMAT_VERSION {
        return Err(ReplayError::UnsupportedFormatVersion {
            found: replay.format_version,
        });
    }
    if replay.state_hash_version != STATE_HASH_VERSION {
        return Err(ReplayError::UnsupportedHashVersion {
            found: replay.state_hash_version,
        });
    }
    Ok(())
}

/// Re-executes `replay` and compares every recorded hash and the outcome.
pub fn verify(replay: &Replay) -> Result<ReplayVerification, ReplayError> {
    check_compatibility(replay)?;
    let scene = Scene::from_document(replay.scene.clone())?;
    let mut episode = Episode::start(&scene)?;
    let mut steps_checked = 0u32;
    let divergence = if episode.scene_digest() != replay.scene_digest {
        Some(Divergence::SceneDigest {
            expected: replay.scene_digest,
            actual: episode.scene_digest(),
        })
    } else if episode.initial_hash() != replay.initial_hash {
        Some(Divergence::InitialHash {
            expected: replay.initial_hash,
            actual: episode.initial_hash(),
        })
    } else {
        replay_steps(&mut episode, replay, &mut steps_checked)
    };
    Ok(ReplayVerification {
        verified: divergence.is_none(),
        steps_checked,
        steps_recorded: u32::try_from(replay.steps.len()).unwrap_or(u32::MAX),
        recorded_outcome: replay.outcome,
        reproduced_outcome: episode.outcome(),
        reproduced_final_hash: episode.current_hash(),
        scene_digest: episode.scene_digest(),
        engine_version_recorded: replay.engine_version.clone(),
        engine_version_current: crate::ENGINE_VERSION.to_owned(),
        divergence,
    })
}

fn replay_steps(episode: &mut Episode, replay: &Replay, checked: &mut u32) -> Option<Divergence> {
    for (index, step) in replay.steps.iter().enumerate() {
        let step_index = u32::try_from(index).unwrap_or(u32::MAX);
        let result = match episode.apply(step.action) {
            Ok(result) => result,
            Err(error) => {
                return Some(Divergence::ActionRejected {
                    step_index,
                    action: step.action,
                    error: error.to_string(),
                });
            }
        };
        if result.tick != step.tick {
            return Some(Divergence::Tick {
                step_index,
                expected: step.tick,
                actual: result.tick,
            });
        }
        if result.hash != step.hash {
            return Some(Divergence::StateHash {
                step_index,
                tick: step.tick,
                expected: step.hash,
                actual: result.hash,
            });
        }
        *checked += 1;
    }
    if episode.outcome() != replay.outcome {
        return Some(Divergence::Outcome {
            expected: replay.outcome,
            actual: episode.outcome(),
        });
    }
    if episode.current_hash() != replay.final_hash {
        return Some(Divergence::FinalHash {
            expected: replay.final_hash,
            actual: episode.current_hash(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityKind;
    use crate::geometry::{Direction, GridSize, Position};
    use crate::rules::Rules;
    use crate::scene::SceneEdit;

    fn recorded() -> Replay {
        let edits = [(EntityKind::Player, 0), (EntityKind::Exit, 3)].map(|(kind, x)| {
            SceneEdit::CreateEntity {
                kind,
                position: Position::new(x, 0),
                label: None,
            }
        });
        let scene = Scene::new(GridSize::new(4, 1), Rules::default())
            .expect("valid")
            .apply_edits(&edits)
            .expect("valid")
            .0;
        let mut episode = Episode::start(&scene).expect("playable");
        for _ in 0..3 {
            episode
                .apply(Action::Move {
                    direction: Direction::East,
                })
                .expect("legal");
        }
        episode.to_replay(Some("line".to_owned()))
    }

    #[test]
    fn a_faithful_replay_verifies_and_survives_json() {
        let replay = recorded();
        let json = serde_json::to_string(&replay).expect("serializable");
        let parsed: Replay = serde_json::from_str(&json).expect("parsable");
        let report = verify(&parsed).expect("compatible");
        assert!(report.verified, "{report:?}");
        assert_eq!(report.steps_checked, 3);
        assert_eq!(report.reproduced_outcome, Outcome::Won { tick: 3 });
    }

    #[test]
    fn tampering_is_located_precisely() {
        let mut replay = recorded();
        replay.steps[1].action = Action::Wait;
        let report = verify(&replay).expect("compatible");
        assert!(!report.verified);
        assert!(matches!(
            report.divergence,
            Some(Divergence::StateHash { step_index: 1, .. })
        ));

        let mut moved_exit = recorded();
        moved_exit.scene.entities[1].position = Position::new(2, 0);
        let report = verify(&moved_exit).expect("compatible");
        assert!(matches!(
            report.divergence,
            Some(Divergence::SceneDigest { .. })
        ));
    }

    #[test]
    fn incompatible_versions_are_rejected_explicitly() {
        let mut replay = recorded();
        replay.state_hash_version = 2;
        assert_eq!(
            verify(&replay),
            Err(ReplayError::UnsupportedHashVersion { found: 2 })
        );
        let mut replay = recorded();
        replay.format = "something.else".to_owned();
        assert!(matches!(
            verify(&replay),
            Err(ReplayError::UnsupportedFormat { .. })
        ));
    }
}
