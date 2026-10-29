//! Typed arguments and results of every operation.
//!
//! Argument types reject unknown fields, so a misspelled field is an `invalid_arguments`
//! error instead of being silently ignored. JSON Schemas for all of these are generated
//! from the Rust definitions (see [`super::catalog`]).

use std::collections::BTreeMap;

use agentforge_core::analysis::ValidationReport;
use agentforge_core::entity::{Entity, EntityKind, KeyColor};
use agentforge_core::episode::{Action, Event, Observation, Outcome};
use agentforge_core::geometry::{GridSize, Position};
use agentforge_core::hash::StateHash;
use agentforge_core::ids::EntityId;
use agentforge_core::layout::LegendEntry;
use agentforge_core::replay::{Replay, ReplayVerification};
use agentforge_core::rules::{Rules, RulesPatch, WinCondition};
use agentforge_core::scene::{EditEffect, SceneEdit};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ErrorCode, Role};

/// Arguments of operations that take none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

// ---------------------------------------------------------------------------------------
// discover / session.status

/// Arguments of `discover`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiscoverArgs {
    /// Describe only this operation, including its argument and result schemas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// Include argument and result schemas for every listed operation.
    #[serde(default)]
    pub include_schemas: bool,
}

/// Result of `discover`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DiscoverResult {
    /// Engine identity.
    pub engine: EngineInfo,
    /// Protocol version of this response.
    pub protocol_version: u32,
    /// Accepted protocol versions.
    pub supported_protocol_versions: Vec<u32>,
    /// This session's role (fixed by the launcher).
    pub role: Role,
    /// What this role may do, in plain language.
    pub role_capabilities: String,
    /// Rules of the simulated game.
    pub game: GameModel,
    /// Cross-cutting protocol conventions.
    pub conventions: Conventions,
    /// Numeric limits.
    pub constraints: Constraints,
    /// Operations this session may call (only those; nothing else is advertised).
    pub operations: Vec<OperationInfo>,
    /// Every error code and its meaning.
    pub error_codes: Vec<ErrorCodeInfo>,
}

/// Engine identity.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EngineInfo {
    /// Engine name (a temporary working name).
    pub name: String,
    /// Engine version.
    pub version: String,
    /// Canonical hash encoding version.
    pub state_hash_version: u32,
}

/// The game model, as data.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct GameModel {
    /// One-paragraph overview.
    pub summary: String,
    /// Observability guarantee.
    pub observability: String,
    /// How time advances.
    pub timing: String,
    /// Coordinate convention.
    pub coordinates: String,
    /// Entity kinds and their behaviour.
    pub entity_kinds: Vec<EntityKindInfo>,
    /// Supported key/door colors.
    pub key_colors: Vec<KeyColor>,
    /// Supported win conditions.
    pub win_conditions: Vec<NamedDescription>,
    /// How an episode can fail.
    pub failure_conditions: Vec<NamedDescription>,
    /// Order of resolution within one tick.
    pub tick_resolution_order: Vec<String>,
    /// ASCII glyphs used by layouts and observations.
    pub ascii_legend: Vec<LegendEntry>,
}

/// One entity kind.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EntityKindInfo {
    /// The kind's `type` tag.
    pub kind: String,
    /// Example `kind` object.
    pub example: EntityKind,
    /// Behaviour.
    pub description: String,
}

/// A named thing with a description.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct NamedDescription {
    /// Wire name.
    pub name: String,
    /// Meaning.
    pub description: String,
}

/// Cross-cutting conventions.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Conventions {
    /// Request/response framing.
    pub framing: String,
    /// Roles and permissions.
    pub roles: String,
    /// Revision and optimistic concurrency rules.
    pub revisions: String,
    /// Retry semantics.
    pub retries: String,
    /// Determinism scope.
    pub determinism: String,
    /// Editor/play separation.
    pub episodes: String,
}

/// Numeric limits.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Constraints {
    /// Largest grid width.
    pub max_grid_width: u32,
    /// Largest grid height.
    pub max_grid_height: u32,
    /// Largest `max_ticks`.
    pub max_ticks_limit: u32,
    /// Entities per cell.
    pub max_entities_per_cell: u32,
    /// Players per scene.
    pub max_players_per_scene: u32,
    /// Keys per color per scene.
    pub max_keys_per_color: u32,
    /// Number of key colors.
    pub key_color_count: u32,
    /// Largest request line in bytes.
    pub max_request_bytes: u64,
    /// Longest `request_id`.
    pub max_request_id_chars: u32,
    /// Most edits per batch.
    pub max_batch_edits: u32,
    /// Longest entity or snapshot label.
    pub max_label_chars: u32,
    /// Longest project or scene name.
    pub max_name_chars: u32,
    /// Most snapshots per session.
    pub max_snapshots: u32,
    /// Most events per `episode.events` call.
    pub max_events_per_query: u32,
    /// Most assertions per `episode.assert` call.
    pub max_assertions: u32,
    /// Most repair candidates verified per reachability query.
    pub max_repair_candidates: u32,
}

/// One operation in `discover`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OperationInfo {
    /// Operation name.
    pub name: String,
    /// What it does.
    pub summary: String,
    /// Roles allowed to call it.
    pub roles: Vec<Role>,
    /// Whether it changes the session revision when accepted.
    pub mutates: bool,
    /// A complete example request line.
    pub example: Value,
    /// JSON Schema of `arguments`, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments_schema: Option<Value>,
    /// JSON Schema of `result`, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_schema: Option<Value>,
}

/// One error code in `discover`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ErrorCodeInfo {
    /// The code.
    pub code: ErrorCode,
    /// Its meaning.
    pub meaning: String,
}

/// Result of `session.status`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SessionStatus {
    /// This session's role.
    pub role: Role,
    /// Current revision.
    pub revision: u64,
    /// For player sessions: the only scene they may play.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bound_scene: Option<String>,
    /// The open project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectInfo>,
    /// The active episode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<EpisodeBrief>,
    /// Snapshots captured in this session.
    pub snapshots: Vec<SnapshotInfo>,
}

// ---------------------------------------------------------------------------------------
// project.*

/// Arguments of `project.create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectCreateArgs {
    /// Project name.
    pub name: String,
    /// Must equal the current revision.
    pub expected_revision: u64,
}

/// Arguments of `project.load`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectLoadArgs {
    /// Path of a project file.
    pub path: String,
    /// Must equal the current revision.
    pub expected_revision: u64,
}

/// Arguments of `project.save`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectSaveArgs {
    /// Destination; defaults to the path the project was loaded from or last saved to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// If given, the save is rejected unless it equals the current revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

/// Summary of the open project.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProjectInfo {
    /// Project name.
    pub name: String,
    /// Where it was loaded from or last saved to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Whether it has changes not yet saved.
    pub dirty: bool,
    /// Scene names.
    pub scenes: Vec<String>,
}

/// Result of `project.save`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProjectSaved {
    /// File written (atomically).
    pub path: String,
    /// Bytes written.
    pub bytes: u64,
    /// Scenes saved.
    pub scenes: Vec<String>,
    /// Revision whose content was saved.
    pub revision: u64,
}

// ---------------------------------------------------------------------------------------
// scene.*

/// Arguments naming a scene.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SceneRef {
    /// Scene name.
    pub scene: String,
}

/// Arguments of `scene.create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SceneCreateArgs {
    /// Scene name.
    pub name: String,
    /// Must equal the current revision.
    pub expected_revision: u64,
    /// Grid width; required without `layout`, must match it otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// Grid height; required without `layout`, must match it otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// ASCII rows (see the legend in `discover`). Entities are created row by row, left to
    /// right, and receive ids in that order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Vec<String>>,
    /// Rule parameters; defaults to `{"max_ticks":100,"win_condition":"reach_exit"}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<Rules>,
}

/// A scene with everything an author may see.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SceneView {
    /// Scene name.
    pub scene: String,
    /// Grid dimensions.
    pub size: GridSize,
    /// Rule parameters.
    pub rules: Rules,
    /// ID allocator state.
    pub next_entity_id: u32,
    /// Entities in ascending id order, with labels.
    pub entities: Vec<Entity>,
    /// The scene as text rows (player shown at its spawn).
    pub ascii: Vec<String>,
    /// Canonical digest of the scene's simulation content.
    pub digest: StateHash,
}

/// Arguments of `scene.apply_batch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyBatchArgs {
    /// Scene name.
    pub scene: String,
    /// Must equal the current revision.
    pub expected_revision: u64,
    /// Edits applied in order; all succeed or none is applied.
    pub edits: Vec<SceneEdit>,
}

/// Result of `scene.apply_batch`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BatchApplied {
    /// Number of edits applied.
    pub applied: u32,
    /// One effect per edit, in order.
    pub effects: Vec<EditEffect>,
}

/// Arguments of `scene.update_rules`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateRulesArgs {
    /// Scene name.
    pub scene: String,
    /// Must equal the current revision.
    pub expected_revision: u64,
    /// Fields to change.
    pub rules: RulesPatch,
}

/// Result of `scene.update_rules`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct RulesUpdated {
    /// Rules before.
    pub before: Rules,
    /// Rules after.
    pub after: Rules,
}

/// Output formats of `scene.render`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RenderFormat {
    /// SVG document.
    #[default]
    Svg,
    /// ASCII rows joined by newlines.
    Ascii,
}

/// Arguments of `scene.render`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenderArgs {
    /// Scene name.
    pub scene: String,
    /// Output format (default `svg`).
    #[serde(default)]
    pub format: RenderFormat,
    /// If given, write the output to this file (atomically) instead of returning it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Result of `scene.render`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Rendered {
    /// Output format.
    pub format: RenderFormat,
    /// Size of the output.
    pub bytes: u64,
    /// File written, when `path` was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The output, when no `path` was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

// ---------------------------------------------------------------------------------------
// entity.*

/// Arguments of `entity.create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityCreateArgs {
    /// Scene name.
    pub scene: String,
    /// Must equal the current revision.
    pub expected_revision: u64,
    /// What to create.
    pub kind: EntityKind,
    /// Where.
    pub position: Position,
    /// Optional authoring note (never shown to players).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Arguments of `entity.get`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityRef {
    /// Scene name.
    pub scene: String,
    /// Entity id.
    pub id: EntityId,
}

/// Arguments of `entity.update`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityUpdateArgs {
    /// Scene name.
    pub scene: String,
    /// Must equal the current revision.
    pub expected_revision: u64,
    /// Entity id.
    pub id: EntityId,
    /// New kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<EntityKind>,
    /// New position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    /// New label; an empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Arguments of `entity.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityRemoveArgs {
    /// Scene name.
    pub scene: String,
    /// Must equal the current revision.
    pub expected_revision: u64,
    /// Entity id.
    pub id: EntityId,
}

/// Result of `entity.create` and `entity.get`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EntityView {
    /// The entity.
    pub entity: Entity,
}

/// Result of `entity.update`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EntityUpdated {
    /// Before the update.
    pub before: Entity,
    /// After the update.
    pub after: Entity,
}

/// Result of `entity.remove`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EntityRemoved {
    /// The removed entity.
    pub removed: Entity,
}

// ---------------------------------------------------------------------------------------
// analysis.*

/// Arguments of `analysis.reachability`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReachabilityArgs {
    /// Scene name.
    pub scene: String,
    /// Start cell; defaults to the player spawn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Position>,
    /// Target cell; defaults to the scene objective (win condition).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Position>,
}

// ---------------------------------------------------------------------------------------
// episode.*

/// Arguments of `episode.start`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EpisodeStartArgs {
    /// Scene to play. Required for authors; players may omit it (their scene is fixed by
    /// the launcher) or repeat it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
}

/// Result of `episode.start` and `episode.reset`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EpisodeStarted {
    /// New episode id (unique within the session).
    pub episode_id: u64,
    /// Scene being played.
    pub scene: String,
    /// Revision of the scene content the episode was started from.
    pub scene_revision: u64,
    /// State hash at tick 0.
    pub initial_hash: StateHash,
    /// The first observation.
    pub observation: Observation,
}

/// Result of `episode.observe`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EpisodeObserved {
    /// Episode id.
    pub episode_id: u64,
    /// Scene being played.
    pub scene: String,
    /// The current observation.
    pub observation: Observation,
}

/// Arguments of `episode.act`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActArgs {
    /// The action.
    pub action: Action,
    /// If given, the action is rejected (`stale_tick`) unless the episode is at this tick.
    /// Recommended: it makes retries safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_tick: Option<u32>,
    /// Include the new observation in the result (default true).
    #[serde(default = "default_true")]
    pub include_observation: bool,
}

fn default_true() -> bool {
    true
}

/// Result of `episode.act`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ActResult {
    /// Episode id.
    pub episode_id: u64,
    /// Tick reached.
    pub tick: u32,
    /// The accepted action.
    pub action: Action,
    /// Events of this tick, in resolution order.
    pub events: Vec<Event>,
    /// Outcome after the tick.
    pub outcome: Outcome,
    /// State hash after the tick.
    pub state_hash: StateHash,
    /// The new observation, unless disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<Observation>,
}

/// Arguments of `episode.events`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventsArgs {
    /// Only events with a tick greater than this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_tick: Option<u32>,
    /// Return at most this many of the most recent matching events (default 100, max 1000).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// Result of `episode.events`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EventsResult {
    /// Episode id.
    pub episode_id: u64,
    /// Current tick.
    pub tick: u32,
    /// Matching events, oldest first.
    pub events: Vec<Event>,
    /// Number of matching events before `limit` was applied.
    pub total_matching: u32,
    /// Whether older matching events were omitted.
    pub truncated: bool,
}

/// Coarse outcome status used by assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    /// Still running.
    InProgress,
    /// Won.
    Won,
    /// Failed.
    Failed,
}

/// Event type names used by assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    /// `started`.
    Started,
    /// `moved`.
    Moved,
    /// `waited`.
    Waited,
    /// `key_collected`.
    KeyCollected,
    /// `door_passed`.
    DoorPassed,
    /// `exit_reached`.
    ExitReached,
    /// `exit_requirements_unmet`.
    ExitRequirementsUnmet,
    /// `won`.
    Won,
    /// `failed`.
    Failed,
}

/// A check against the active episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Assertion {
    /// The outcome has this status.
    OutcomeIs {
        /// Expected status.
        status: OutcomeStatus,
    },
    /// The player stands on this cell.
    PlayerAt {
        /// Expected cell.
        position: Position,
    },
    /// The player holds this key.
    HasKey {
        /// Expected color.
        color: KeyColor,
    },
    /// At most this many ticks have elapsed.
    TickAtMost {
        /// Upper bound.
        tick: u32,
    },
    /// At least one event of this type was recorded.
    EventOccurred {
        /// Event type.
        event: EventType,
    },
}

/// Arguments of `episode.assert`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AssertArgs {
    /// Assertions to evaluate (1-256).
    pub assertions: Vec<Assertion>,
}

/// Result of one assertion.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AssertionResult {
    /// The assertion.
    pub assertion: Assertion,
    /// Whether it holds.
    pub passed: bool,
    /// The observed value, in words.
    pub actual: String,
}

/// Result of `episode.assert`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AssertResult {
    /// Episode id.
    pub episode_id: u64,
    /// Whether every assertion holds.
    pub all_passed: bool,
    /// One result per assertion, in order.
    pub results: Vec<AssertionResult>,
}

/// Summary of an episode.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct EpisodeBrief {
    /// Episode id.
    pub episode_id: u64,
    /// Scene being played.
    pub scene: String,
    /// Revision of the scene content it started from.
    pub scene_revision: u64,
    /// Current tick.
    pub tick: u32,
    /// Result so far.
    pub outcome: Outcome,
    /// Current state hash.
    pub state_hash: StateHash,
}

// ---------------------------------------------------------------------------------------
// snapshot.*

/// Arguments of `snapshot.capture`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotCaptureArgs {
    /// Optional note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Arguments of `snapshot.restore`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRestoreArgs {
    /// Snapshot id from `snapshot.capture`.
    pub snapshot_id: u64,
    /// Must equal the current revision.
    pub expected_revision: u64,
}

/// A captured snapshot.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SnapshotInfo {
    /// Snapshot id.
    pub snapshot_id: u64,
    /// Optional note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Revision when captured (metadata; restoring never rewinds the revision).
    pub captured_at_revision: u64,
    /// Project captured, if one was open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Digest of every scene captured.
    pub scene_digests: BTreeMap<String, StateHash>,
    /// Episode captured, if one was active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<EpisodeBrief>,
}

/// Result of `snapshot.restore`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SnapshotRestored {
    /// What was restored.
    pub snapshot: SnapshotInfo,
    /// The new revision (always greater than before the restore).
    pub revision: u64,
}

// ---------------------------------------------------------------------------------------
// replay.*

/// Arguments of `replay.export`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplayExportArgs {
    /// Author sessions only: write the replay to this file. Player sessions write to the
    /// launcher's replay directory (if configured) and may not choose paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// Result of `replay.export`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReplayExported {
    /// Episode id.
    pub episode_id: u64,
    /// Accepted actions recorded.
    pub steps: u32,
    /// Outcome at export time.
    pub outcome: Outcome,
    /// State hash at export time.
    pub final_hash: StateHash,
    /// File written, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The replay itself, when no file was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay: Option<Replay>,
}

/// Arguments of `replay.verify`. Provide exactly one of `path` and `replay`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplayVerifyArgs {
    /// Replay file to verify.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Inline replay to verify.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay: Option<Replay>,
    /// Also check that the replay's scene matches this scene of the open project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
}

/// Result of `replay.verify`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReplayVerified {
    /// Re-execution report.
    #[serde(flatten)]
    pub verification: ReplayVerification,
    /// With `scene`: whether the replay's scene digest equals that scene's digest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene_matches: Option<bool>,
}

/// Validation result (re-exported for the catalog).
pub type ValidateResult = ValidationReport;

/// Win conditions with descriptions, for `discover`.
pub fn win_conditions() -> Vec<NamedDescription> {
    [
        (WinCondition::ReachExit, "Step onto any exit."),
        (
            WinCondition::CollectAllKeysThenExit,
            "Hold every key that exists in the scene, then step onto an exit. Stepping onto an exit early is allowed and does nothing.",
        ),
    ]
    .into_iter()
    .map(|(win, description)| NamedDescription {
        name: win.as_str().to_owned(),
        description: description.to_owned(),
    })
    .collect()
}
