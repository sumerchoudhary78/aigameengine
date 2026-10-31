//! Mapping of domain errors to structured protocol errors.

use agentforge_core::analysis::AnalysisError;
use agentforge_core::entity::EntityKind;
use agentforge_core::episode::{ActionError, Episode, StartError};
use agentforge_core::layout::LayoutError;
use agentforge_core::project::{Name, NameError, ProjectError};
use agentforge_core::replay::ReplayError;
use agentforge_core::scene::{BatchError, SceneError};
use serde::Serialize;
use serde_json::Map;

use crate::protocol::{ErrorBody, ErrorCode};
use crate::storage::StorageError;

/// A protocol error under construction. Boxed, because errors travel through many
/// `Result`s and the wire body is comparatively large.
#[derive(Debug, Clone)]
pub struct ApiError(Box<ErrorBody>);

impl ApiError {
    /// An error with a code and message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self(Box::new(ErrorBody {
            code,
            message: message.into(),
            details: Map::new(),
            hint: None,
        }))
    }

    /// Adds a detail field.
    pub fn with(mut self, key: &str, value: impl Serialize) -> Self {
        if let Ok(value) = serde_json::to_value(value) {
            self.0.details.insert(key.to_owned(), value);
        }
        self
    }

    /// Sets the recovery hint.
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.0.hint = Some(hint.into());
        self
    }

    /// The error code.
    pub fn code(&self) -> ErrorCode {
        self.0.code
    }

    /// Converts into the wire representation.
    pub fn into_body(self) -> ErrorBody {
        *self.0
    }
}

pub(crate) fn invalid_arguments(message: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::InvalidArguments, message)
}

pub(crate) fn name_error(error: NameError, field: &str) -> ApiError {
    ApiError::new(ErrorCode::InvalidName, error.to_string())
        .with("field", field)
        .with("value", error.name)
        .hint("Use lowercase letters, digits, '_' or '-', e.g. \"blocked_dungeon\".")
}

pub(crate) fn scene_not_found(name: &Name, available: Vec<String>) -> ApiError {
    ApiError::new(
        ErrorCode::SceneNotFound,
        format!("the open project has no scene named {name:?}"),
    )
    .with("scene", name.as_str())
    .with("available_scenes", available)
    .hint("Use one of available_scenes, or create the scene with scene.create.")
}

/// Maps a scene invariant violation.
pub(crate) fn scene_error(error: SceneError) -> ApiError {
    let message = error.to_string();
    match error {
        SceneError::InvalidGridSize { size, max } => ApiError::new(ErrorCode::InvalidGridSize, message)
            .with("size", size)
            .with("max_dimension", max),
        SceneError::OutOfBounds { position, size } => ApiError::new(ErrorCode::OutOfBounds, message)
            .with("position", position)
            .with("grid", size)
            .hint(format!(
                "Valid cells have 0 <= x < {} and 0 <= y < {}.",
                size.width, size.height
            )),
        SceneError::CellOccupied {
            position,
            occupant,
            occupant_kind,
        } => ApiError::new(ErrorCode::CellOccupied, message)
            .with("position", position)
            .with("occupant", occupant)
            .with("occupant_kind", occupant_kind)
            .hint(format!(
                "Each cell holds one entity. Remove or move entity {occupant} first (a batch can do both atomically), or choose an empty cell."
            )),
        SceneError::EntityNotFound { id } => ApiError::new(ErrorCode::EntityNotFound, message)
            .with("entity", id)
            .hint("List current entities with scene.get. Removed ids are never reused."),
        SceneError::DuplicatePlayer { existing } => ApiError::new(ErrorCode::DuplicatePlayer, message)
            .with("existing", existing)
            .hint(format!("Move the existing player with entity.update (id {}).", existing.get())),
        SceneError::DuplicateKeyColor { color, existing } => {
            ApiError::new(ErrorCode::DuplicateKeyColor, message)
                .with("color", color)
                .with("existing", existing)
                .hint("Use another color; one key opens every door of its color.")
        }
        SceneError::InvalidLabel { .. } => ApiError::new(ErrorCode::InvalidLabel, message),
        SceneError::InvalidPortalPair { pair } => {
            ApiError::new(ErrorCode::InvalidPortalPair, message)
                .with("pair", pair)
                .hint("Pair ids are 0-9, so a scene has at most ten portal pairs.")
        }
        SceneError::InvalidGateGroup { group } => {
            ApiError::new(ErrorCode::InvalidGateGroup, message)
                .with("group", group)
                .hint("Gate groups are 0-3, so a scene has at most four groups.")
        }
        SceneError::InvalidRules { field, .. } => {
            ApiError::new(ErrorCode::InvalidRules, message).with("field", field)
        }
        SceneError::EmptyUpdate { op } => invalid_arguments(message).with("op", op),
        SceneError::IdSpaceExhausted => ApiError::new(ErrorCode::LimitExceeded, message),
        SceneError::InvalidNextEntityId { next_entity_id, .. } => {
            invalid_arguments(message).with("next_entity_id", next_entity_id)
        }
        SceneError::DuplicateEntityId { id } | SceneError::IdOutsideAllocatedRange { id, .. } => {
            invalid_arguments(message).with("entity", id)
        }
    }
}

/// Maps a failed batch, adding the failing edit's index.
pub(crate) fn batch_error(error: BatchError) -> ApiError {
    let index = error.index;
    let mut mapped = scene_error(error.error);
    mapped.0.message = format!(
        "edit {index} was rejected: {}; no edit of the batch was applied",
        mapped.0.message
    );
    mapped.with("edit_index", index)
}

/// Maps a failed single-edit operation (no index: there is only one edit).
pub(crate) fn single_edit_error(error: BatchError) -> ApiError {
    scene_error(error.error)
}

/// Maps a layout parse failure.
pub(crate) fn layout_error(error: LayoutError) -> ApiError {
    let message = error.to_string();
    let base = ApiError::new(ErrorCode::InvalidLayout, message).hint(
        "Rows must be equally wide and use only legend glyphs (see discover: game.ascii_legend).",
    );
    match error {
        LayoutError::RaggedRow {
            row,
            expected,
            actual,
        } => base
            .with("row", row)
            .with("expected_width", expected)
            .with("actual_width", actual),
        LayoutError::UnknownGlyph { glyph, position } => base
            .with("glyph", glyph.to_string())
            .with("position", position),
        LayoutError::TooLarge { width, height, max } => base
            .with("width", width)
            .with("height", height)
            .with("max_dimension", max),
        LayoutError::Empty => base,
    }
}

/// Maps an episode start failure.
pub(crate) fn start_error(error: StartError) -> ApiError {
    ApiError::new(ErrorCode::SceneNotPlayable, error.to_string())
        .with("issues", &error.issues)
        .hint("Add the missing player or exit (scene.validate lists every problem).")
}

/// Maps a rejected action, with the legal alternatives.
pub(crate) fn action_error(error: ActionError, episode: &Episode) -> ApiError {
    let message = error.to_string();
    let legal = episode.legal_actions();
    let with_legal = |e: ApiError| e.with("legal_actions", &legal).with("tick", episode.tick());
    match error {
        ActionError::EpisodeFinished { outcome } => {
            ApiError::new(ErrorCode::EpisodeFinished, message)
                .with("outcome", outcome)
                .hint("Start a new episode with episode.start or episode.reset.")
        }
        ActionError::OutOfBounds {
            from,
            direction,
            target,
        } => with_legal(
            ApiError::new(ErrorCode::OutOfBounds, message)
                .with("from", from)
                .with("direction", direction)
                .with("target", target),
        )
        .hint("Choose one of legal_actions."),
        ActionError::Blocked {
            from,
            direction,
            target,
            blocker,
        } => with_legal(
            ApiError::new(ErrorCode::MoveBlocked, message)
                .with("from", from)
                .with("direction", direction)
                .with("target", target)
                .with("blocker", blocker)
                .with("blocker_kind", "wall"),
        )
        .hint("Walls never open. Choose one of legal_actions."),
        ActionError::DoorLocked {
            from,
            direction,
            target,
            door,
            required,
        } => {
            let key = episode
                .scene()
                .entities()
                .find(|e| e.kind == EntityKind::Key { color: required });
            let hint = match key {
                Some(key) => format!(
                    "Collect the {required} key first: entity {} at {}.",
                    key.id, key.position
                ),
                None => {
                    format!("No {required} key exists in this scene, so this door never opens.")
                }
            };
            with_legal(
                ApiError::new(ErrorCode::DoorLocked, message)
                    .with("from", from)
                    .with("direction", direction)
                    .with("target", target)
                    .with("door", door)
                    .with("required_key", required),
            )
            .hint(hint)
        }
        ActionError::OneWayBlocked {
            from,
            direction,
            target,
            tile,
            allowed,
        } => with_legal(
            ApiError::new(ErrorCode::OneWayBlocked, message)
                .with("from", from)
                .with("direction", direction)
                .with("target", target)
                .with("tile", tile)
                .with("allowed_direction", allowed),
        )
        .hint("This tile can only be entered while moving in allowed_direction."),
        ActionError::GateClosed {
            from,
            direction,
            target,
            gate,
            group,
        } => {
            let switch = episode.scene().entities().find(
                |e| matches!(e.kind, EntityKind::Switch { group: group2 } if group2 == group),
            );
            let hint = match switch {
                Some(switch) => format!(
                    "Press the switch of group {group} first: entity {} at {}.",
                    switch.id, switch.position
                ),
                None => format!(
                    "No switch of group {group} exists in this scene, so this gate never opens."
                ),
            };
            with_legal(
                ApiError::new(ErrorCode::GateClosed, message)
                    .with("from", from)
                    .with("direction", direction)
                    .with("target", target)
                    .with("gate", gate)
                    .with("group", group),
            )
            .hint(hint)
        }
    }
}

/// Maps a reachability query failure.
pub(crate) fn analysis_error(error: AnalysisError) -> ApiError {
    let message = error.to_string();
    match error {
        AnalysisError::NoPlayer | AnalysisError::NoExit => {
            ApiError::new(ErrorCode::SceneNotPlayable, message)
        }
        AnalysisError::OutOfBounds { field, position } => {
            ApiError::new(ErrorCode::OutOfBounds, message)
                .with("field", field)
                .with("position", position)
        }
        AnalysisError::StartInsideWall { position, wall } => invalid_arguments(message)
            .with("field", "from")
            .with("position", position)
            .with("wall", wall),
        AnalysisError::StateSpaceTooLarge { states, cap } => ApiError::new(
            ErrorCode::LimitExceeded,
            message,
        )
        .with("states", states)
        .with("cap", cap)
        .hint(
            "The exhaustive search refuses to guess. Reduce the number of distinct key colors or gate groups the player can reach, then retry.",
        ),
    }
}

/// Maps a replay that cannot be verified.
pub(crate) fn replay_error(error: ReplayError) -> ApiError {
    let message = error.to_string();
    match error {
        ReplayError::UnsupportedFormat { .. }
        | ReplayError::UnsupportedFormatVersion { .. }
        | ReplayError::UnsupportedHashVersion { .. } => {
            ApiError::new(ErrorCode::IncompatibleVersion, message)
        }
        ReplayError::InvalidScene(_) | ReplayError::NotPlayable(_) => {
            ApiError::new(ErrorCode::InvalidReplay, message)
        }
    }
}

/// Maps a storage failure; `what` says whether a project or replay was involved.
pub(crate) fn storage_error(error: StorageError, what: &'static str) -> ApiError {
    let message = error.to_string();
    match error {
        StorageError::Io {
            action,
            path,
            source,
        } => ApiError::new(ErrorCode::StorageError, message)
            .with("path", path.display().to_string())
            .with("action", action)
            .with("io_error", source.kind().to_string()),
        StorageError::TooLarge { path, size } => ApiError::new(ErrorCode::LimitExceeded, message)
            .with("path", path.display().to_string())
            .with("size", size),
        StorageError::Malformed { path, .. } => {
            let code = if what == "replay" {
                ErrorCode::InvalidReplay
            } else {
                ErrorCode::InvalidProjectFile
            };
            ApiError::new(code, message).with("path", path.display().to_string())
        }
        StorageError::Incompatible { path, .. } => {
            ApiError::new(ErrorCode::IncompatibleVersion, message)
                .with("path", path.display().to_string())
        }
        StorageError::InvalidProject { path, source } => {
            let code = match source {
                ProjectError::UnsupportedFormat { .. }
                | ProjectError::UnsupportedFormatVersion { .. } => ErrorCode::IncompatibleVersion,
                ProjectError::InvalidName(_) | ProjectError::InvalidScene { .. } => {
                    ErrorCode::InvalidProjectFile
                }
            };
            ApiError::new(code, message).with("path", path.display().to_string())
        }
    }
}
