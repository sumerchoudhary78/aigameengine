//! Episode handlers, shared by author and player sessions, and replay export.

use std::path::PathBuf;

use agentforge_core::episode::{Episode, EventKind, Outcome};

use super::errors::{ApiError, action_error, start_error, storage_error};
use super::{ActiveEpisode, Host, no_active_episode};
use crate::protocol::catalog::{DEFAULT_EVENTS_LIMIT, MAX_ASSERTIONS, MAX_EVENTS_PER_QUERY};
use crate::protocol::ops::*;
use crate::protocol::{ErrorCode, Role};
use crate::storage;

impl Host {
    pub(super) fn episode_start(
        &mut self,
        args: EpisodeStartArgs,
    ) -> Result<EpisodeStarted, ApiError> {
        let scene_name = match (&self.bound_scene, args.scene) {
            (Some(bound), Some(requested)) if requested != bound.as_str() => {
                return Err(ApiError::new(
                    ErrorCode::PermissionDenied,
                    format!("this player session is bound to scene {:?}", bound.as_str()),
                )
                .with("bound_scene", bound.as_str())
                .hint("Omit `scene`, or ask the launcher for a session bound to another scene."));
            }
            (Some(bound), _) => bound.to_string(),
            (None, Some(requested)) => requested,
            (None, None) => {
                return Err(super::errors::invalid_arguments(
                    "`scene` is required in author sessions",
                ));
            }
        };
        let (name, scene) = self.scene(&scene_name)?;
        let episode = Episode::start(scene).map_err(start_error)?;
        Ok(self.begin(ActiveEpisode {
            id: self.next_episode_id,
            scene: name,
            scene_revision: self.revision,
            episode,
        }))
    }

    pub(super) fn episode_reset(&mut self) -> Result<EpisodeStarted, ApiError> {
        let active = self.active_episode()?;
        let episode = Episode::start(active.episode.scene()).map_err(start_error)?;
        let restarted = ActiveEpisode {
            id: self.next_episode_id,
            scene: active.scene.clone(),
            scene_revision: active.scene_revision,
            episode,
        };
        Ok(self.begin(restarted))
    }

    /// Installs a new active episode. Infallible.
    fn begin(&mut self, active: ActiveEpisode) -> EpisodeStarted {
        let started = EpisodeStarted {
            episode_id: active.id,
            scene: active.scene.to_string(),
            scene_revision: active.scene_revision,
            initial_hash: active.episode.initial_hash(),
            observation: active.episode.observation(),
        };
        self.next_episode_id = active.id + 1;
        self.episode = Some(active);
        started
    }

    pub(super) fn episode_observe(&self) -> Result<EpisodeObserved, ApiError> {
        let active = self.active_episode()?;
        Ok(EpisodeObserved {
            episode_id: active.id,
            scene: active.scene.to_string(),
            observation: active.episode.observation(),
        })
    }

    pub(super) fn episode_act(&mut self, args: ActArgs) -> Result<ActResult, ApiError> {
        let active = self.episode.as_mut().ok_or_else(no_active_episode)?;
        if let Some(expected) = args.expected_episode_id
            && expected != active.id
        {
            return Err(ApiError::new(
                ErrorCode::StaleEpisode,
                format!(
                    "expected_episode_id {expected} is not the active episode {}; the action was not applied",
                    active.id
                ),
            )
            .with("expected_episode_id", expected)
            .with("active_episode_id", active.id)
            .hint("The episode was reset, restarted or restored since; call episode.observe and decide again."));
        }
        let tick = active.episode.tick();
        if let Some(expected) = args.expected_tick
            && expected != tick
        {
            return Err(ApiError::new(
                ErrorCode::StaleTick,
                format!("expected_tick {expected} does not match the current tick {tick}; the action was not applied"),
            )
            .with("expected_tick", expected)
            .with("current_tick", tick)
            .hint("If a previous attempt of this action succeeded, do not resend it; call episode.observe."));
        }
        // Episode::apply validates before committing, so an error leaves it untouched.
        let step = match active.episode.apply(args.action) {
            Ok(step) => step,
            Err(error) => return Err(action_error(error, &active.episode)),
        };
        Ok(ActResult {
            episode_id: active.id,
            tick: step.tick,
            action: args.action,
            events: step.events,
            outcome: step.outcome,
            state_hash: step.hash,
            observation: args
                .include_observation
                .then(|| active.episode.observation()),
        })
    }

    pub(super) fn episode_events(&self, args: EventsArgs) -> Result<EventsResult, ApiError> {
        let active = self.active_episode()?;
        let limit = args.limit.unwrap_or(DEFAULT_EVENTS_LIMIT);
        if limit == 0 || limit > MAX_EVENTS_PER_QUERY {
            return Err(super::errors::invalid_arguments(format!(
                "`limit` must be between 1 and {MAX_EVENTS_PER_QUERY}"
            )));
        }
        let matching: Vec<_> = active
            .episode
            .events()
            .iter()
            .filter(|event| args.since_tick.is_none_or(|since| event.tick > since))
            .collect();
        let limit = limit as usize;
        let skip = matching.len().saturating_sub(limit);
        Ok(EventsResult {
            episode_id: active.id,
            tick: active.episode.tick(),
            total_matching: u32::try_from(matching.len()).unwrap_or(u32::MAX),
            truncated: skip > 0,
            events: matching.into_iter().skip(skip).cloned().collect(),
        })
    }

    pub(super) fn episode_assert(&self, args: AssertArgs) -> Result<AssertResult, ApiError> {
        let active = self.active_episode()?;
        if args.assertions.is_empty() || args.assertions.len() > MAX_ASSERTIONS {
            return Err(super::errors::invalid_arguments(format!(
                "`assertions` must hold 1 to {MAX_ASSERTIONS} entries"
            )));
        }
        let episode = &active.episode;
        let state = episode.state();
        let results: Vec<AssertionResult> = args
            .assertions
            .into_iter()
            .map(|assertion| {
                let (passed, actual) = match assertion {
                    Assertion::OutcomeIs { status } => {
                        let actual = match state.outcome {
                            Outcome::InProgress => OutcomeStatus::InProgress,
                            Outcome::Won { .. } => OutcomeStatus::Won,
                            Outcome::Failed { .. } => OutcomeStatus::Failed,
                        };
                        (actual == status, state.outcome.to_string())
                    }
                    Assertion::PlayerAt { position } => (
                        state.player == position,
                        format!("player at {}", state.player),
                    ),
                    Assertion::HasKey { color } => (
                        state.inventory.contains(color),
                        format!("holding {:?}", state.inventory.colors()),
                    ),
                    Assertion::TickAtMost { tick } => {
                        (state.tick <= tick, format!("tick {}", state.tick))
                    }
                    Assertion::EventOccurred { event } => {
                        let count = episode
                            .events()
                            .iter()
                            .filter(|e| event_type(&e.kind) == event)
                            .count();
                        (count > 0, format!("{count} matching event(s)"))
                    }
                };
                AssertionResult {
                    assertion,
                    passed,
                    actual,
                }
            })
            .collect();
        Ok(AssertResult {
            episode_id: active.id,
            all_passed: results.iter().all(|r| r.passed),
            results,
        })
    }

    pub(super) fn replay_export(&self, args: ReplayExportArgs) -> Result<ReplayExported, ApiError> {
        let active = self.active_episode()?;
        let mut replay = active.episode.to_replay(Some(active.scene.to_string()));
        if self.role == Role::Player {
            // Labels are authoring notes; players never see them.
            replay = replay.without_labels();
        }
        let fingerprint = replay.fingerprint();
        let destination = match (self.role, args.path) {
            (Role::Player, Some(_)) => {
                return Err(ApiError::new(
                    ErrorCode::PermissionDenied,
                    "player sessions cannot choose output paths",
                )
                .hint("Omit `path`: the replay is written to the launcher's --replay-dir, or returned inline."));
            }
            // Content-addressed: the same recording always gets the same name, and different
            // recordings never share one, even across player processes.
            (Role::Player, None) => self.replay_dir.as_ref().map(|directory| {
                let status = match replay.outcome {
                    Outcome::InProgress => "in-progress",
                    Outcome::Won { .. } => "won",
                    Outcome::Failed { .. } => "failed",
                };
                directory.join(format!(
                    "{}-{status}-{}t-{}.replay.json",
                    active.scene,
                    replay.steps.len(),
                    &fingerprint.to_hex()[..16]
                ))
            }),
            (Role::Author, path) => path.map(PathBuf::from),
        };
        let steps = u32::try_from(replay.steps.len()).unwrap_or(u32::MAX);
        let (outcome, final_hash) = (replay.outcome, replay.final_hash);
        match destination {
            Some(path) => {
                storage::save_replay(&path, &replay).map_err(|e| storage_error(e, "replay"))?;
                Ok(ReplayExported {
                    episode_id: active.id,
                    steps,
                    outcome,
                    final_hash,
                    fingerprint,
                    path: Some(path.display().to_string()),
                    replay: None,
                })
            }
            None => Ok(ReplayExported {
                episode_id: active.id,
                steps,
                outcome,
                final_hash,
                fingerprint,
                path: None,
                replay: Some(replay),
            }),
        }
    }
}

fn event_type(kind: &EventKind) -> EventType {
    match kind {
        EventKind::Started { .. } => EventType::Started,
        EventKind::Moved { .. } => EventType::Moved,
        EventKind::Waited => EventType::Waited,
        EventKind::KeyCollected { .. } => EventType::KeyCollected,
        EventKind::DoorPassed { .. } => EventType::DoorPassed,
        EventKind::ExitReached { .. } => EventType::ExitReached,
        EventKind::ExitRequirementsUnmet { .. } => EventType::ExitRequirementsUnmet,
        EventKind::PortalUsed { .. } => EventType::PortalUsed,
        EventKind::SwitchPressed { .. } => EventType::SwitchPressed,
        EventKind::HazardEntered { .. } => EventType::HazardEntered,
        EventKind::Won => EventType::Won,
        EventKind::Failed { .. } => EventType::Failed,
    }
}
