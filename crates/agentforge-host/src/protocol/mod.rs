//! The versioned wire contract.
//!
//! Every input line is one [`RequestEnvelope`]; every output line is one
//! [`ResponseEnvelope`] carrying the same `request_id`. A `request_id` is for correlation
//! only: the engine does not deduplicate. Safe retries come from `expected_revision` on
//! authoring mutations and `expected_tick` on player actions, which make a duplicate
//! delivery fail loudly instead of applying twice.

pub mod catalog;
pub mod ops;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Protocol version implemented by this build.
pub const PROTOCOL_VERSION: u32 = 1;

/// Protocol versions this build accepts.
pub const SUPPORTED_PROTOCOL_VERSIONS: [u32; 1] = [PROTOCOL_VERSION];

/// Largest accepted request line, in bytes (excluding the newline).
pub const MAX_REQUEST_BYTES: usize = 1 << 20;

/// Longest accepted `request_id`, in characters.
pub const MAX_REQUEST_ID_CHARS: usize = 128;

/// Capability of a session, fixed by the launcher when the session is created.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Trusted: may create, inspect and modify projects, scenes and entities, run analysis,
    /// snapshots and replay verification, and also play episodes.
    Author,
    /// Restricted: may only play episodes of the scene the launcher chose, observe them and
    /// export their replays. Cannot change rules, spawn entities, set state or restore.
    Player,
}

impl Role {
    /// Wire name of the role.
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Author => "author",
            Role::Player => "player",
        }
    }
}

/// One request line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    /// Client-chosen correlation id (1-128 characters), echoed in the response.
    pub request_id: String,
    /// Must be one of the supported protocol versions.
    pub protocol_version: u32,
    /// Operation name, e.g. `entity.create`. See `discover`.
    pub operation: String,
    /// Operation arguments; may be omitted for operations without arguments.
    #[serde(default)]
    pub arguments: Value,
}

/// Whether a request succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    /// `result` is present.
    Ok,
    /// `error` is present.
    Error,
}

/// One response line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResponseEnvelope {
    /// The request's `request_id`, or null if it could not be read.
    pub request_id: Option<String>,
    /// Protocol version of this response.
    pub protocol_version: u32,
    /// The request's operation name, when it could be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// `ok` or `error`.
    pub status: ResponseStatus,
    /// Session revision after handling the request. It increases with every accepted
    /// authoring mutation (including snapshot restores) and never decreases.
    pub revision: u64,
    /// Typed result (see the operation's result schema). Present when `status` is `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Present when `status` is `error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

/// A structured error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorBody {
    /// Stable machine-readable code.
    pub code: ErrorCode,
    /// Concise explanation.
    pub message: String,
    /// Relevant ids, fields and values.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub details: Map<String, Value>,
    /// Suggested recovery, when one is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

macro_rules! error_codes {
    ($($variant:ident => $name:literal : $meaning:literal,)+) => {
        /// Stable error codes. Codes are never renamed within a protocol version.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum ErrorCode {
            $(#[doc = $meaning] $variant,)+
        }

        impl ErrorCode {
            /// Every code, in documentation order.
            pub const ALL: &'static [ErrorCode] = &[$(ErrorCode::$variant,)+];

            /// Wire name.
            pub const fn as_str(self) -> &'static str {
                match self { $(ErrorCode::$variant => $name,)+ }
            }

            /// What the code means.
            pub const fn meaning(self) -> &'static str {
                match self { $(ErrorCode::$variant => $meaning,)+ }
            }
        }
    };
}

error_codes! {
    MalformedJson => "malformed_json": "The line is not valid UTF-8 JSON.",
    InvalidEnvelope => "invalid_envelope": "The JSON is not a valid request envelope (missing, mistyped or unknown top-level fields).",
    UnsupportedProtocolVersion => "unsupported_protocol_version": "protocol_version is not supported by this engine.",
    RequestTooLarge => "request_too_large": "The request line exceeds the size limit.",
    UnknownOperation => "unknown_operation": "No operation has this name.",
    InvalidArguments => "invalid_arguments": "Arguments do not match the operation's schema or are inconsistent.",
    PermissionDenied => "permission_denied": "This session's role may not perform the operation.",
    NoProjectOpen => "no_project_open": "The operation needs an open project.",
    SceneNotFound => "scene_not_found": "The project has no scene with this name.",
    SceneAlreadyExists => "scene_already_exists": "A scene with this name already exists.",
    EntityNotFound => "entity_not_found": "The scene has no entity with this id.",
    InvalidName => "invalid_name": "Names use 1-64 characters of a-z, 0-9, '_' or '-'.",
    InvalidLayout => "invalid_layout": "The ASCII layout cannot be parsed.",
    InvalidGridSize => "invalid_grid_size": "Grid width and height must be between 1 and 64.",
    StaleRevision => "stale_revision": "expected_revision does not match the current revision; nothing was changed.",
    OutOfBounds => "out_of_bounds": "A position (or move target) lies outside the grid.",
    CellOccupied => "cell_occupied": "The target cell already holds an entity.",
    DuplicatePlayer => "duplicate_player": "A scene holds at most one player.",
    DuplicateKeyColor => "duplicate_key_color": "A scene holds at most one key per color.",
    InvalidLabel => "invalid_label": "Labels are 1-64 characters without control characters.",
    InvalidRules => "invalid_rules": "A rule parameter is out of range.",
    SceneNotPlayable => "scene_not_playable": "The scene lacks a player or an exit, so no episode can start.",
    NoActiveEpisode => "no_active_episode": "The operation needs a started episode.",
    EpisodeFinished => "episode_finished": "The episode already ended; start or reset one.",
    StaleEpisode => "stale_episode": "expected_episode_id is not the active episode; the action was not applied.",
    StaleTick => "stale_tick": "expected_tick does not match the episode's tick; the action was not applied.",
    MoveBlocked => "move_blocked": "A wall occupies the move target.",
    DoorLocked => "door_locked": "The move target is a door whose key the player does not hold.",
    SnapshotNotFound => "snapshot_not_found": "No snapshot has this id in this session.",
    LimitExceeded => "limit_exceeded": "A documented limit (batch size, snapshot count, ...) would be exceeded.",
    StorageError => "storage_error": "A file could not be read or written.",
    InvalidProjectFile => "invalid_project_file": "The project file is malformed or violates scene invariants.",
    InvalidReplay => "invalid_replay": "The replay file is malformed or its scene is invalid.",
    IncompatibleVersion => "incompatible_version": "A file uses a format or hash version this engine does not support.",
    InternalError => "internal_error": "Engine bug. State changes commit only after a handler completes, so the request had no effect.",
}

impl ResponseEnvelope {
    /// A success response.
    pub fn ok(
        request_id: Option<String>,
        operation: Option<String>,
        revision: u64,
        result: Value,
    ) -> Self {
        Self {
            request_id,
            protocol_version: PROTOCOL_VERSION,
            operation,
            status: ResponseStatus::Ok,
            revision,
            result: Some(result),
            error: None,
        }
    }

    /// An error response.
    pub fn error(
        request_id: Option<String>,
        operation: Option<String>,
        revision: u64,
        error: ErrorBody,
    ) -> Self {
        Self {
            request_id,
            protocol_version: PROTOCOL_VERSION,
            operation,
            status: ResponseStatus::Error,
            revision,
            result: None,
            error: Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_wire_names_match_serde() {
        for code in ErrorCode::ALL {
            let json = serde_json::to_value(code).expect("serializable");
            assert_eq!(json, Value::String(code.as_str().to_owned()));
        }
    }
}
