//! Capability-scoped physics sessions and versioned project persistence.

use crate::{
    protocol::{
        ErrorCode, MAX_REQUEST_BYTES, MAX_REQUEST_ID_CHARS, PROTOCOL_VERSION, RequestEnvelope,
        ResponseEnvelope, Role, SUPPORTED_PROTOCOL_VERSIONS,
    },
    session::ApiError,
    storage,
};
use agentforge_physics::{
    episode::{Action, Episode, EpisodeError, Event, Observation, Outcome},
    replay::{ENGINE_VERSION, PHYSICS_COMPATIBILITY, Replay, ReplayError},
    scene::{PhysicsEdit, PhysicsEditEffect, PhysicsScene, PhysicsSceneDocument},
};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

const PROJECT_FORMAT: &str = "agentforge.physics.project";
const MAX_SNAPSHOTS: usize = 32;
const MAX_BATCH_EDITS: usize = 256;

type ApiResult<T> = Result<T, ApiError>;

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProjectDocument {
    format: String,
    format_version: u32,
    engine_version: String,
    scene: PhysicsSceneDocument,
}

fn header(value: &Value, format: &str, replay: bool) -> ApiResult<()> {
    if value.get("format").and_then(Value::as_str) != Some(format)
        || value.get("format_version").and_then(Value::as_u64) != Some(1)
        || value.get("engine_version").and_then(Value::as_str) != Some(ENGINE_VERSION)
        || (replay
            && value.get("physics_version").and_then(Value::as_str) != Some(PHYSICS_COMPATIBILITY))
    {
        return Err(ApiError::new(
            ErrorCode::IncompatibleVersion,
            "unsupported physics format, version, or engine",
        ));
    }
    Ok(())
}

fn read_value(path: &Path) -> ApiResult<Value> {
    let file = File::open(path).map_err(storage_error)?;
    let mut bytes = Vec::new();
    file.take(storage::MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(storage_error)?;
    if bytes.len() as u64 > storage::MAX_FILE_BYTES {
        return Err(ApiError::new(
            ErrorCode::LimitExceeded,
            "physics file exceeds file size limit",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| ApiError::new(ErrorCode::InvalidArguments, e.to_string()))
}

fn read_project(path: &Path) -> ApiResult<PhysicsSceneDocument> {
    let value = read_value(path)?;
    header(&value, PROJECT_FORMAT, false)?;
    let project: ProjectDocument = serde_json::from_value(value)
        .map_err(|e| ApiError::new(ErrorCode::InvalidProjectFile, e.to_string()))?;
    PhysicsScene::from_document(project.scene.clone())
        .map_err(|e| ApiError::new(ErrorCode::InvalidProjectFile, e.to_string()))?;
    Ok(project.scene)
}

/// Loads a bounded, strictly versioned physics project and validates its scene.
pub fn load_project(path: &Path) -> Result<PhysicsSceneDocument, String> {
    read_project(path).map_err(|e| e.into_body().message)
}

fn write_project(path: &Path, scene: &PhysicsSceneDocument) -> ApiResult<()> {
    PhysicsScene::from_document(scene.clone()).map_err(invalid)?;
    storage::write_json(
        path,
        &ProjectDocument {
            format: PROJECT_FORMAT.into(),
            format_version: 1,
            engine_version: ENGINE_VERSION.into(),
            scene: scene.clone(),
        },
    )
    .map_err(storage_error)?;
    Ok(())
}

/// Validates and atomically saves a versioned physics project.
pub fn save_project(path: &Path, scene: &PhysicsSceneDocument) -> Result<(), String> {
    write_project(path, scene).map_err(|e| e.into_body().message)
}

macro_rules! input {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Serialize, Deserialize, JsonSchema)]
        #[serde(deny_unknown_fields)]
        struct $name { $($field: $ty),* }
    };
}
macro_rules! output {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Serialize, JsonSchema)]
        struct $name { $($field: $ty),* }
    };
}
input!(NoArgs {});
input!(SetArgs {
    document: PhysicsSceneDocument,
    expected_revision: u64
});
input!(BatchArgs { edits: Vec<PhysicsEdit>, expected_revision: u64 });
input!(PathArgs {
    path: String,
    expected_revision: u64
});
input!(ActArgs {
    action: Action,
    expected_episode_id: u64,
    expected_tick: u32
});
input!(ExportArgs { path: Option<String> });
// Replay remains raw until its compatibility header has been checked.
input!(VerifyArgs { replay: Value });
input!(ReplayPathArgs { path: String });
input!(RestoreArgs {
    snapshot_id: u64,
    expected_revision: u64
});
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TypedVerifyArgs {
    replay: Replay,
}
output!(SceneResult {
    document: PhysicsSceneDocument
});
output!(MutationResult { revision: u64 });
output!(BatchResult { revision: u64, effects: Vec<PhysicsEditEffect> });
output!(SavedResult {
    revision: u64,
    path: String
});
output!(ValidationResult { valid: bool, playable: bool, issues: Vec<String> });
output!(SvgResult { svg: String });
output!(EpisodeResult {
    episode_id: u64,
    observation: Observation
});
output!(ActResult {
    episode_id: u64,
    tick: u32,
    outcome: Outcome,
    state_hash: agentforge_core::hash::StateHash,
    events: Vec<Event>,
    observation: Observation
});
output!(EventsResult { episode_id: u64, tick: u32, events: Vec<Event> });
output!(ExportResult { episode_id: u64, replay: Replay, path: Option<String> });
output!(SnapshotResult {
    snapshot_id: u64,
    captured_at_revision: u64
});
output!(RestoredResult { revision: u64, episode_id: Option<u64> });
output!(EpisodeStatus {
    episode_id: u64,
    tick: u32,
    outcome: Outcome
});
output!(StatusResult { role: Role, revision: u64, has_scene: bool, path: Option<String>, episode: Option<EpisodeStatus>, snapshots: usize });
/// One physics operation as published by `discover` and the generated schema.
#[derive(Serialize, JsonSchema)]
pub struct OperationInfo {
    /// Wire name, for example `scene.set`.
    pub name: String,
    /// One-line description.
    pub summary: String,
    /// Roles allowed to call it.
    pub roles: Vec<Role>,
    /// Standalone JSON Schema of the arguments.
    pub input_schema: Value,
    /// Standalone JSON Schema of the result.
    pub output_schema: Value,
    /// An example arguments object.
    pub example_arguments: Value,
    /// The example's expected result.
    pub example_result: Value,
}
output!(Discovery { protocol_version: u32, engine_version: String, physics_version: String, conventions: Vec<String>, max_bodies: usize, max_ticks: u32, max_request_bytes: usize, max_snapshots: usize, max_batch_edits: usize, operations: Vec<OperationInfo> });

struct ActiveEpisode {
    id: u64,
    episode: Episode,
}
struct Snapshot {
    scene: Option<PhysicsScene>,
    path: Option<PathBuf>,
    replay: Option<Replay>,
}

/// Authoritative physics session with a fixed capability and monotonic revision.
pub struct PhysicsHost {
    role: Role,
    scene: Option<PhysicsScene>,
    path: Option<PathBuf>,
    revision: u64,
    episode: Option<ActiveEpisode>,
    next_episode_id: u64,
    snapshots: BTreeMap<u64, Snapshot>,
    next_snapshot_id: u64,
}

impl PhysicsHost {
    /// Creates an author session, optionally with a validated scene (revision 1).
    pub fn author(scene: Option<PhysicsSceneDocument>) -> Result<Self, String> {
        let scene = scene
            .map(PhysicsScene::from_document)
            .transpose()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            role: Role::Author,
            revision: u64::from(scene.is_some()),
            scene,
            path: None,
            episode: None,
            next_episode_id: 1,
            snapshots: BTreeMap::new(),
            next_snapshot_id: 1,
        })
    }

    /// Creates an author session with the source path retained as snapshot metadata.
    pub fn author_with_project(scene: PhysicsSceneDocument, path: PathBuf) -> Result<Self, String> {
        let mut host = Self::author(Some(scene))?;
        host.path = Some(path);
        Ok(host)
    }

    /// Creates a player session bound to the supplied validated scene.
    pub fn player(scene: PhysicsSceneDocument) -> Result<Self, String> {
        let mut host = Self::author(Some(scene))?;
        host.role = Role::Player;
        Ok(host)
    }

    /// Current authoring revision. Episode ticks do not increment it.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Capability fixed when the session was created.
    pub fn role(&self) -> Role {
        self.role
    }

    /// Handles a bounded JSON request, preserving correlation and protocol checks.
    pub fn handle_line(&mut self, line: &str) -> ResponseEnvelope {
        let mut id = None;
        let mut operation = None;
        let result = (|| {
            if line.len() > MAX_REQUEST_BYTES {
                return Err(ApiError::new(
                    ErrorCode::RequestTooLarge,
                    "request exceeds size limit",
                ));
            }
            let value: Value = serde_json::from_str(line)
                .map_err(|e| ApiError::new(ErrorCode::MalformedJson, e.to_string()))?;
            id = value
                .get("request_id")
                .and_then(Value::as_str)
                .filter(|id| valid_id(id))
                .map(str::to_owned);
            operation = value
                .get("operation")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let request: RequestEnvelope = serde_json::from_value(value)
                .map_err(|e| ApiError::new(ErrorCode::InvalidEnvelope, e.to_string()))?;
            if !valid_id(&request.request_id) {
                return Err(ApiError::new(
                    ErrorCode::InvalidEnvelope,
                    "request_id must be 1-128 characters",
                ));
            }
            if !SUPPORTED_PROTOCOL_VERSIONS.contains(&request.protocol_version) {
                return Err(ApiError::new(
                    ErrorCode::UnsupportedProtocolVersion,
                    "unsupported protocol version",
                )
                .with("supported", SUPPORTED_PROTOCOL_VERSIONS));
            }
            let author = author_only(&request.operation).ok_or_else(|| {
                ApiError::new(ErrorCode::UnknownOperation, "unknown physics operation")
            })?;
            if author && self.role == Role::Player {
                return Err(ApiError::new(
                    ErrorCode::PermissionDenied,
                    "operation requires an author session",
                ));
            }
            self.dispatch(&request.operation, request.arguments)
        })();
        match result {
            Ok(value) => ResponseEnvelope::ok(id, operation, self.revision, value),
            Err(error) => {
                let mut body = error.into_body();
                if body.hint.is_none() {
                    body.hint = match body.code {
                        ErrorCode::StaleRevision => Some("Read session.status and scene.get, then retry with the current revision if the edit still applies."),
                        ErrorCode::StaleTick | ErrorCode::StaleEpisode => Some("Call episode.observe; a previous request or a reset/restore may already have changed the episode. Do not blindly resend the action."),
                        ErrorCode::PermissionDenied => Some("The role is fixed at process launch. Author operations require a trusted physics serve --role author process."),
                        ErrorCode::UnknownOperation | ErrorCode::InvalidArguments => Some("Call discover for the supported operations, input schemas, constraints and examples."),
                        ErrorCode::UnsupportedProtocolVersion => Some("Use protocol_version 1."),
                        ErrorCode::NoActiveEpisode => Some("Call episode.start first."),
                        ErrorCode::EpisodeFinished => Some("Call episode.reset or episode.start to obtain a fresh episode ID."),
                        _ => None,
                    }.map(str::to_owned);
                }
                ResponseEnvelope::error(id, operation, self.revision, body)
            }
        }
    }

    fn scene(&self) -> ApiResult<&PhysicsScene> {
        self.scene
            .as_ref()
            .ok_or_else(|| ApiError::new(ErrorCode::NoProjectOpen, "no physics scene is open"))
    }
    fn active(&self) -> ApiResult<&ActiveEpisode> {
        self.episode.as_ref().ok_or_else(no_episode)
    }
    fn check_revision(&self, expected: u64) -> ApiResult<u64> {
        if expected != self.revision {
            return Err(ApiError::new(
                ErrorCode::StaleRevision,
                "expected_revision does not match",
            )
            .with("expected_revision", expected)
            .with("current_revision", self.revision));
        }
        self.revision.checked_add(1).ok_or_else(limit)
    }
    fn editable(&self) -> ApiResult<()> {
        if self
            .episode
            .as_ref()
            .is_some_and(|e| e.episode.outcome() == Outcome::InProgress)
        {
            return Err(invalid(
                "scene edits are frozen during a running episode; finish or forfeit it first",
            ));
        }
        Ok(())
    }
    fn begin(&mut self, episode: Episode) -> ApiResult<Value> {
        let id = self.next_episode_id;
        let next = id.checked_add(1).ok_or_else(limit)?;
        let result = encode(EpisodeResult {
            episode_id: id,
            observation: episode.observe(),
        })?;
        self.episode = Some(ActiveEpisode { id, episode });
        self.next_episode_id = next;
        Ok(result)
    }
    fn dispatch(&mut self, op: &str, args: Value) -> ApiResult<Value> {
        match op {
            "discover" => {
                let _: NoArgs = decode(args)?;
                encode(discovery(self.role))
            }
            "session.status" => {
                let _: NoArgs = decode(args)?;
                encode(StatusResult {
                    role: self.role,
                    revision: self.revision,
                    has_scene: self.scene.is_some(),
                    path: if self.role == Role::Author {
                        self.path.as_ref().map(|p| p.display().to_string())
                    } else {
                        None
                    },
                    episode: self.episode.as_ref().map(|e| EpisodeStatus {
                        episode_id: e.id,
                        tick: e.episode.tick(),
                        outcome: e.episode.outcome(),
                    }),
                    snapshots: self.snapshots.len(),
                })
            }
            "scene.set" => {
                let a: SetArgs = decode(args)?;
                let revision = self.check_revision(a.expected_revision)?;
                self.editable()?;
                let scene = PhysicsScene::from_document(a.document).map_err(invalid)?;
                let result = encode(MutationResult { revision })?;
                self.scene = Some(scene);
                self.path = None;
                self.episode = None;
                self.revision = revision;
                Ok(result)
            }
            "scene.get" => {
                let _: NoArgs = decode(args)?;
                encode(SceneResult {
                    document: self.scene()?.to_document(),
                })
            }
            "scene.apply_batch" => {
                let a: BatchArgs = decode(args)?;
                let revision = self.check_revision(a.expected_revision)?;
                self.editable()?;
                if a.edits.is_empty() || a.edits.len() > MAX_BATCH_EDITS {
                    return Err(invalid("edits must contain 1-256 entries"));
                }
                let (scene, effects) = self
                    .scene()?
                    .apply_edits(&a.edits)
                    .map_err(|e| invalid(&e).with("edit_index", e.index))?;
                let result = encode(BatchResult { revision, effects })?;
                self.scene = Some(scene);
                self.episode = None;
                self.revision = revision;
                Ok(result)
            }
            "scene.validate" => {
                let _: NoArgs = decode(args)?;
                let settings = self.scene()?.settings();
                let mut issues = Vec::new();
                if settings.control.is_none() {
                    issues.push("missing control body".into());
                }
                if settings.objective.is_none() {
                    issues.push("missing objective zone".into());
                }
                encode(ValidationResult {
                    valid: true,
                    playable: issues.is_empty(),
                    issues,
                })
            }
            "scene.save" => {
                let a: PathArgs = decode(args)?;
                let revision = self.check_revision(a.expected_revision)?;
                let path = checked_path(&a.path)?;
                let result = encode(SavedResult {
                    revision,
                    path: a.path.clone(),
                })?;
                write_project(&path, &self.scene()?.to_document())?;
                self.path = Some(path);
                self.revision = revision;
                Ok(result)
            }
            "scene.load" => {
                let a: PathArgs = decode(args)?;
                let revision = self.check_revision(a.expected_revision)?;
                self.editable()?;
                let path = checked_path(&a.path)?;
                let scene = PhysicsScene::from_document(read_project(&path)?).map_err(invalid)?;
                let result = encode(MutationResult { revision })?;
                self.scene = Some(scene);
                self.path = Some(path);
                self.episode = None;
                self.revision = revision;
                Ok(result)
            }
            "scene.svg" => {
                let _: NoArgs = decode(args)?;
                encode(SvgResult {
                    svg: agentforge_physics::render::svg(&self.scene()?.to_document()),
                })
            }
            "episode.start" | "episode.reset" => {
                let _: NoArgs = decode(args)?;
                let scene = if op == "episode.reset" {
                    self.active()?.episode.scene()
                } else {
                    self.scene()?
                };
                let episode = Episode::start(scene).map_err(episode_error)?;
                self.begin(episode)
            }
            "episode.observe" => {
                let _: NoArgs = decode(args)?;
                let active = self.active()?;
                encode(EpisodeResult {
                    episode_id: active.id,
                    observation: active.episode.observe(),
                })
            }
            "episode.act" => {
                let a: ActArgs = decode(args)?;
                let active = self.episode.as_mut().ok_or_else(no_episode)?;
                if a.expected_episode_id != active.id {
                    return Err(ApiError::new(
                        ErrorCode::StaleEpisode,
                        "episode id does not match",
                    )
                    .with("active_episode_id", active.id));
                }
                if a.expected_tick != active.episode.tick() {
                    return Err(ApiError::new(ErrorCode::StaleTick, "tick does not match")
                        .with("current_tick", active.episode.tick()));
                }
                let step = active.episode.apply(a.action).map_err(episode_error)?;
                encode(ActResult {
                    episode_id: active.id,
                    tick: step.tick,
                    outcome: step.outcome,
                    state_hash: step.state_hash,
                    events: step.events,
                    observation: active.episode.observe(),
                })
            }
            "episode.events" => {
                let _: NoArgs = decode(args)?;
                let active = self.active()?;
                encode(EventsResult {
                    episode_id: active.id,
                    tick: active.episode.tick(),
                    events: active.episode.last_events().to_vec(),
                })
            }
            "replay.export" => {
                let a: ExportArgs = decode(args)?;
                if self.role == Role::Player && a.path.is_some() {
                    return Err(ApiError::new(
                        ErrorCode::PermissionDenied,
                        "players cannot choose output paths",
                    ));
                }
                let active = self.active()?;
                let replay = Replay::capture(&active.episode);
                let path = a.path.as_deref().map(checked_path).transpose()?;
                let result = encode(ExportResult {
                    episode_id: active.id,
                    replay: replay.clone(),
                    path: a.path,
                })?;
                if let Some(path) = path {
                    storage::write_json(&path, &replay).map_err(storage_error)?;
                }
                Ok(result)
            }
            "replay.verify" | "replay.verify_file" => {
                let value = if op == "replay.verify_file" {
                    let a: ReplayPathArgs = decode(args)?;
                    read_value(&checked_path(&a.path)?)?
                } else {
                    let a: VerifyArgs = decode(args)?;
                    a.replay
                };
                header(&value, "agentforge.physics.replay", true)?;
                let replay: Replay = serde_json::from_value(value)
                    .map_err(|e| ApiError::new(ErrorCode::InvalidReplay, e.to_string()))?;
                encode(replay.verify().map_err(replay_error)?)
            }
            "snapshot.capture" => {
                let _: NoArgs = decode(args)?;
                if self.snapshots.len() >= MAX_SNAPSHOTS {
                    return Err(limit());
                }
                let snapshot_id = self.next_snapshot_id;
                let next = snapshot_id.checked_add(1).ok_or_else(limit)?;
                let snapshot = Snapshot {
                    scene: self.scene.clone(),
                    path: self.path.clone(),
                    replay: self.episode.as_ref().map(|e| Replay::capture(&e.episode)),
                };
                let result = encode(SnapshotResult {
                    snapshot_id,
                    captured_at_revision: self.revision,
                })?;
                self.snapshots.insert(snapshot_id, snapshot);
                self.next_snapshot_id = next;
                Ok(result)
            }
            "snapshot.restore" => {
                let a: RestoreArgs = decode(args)?;
                let revision = self.check_revision(a.expected_revision)?;
                let snapshot = self.snapshots.get(&a.snapshot_id).ok_or_else(|| {
                    ApiError::new(ErrorCode::SnapshotNotFound, "unknown snapshot id")
                })?;
                let restored = snapshot
                    .replay
                    .as_ref()
                    .map(Replay::restore)
                    .transpose()
                    .map_err(replay_error)?;
                let next = if restored.is_some() {
                    self.next_episode_id.checked_add(1).ok_or_else(limit)?
                } else {
                    self.next_episode_id
                };
                let episode = restored.map(|episode| ActiveEpisode {
                    id: self.next_episode_id,
                    episode,
                });
                let result = encode(RestoredResult {
                    revision,
                    episode_id: episode.as_ref().map(|e| e.id),
                })?;
                self.scene = snapshot.scene.clone();
                self.path = snapshot.path.clone();
                self.episode = episode;
                self.next_episode_id = next;
                self.revision = revision;
                Ok(result)
            }
            _ => Err(ApiError::new(
                ErrorCode::UnknownOperation,
                "unknown physics operation",
            )),
        }
    }
}

fn valid_id(id: &str) -> bool {
    (1..=MAX_REQUEST_ID_CHARS).contains(&id.chars().count())
}
fn invalid(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::InvalidArguments, error.to_string())
}
fn storage_error(error: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::StorageError, error.to_string())
}
fn no_episode() -> ApiError {
    ApiError::new(ErrorCode::NoActiveEpisode, "start an episode first")
}
fn limit() -> ApiError {
    ApiError::new(ErrorCode::LimitExceeded, "session limit reached")
}
fn checked_path(path: &str) -> ApiResult<PathBuf> {
    if path.trim().is_empty() || path.contains('\0') {
        return Err(invalid("path must be nonempty and contain no NUL"));
    }
    Ok(PathBuf::from(path))
}
fn episode_error(error: EpisodeError) -> ApiError {
    let code = match error {
        EpisodeError::NotPlayable => ErrorCode::SceneNotPlayable,
        EpisodeError::Finished(_) => ErrorCode::EpisodeFinished,
        _ => ErrorCode::InvalidArguments,
    };
    ApiError::new(code, error.to_string())
}
fn replay_error(error: ReplayError) -> ApiError {
    let code = if matches!(error, ReplayError::Incompatible) {
        ErrorCode::IncompatibleVersion
    } else {
        ErrorCode::InvalidReplay
    };
    ApiError::new(code, error.to_string())
}
fn decode<T: DeserializeOwned>(value: Value) -> ApiResult<T> {
    serde_json::from_value(if value.is_null() { json!({}) } else { value }).map_err(invalid)
}
fn encode<T: Serialize>(value: T) -> ApiResult<Value> {
    serde_json::to_value(value).map_err(|e| ApiError::new(ErrorCode::InternalError, e.to_string()))
}

impl crate::server::Session for PhysicsHost {
    fn revision(&self) -> u64 {
        self.revision()
    }
    fn handle_line(&mut self, line: &str) -> ResponseEnvelope {
        self.handle_line(line)
    }
}

/// Every physics operation: name, author-only, summary. The order is documentation order.
const OPERATIONS: &[(&str, bool, &str)] = &[
    (
        "discover",
        false,
        "Describe the physics model, conventions, limits, operations and schemas this session may call.",
    ),
    (
        "session.status",
        false,
        "Report this session's role, revision, open scene, active episode and snapshots.",
    ),
    (
        "scene.set",
        true,
        "Replace the open scene with a validated document; the change is in memory until saved.",
    ),
    ("scene.get", true, "Return the open scene document."),
    (
        "scene.apply_batch",
        true,
        "Apply settings and body edits atomically: all succeed, or nothing changes.",
    ),
    (
        "scene.validate",
        true,
        "Check data and reference invariants and whether control and objective are configured.",
    ),
    (
        "scene.save",
        true,
        "Validate the scene and write it atomically as a versioned physics project.",
    ),
    (
        "scene.load",
        true,
        "Load and validate a versioned physics project without changing the session on failure.",
    ),
    (
        "scene.svg",
        true,
        "Render the initial body poses and goal sensor as SVG.",
    ),
    (
        "episode.start",
        false,
        "Start a fresh episode on the open scene.",
    ),
    (
        "episode.reset",
        false,
        "Restart the active episode from its initial scene.",
    ),
    (
        "episode.observe",
        false,
        "Return the current observation: body poses, velocities, contacts and outcome.",
    ),
    (
        "episode.act",
        false,
        "Apply one action and advance exactly one fixed timestep.",
    ),
    (
        "episode.events",
        false,
        "Return recent contact, overlap and outcome events.",
    ),
    (
        "replay.export",
        false,
        "Export the active episode recording inline and optionally to an author-chosen path.",
    ),
    (
        "replay.verify",
        true,
        "Re-execute an inline recording and compare every recorded state hash.",
    ),
    (
        "replay.verify_file",
        true,
        "Re-execute a recording from a bounded file and compare every recorded state hash.",
    ),
    (
        "snapshot.capture",
        true,
        "Capture the scene, save path and episode recording as an in-memory checkpoint.",
    ),
    (
        "snapshot.restore",
        true,
        "Rebuild a checkpoint by replaying its inputs and assign a fresh episode id.",
    ),
];

fn operation_entry(name: &str) -> Option<&'static (&'static str, bool, &'static str)> {
    OPERATIONS.iter().find(|(op, _, _)| *op == name)
}

fn author_only(name: &str) -> Option<bool> {
    operation_entry(name).map(|(_, author, _)| *author)
}

/// Operations advertised to `role`, in documentation order, with schemas and examples.
pub fn operations(role: Role) -> Vec<OperationInfo> {
    discovery(role).operations
}

fn example_scene() -> PhysicsSceneDocument {
    serde_json::from_value(json!({
        "settings": {
            "gravity": {"x":0.0,"y":0.0}, "ticks_per_second":60, "max_ticks":180,
            "bounds":{"min":{"x":-10.0,"y":-10.0},"max":{"x":10.0,"y":10.0}},
            "control":{"body":1,"max_impulse":5.0,"max_impulses":1},
            "objective":{"body":1,"zone":2}
        },
        "next_body_id":3,
        "bodies":[
            {"id":1,"kind":"dynamic","shape":{"type":"ball","radius":0.25},"position":{"x":0.0,"y":0.0}},
            {"id":2,"kind":"fixed","shape":{"type":"ball","radius":0.5},"position":{"x":4.0,"y":0.0},"sensor":true}
        ]
    })).expect("built-in schema example is a physics scene")
}

fn operation<A: JsonSchema + Serialize, R: JsonSchema + Serialize>(
    name: &str,
    args: A,
    result: R,
) -> OperationInfo {
    OperationInfo {
        name: name.into(),
        summary: operation_entry(name).map_or_else(String::new, |entry| entry.2.to_owned()),
        roles: if author_only(name) == Some(true) {
            vec![Role::Author]
        } else {
            vec![Role::Author, Role::Player]
        },
        input_schema: serde_json::to_value(schema_for!(A)).expect("schema serializes"),
        output_schema: serde_json::to_value(schema_for!(R)).expect("schema serializes"),
        example_arguments: serde_json::to_value(args).expect("example serializes"),
        example_result: serde_json::to_value(result).expect("example serializes"),
    }
}
fn conventions() -> Vec<String> {
    [
        "SI units: meters, seconds, kg, density kg/m2, impulse N.s, radians; Y points up.",
        "Fully observable. Labels are authoring notes and excluded from player observations/replays.",
        "Every accepted action advances exactly one fixed timestep; invalid actions change nothing.",
        "request_id only correlates replies. Use expected_revision on author writes and expected_episode_id plus expected_tick on actions; after a lost reply observe before retrying.",
        "Player capability is fixed by the launcher, bound to one saved scene; this is local separation, not remote authentication.",
        "Scene editing is frozen while this host has a running episode; finish or forfeit first.",
        "Goal overlaps are sampled at completed tick poses. Validation does not prove gameplay solvability.",
        "Snapshot restore replays recorded inputs to rebuild solver caches, takes O(recorded ticks), increments revision and assigns a fresh episode ID.",
        "Determinism is verified on macOS arm64 with the pinned toolchain/dependency set only; other platforms and versions are unverified.",
    ].into_iter().map(str::to_owned).collect()
}

fn discovery(role: Role) -> Discovery {
    let document = example_scene();
    let scene = PhysicsScene::from_document(document.clone()).expect("valid built-in scene");
    let mut episode = Episode::start(&scene).expect("playable built-in scene");
    let replay = Replay::capture(&episode);
    let mut operations = vec![
        operation(
            "discover",
            NoArgs {},
            Discovery {
                protocol_version: PROTOCOL_VERSION,
                engine_version: ENGINE_VERSION.into(),
                physics_version: PHYSICS_COMPATIBILITY.into(),
                conventions: conventions(),
                max_bodies: agentforge_physics::scene::MAX_BODIES,
                max_ticks: agentforge_physics::scene::MAX_PHYSICS_TICKS,
                max_request_bytes: MAX_REQUEST_BYTES,
                max_snapshots: MAX_SNAPSHOTS,
                max_batch_edits: MAX_BATCH_EDITS,
                operations: vec![],
            },
        ),
        operation(
            "session.status",
            NoArgs {},
            StatusResult {
                role,
                revision: 1,
                has_scene: true,
                path: None,
                episode: None,
                snapshots: 0,
            },
        ),
        operation(
            "scene.set",
            SetArgs {
                document: document.clone(),
                expected_revision: 1,
            },
            MutationResult { revision: 2 },
        ),
        operation(
            "scene.get",
            NoArgs {},
            SceneResult {
                document: document.clone(),
            },
        ),
        operation(
            "scene.apply_batch",
            BatchArgs {
                edits: vec![PhysicsEdit::UpdateSettings {
                    settings: agentforge_physics::scene::SettingsPatch {
                        max_ticks: Some(200),
                        ..Default::default()
                    },
                }],
                expected_revision: 1,
            },
            BatchResult {
                revision: 2,
                effects: vec![PhysicsEditEffect::SettingsUpdated {
                    before: *scene.settings(),
                    after: agentforge_physics::scene::PhysicsSettings {
                        max_ticks: 200,
                        ..*scene.settings()
                    },
                }],
            },
        ),
        operation(
            "scene.validate",
            NoArgs {},
            ValidationResult {
                valid: true,
                playable: true,
                issues: vec![],
            },
        ),
        operation(
            "scene.save",
            PathArgs {
                path: "scene.physics.json".into(),
                expected_revision: 1,
            },
            SavedResult {
                revision: 2,
                path: "scene.physics.json".into(),
            },
        ),
        operation(
            "scene.load",
            PathArgs {
                path: "scene.physics.json".into(),
                expected_revision: 1,
            },
            MutationResult { revision: 2 },
        ),
        operation(
            "scene.svg",
            NoArgs {},
            SvgResult {
                svg: agentforge_physics::render::svg(&document),
            },
        ),
        operation(
            "episode.start",
            NoArgs {},
            EpisodeResult {
                episode_id: 1,
                observation: episode.observe(),
            },
        ),
        operation(
            "episode.reset",
            NoArgs {},
            EpisodeResult {
                episode_id: 2,
                observation: episode.observe(),
            },
        ),
        operation(
            "episode.observe",
            NoArgs {},
            EpisodeResult {
                episode_id: 1,
                observation: episode.observe(),
            },
        ),
    ];
    let step = episode.apply(Action::Wait).expect("legal example action");
    operations.extend([
        operation(
            "episode.act",
            ActArgs {
                action: Action::Wait,
                expected_episode_id: 1,
                expected_tick: 0,
            },
            ActResult {
                episode_id: 1,
                tick: step.tick,
                outcome: step.outcome,
                state_hash: step.state_hash,
                events: step.events,
                observation: episode.observe(),
            },
        ),
        operation(
            "episode.events",
            NoArgs {},
            EventsResult {
                episode_id: 1,
                tick: 1,
                events: episode.last_events().to_vec(),
            },
        ),
        operation(
            "replay.export",
            ExportArgs { path: None },
            ExportResult {
                episode_id: 1,
                replay: replay.clone(),
                path: None,
            },
        ),
        operation(
            "replay.verify",
            TypedVerifyArgs {
                replay: replay.clone(),
            },
            replay.verify().expect("example verifies"),
        ),
        operation(
            "replay.verify_file",
            ReplayPathArgs {
                path: "episode.physics.replay.json".into(),
            },
            replay.verify().expect("example verifies"),
        ),
        operation(
            "snapshot.capture",
            NoArgs {},
            SnapshotResult {
                snapshot_id: 1,
                captured_at_revision: 1,
            },
        ),
        operation(
            "snapshot.restore",
            RestoreArgs {
                snapshot_id: 1,
                expected_revision: 1,
            },
            RestoredResult {
                revision: 2,
                episode_id: Some(2),
            },
        ),
    ]);
    operations.retain(|op| op.roles.contains(&role));
    Discovery {
        protocol_version: PROTOCOL_VERSION,
        engine_version: ENGINE_VERSION.into(),
        physics_version: PHYSICS_COMPATIBILITY.into(),
        conventions: conventions(),
        max_bodies: agentforge_physics::scene::MAX_BODIES,
        max_ticks: agentforge_physics::scene::MAX_PHYSICS_TICKS,
        max_request_bytes: MAX_REQUEST_BYTES,
        max_snapshots: MAX_SNAPSHOTS,
        max_batch_edits: MAX_BATCH_EDITS,
        operations,
    }
}

/// Typed, standalone input/output JSON schemas and examples for every physics operation.
/// Each schema carries its own definitions, so it can be used independently.
pub fn schemas() -> Value {
    json!({
        "protocol_version": PROTOCOL_VERSION,
        "request": schema_for!(RequestEnvelope),
        "response": schema_for!(ResponseEnvelope),
        "project": schema_for!(ProjectDocument),
        "operations": discovery(Role::Author).operations
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ResponseStatus;

    fn author() -> PhysicsHost {
        PhysicsHost::author(Some(example_scene())).expect("valid scene")
    }
    fn call(host: &mut PhysicsHost, operation: &str, arguments: Value) -> ResponseEnvelope {
        host.handle_line(&json!({"request_id":"test", "protocol_version":1, "operation":operation, "arguments":arguments}).to_string())
    }
    fn ok(host: &mut PhysicsHost, operation: &str, arguments: Value) -> Value {
        let response = call(host, operation, arguments);
        assert_eq!(
            response.status,
            ResponseStatus::Ok,
            "{operation}: {response:?}"
        );
        response.result.expect("success result")
    }
    fn error(host: &mut PhysicsHost, operation: &str, arguments: Value, code: ErrorCode) {
        let response = call(host, operation, arguments);
        assert_eq!(
            response.error.expect("error result").code,
            code,
            "{operation}"
        );
    }
    fn act(host: &mut PhysicsHost, id: u64, tick: u32, action: Action) -> Value {
        ok(
            host,
            "episode.act",
            json!({"action":action,"expected_episode_id":id,"expected_tick":tick}),
        )
    }

    #[test]
    fn permissions_unknown_operations_and_strict_inputs() {
        let mut host = PhysicsHost::player(example_scene()).expect("player");
        for &(name, author, _) in OPERATIONS {
            if author {
                error(&mut host, name, json!({}), ErrorCode::PermissionDenied);
            }
        }
        error(
            &mut host,
            "scene.teleport",
            json!({}),
            ErrorCode::UnknownOperation,
        );
        error(
            &mut host,
            "session.status",
            json!({"extra":1}),
            ErrorCode::InvalidArguments,
        );
        let discovery = ok(&mut host, "discover", json!({}));
        assert!(
            discovery["operations"]
                .as_array()
                .expect("ops")
                .iter()
                .all(|op| author_only(op["name"].as_str().expect("name")) == Some(false))
        );
        ok(&mut host, "episode.start", json!({}));
        error(
            &mut host,
            "replay.export",
            json!({"path":"forbidden.json"}),
            ErrorCode::PermissionDenied,
        );
        error(
            &mut host,
            "episode.act",
            json!({"action":{"type":"wait"},"expected_tick":0}),
            ErrorCode::InvalidArguments,
        );
        error(
            &mut host,
            "episode.act",
            json!({"action":{"type":"wait"},"expected_episode_id":1}),
            ErrorCode::InvalidArguments,
        );
    }

    #[test]
    fn envelope_version_correlation_and_size_limits() {
        let mut host = author();
        let response = host.handle_line(
            r#"{"request_id":"v","protocol_version":99,"operation":"scene.set","arguments":{}}"#,
        );
        assert_eq!(response.request_id.as_deref(), Some("v"));
        assert_eq!(response.operation.as_deref(), Some("scene.set"));
        assert_eq!(
            response.error.expect("error").code,
            ErrorCode::UnsupportedProtocolVersion
        );
        let response = host.handle_line(
            r#"{"request_id":"x","protocol_version":1,"operation":"session.status","extra":0}"#,
        );
        assert_eq!(response.request_id.as_deref(), Some("x"));
        assert_eq!(
            response.error.expect("error").code,
            ErrorCode::InvalidEnvelope
        );
        assert_eq!(
            host.handle_line("{").error.expect("error").code,
            ErrorCode::MalformedJson
        );
        assert_eq!(
            host.handle_line(&" ".repeat(MAX_REQUEST_BYTES + 1))
                .error
                .expect("error")
                .code,
            ErrorCode::RequestTooLarge
        );
        error(
            &mut host,
            "episode.start",
            json!({"unexpected":0}),
            ErrorCode::InvalidArguments,
        );
        assert_eq!(host.revision(), 1);
    }

    #[test]
    fn revisions_and_batches_commit_atomically_and_edits_freeze() {
        let mut host = author();
        let initial = ok(&mut host, "scene.get", json!({}));
        error(
            &mut host,
            "scene.apply_batch",
            json!({"expected_revision":1,"edits":[
                {"op":"update_settings","settings":{"max_ticks":200}},
                {"op":"remove_body","id":999}
            ]}),
            ErrorCode::InvalidArguments,
        );
        assert_eq!(ok(&mut host, "scene.get", json!({})), initial);
        assert_eq!(host.revision(), 1);
        ok(
            &mut host,
            "scene.apply_batch",
            json!({"expected_revision":1,"edits":[{"op":"update_settings","settings":{"max_ticks":200}}]}),
        );
        assert_eq!(host.revision(), 2);
        error(
            &mut host,
            "scene.set",
            json!({"expected_revision":1,"document":example_scene()}),
            ErrorCode::StaleRevision,
        );
        ok(&mut host, "episode.start", json!({}));
        error(
            &mut host,
            "scene.set",
            json!({"expected_revision":2,"document":example_scene()}),
            ErrorCode::InvalidArguments,
        );
        error(
            &mut host,
            "scene.load",
            json!({"expected_revision":2,"path":"missing.json"}),
            ErrorCode::InvalidArguments,
        );
        error(
            &mut host,
            "episode.act",
            json!({"expected_episode_id":9,"expected_tick":0,"action":{"type":"wait"}}),
            ErrorCode::StaleEpisode,
        );
        error(
            &mut host,
            "episode.act",
            json!({"expected_episode_id":1,"expected_tick":9,"action":{"type":"wait"}}),
            ErrorCode::StaleTick,
        );
        error(
            &mut host,
            "episode.act",
            json!({"expected_episode_id":1,"expected_tick":0,"action":{"type":"impulse","impulse":{"x":100.0,"y":0.0}}}),
            ErrorCode::InvalidArguments,
        );
        assert_eq!(
            ok(&mut host, "episode.observe", json!({}))["observation"]["tick"],
            0
        );
        assert_eq!(host.revision(), 2);
        act(&mut host, 1, 0, Action::Forfeit);
        ok(
            &mut host,
            "scene.set",
            json!({"expected_revision":2,"document":example_scene()}),
        );
        assert_eq!(host.revision(), 3);
    }

    #[test]
    fn strict_project_and_replay_headers_precede_typed_parsing() {
        let dir = tempfile::tempdir().expect("temp directory");
        let path = dir.path().join("project.json");
        let mut host = author();
        std::fs::write(
            &path,
            r#"{"format":"agentforge.physics.project","format_version":2,"scene":null}"#,
        )
        .expect("write");
        error(
            &mut host,
            "scene.load",
            json!({"path":path,"expected_revision":1}),
            ErrorCode::IncompatibleVersion,
        );
        assert!(load_project(&path).is_err());
        error(
            &mut host,
            "replay.verify",
            json!({"replay":{"format":"future","steps":null}}),
            ErrorCode::IncompatibleVersion,
        );
        save_project(&path, &example_scene()).expect("save");
        assert_eq!(load_project(&path).expect("load"), example_scene());
        let mut value: Value =
            serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
        value["extra"] = json!(true);
        std::fs::write(&path, value.to_string()).expect("write");
        error(
            &mut host,
            "scene.load",
            json!({"path":path,"expected_revision":1}),
            ErrorCode::InvalidProjectFile,
        );
        assert_eq!(host.revision(), 1);
        let missing = dir.path().join("missing").join("file.json");
        error(
            &mut host,
            "scene.save",
            json!({"path":missing,"expected_revision":1}),
            ErrorCode::StorageError,
        );
        assert_eq!(host.revision(), 1);
        assert!(host.path.is_none());
    }

    #[test]
    fn restore_preserves_author_scene_path_and_exact_solver_state() {
        let dir = tempfile::tempdir().expect("temp directory");
        let first = dir.path().join("first.json");
        let second = dir.path().join("second.json");
        let mut document = example_scene();
        document.bodies[0].label = Some("private author label".into());
        let mut host = PhysicsHost::author(Some(document.clone())).expect("author");
        ok(
            &mut host,
            "scene.save",
            json!({"path":first,"expected_revision":1}),
        );
        ok(&mut host, "episode.start", json!({}));
        for tick in 0..5 {
            act(&mut host, 1, tick, Action::Wait);
        }
        let captured = ok(&mut host, "episode.observe", json!({}));
        ok(&mut host, "snapshot.capture", json!({}));
        let expected_next = act(&mut host, 1, 5, Action::Wait);
        ok(
            &mut host,
            "scene.save",
            json!({"path":second,"expected_revision":2}),
        );
        ok(&mut host, "episode.reset", json!({}));
        let result = ok(
            &mut host,
            "snapshot.restore",
            json!({"snapshot_id":1,"expected_revision":3}),
        );
        assert_eq!(result["episode_id"], 3);
        assert_eq!(host.revision(), 4);
        assert_eq!(host.path.as_deref(), Some(first.as_path()));
        assert_eq!(host.scene().expect("scene").to_document(), document);
        let restored = ok(&mut host, "episode.observe", json!({}));
        assert_eq!(restored["observation"], captured["observation"]);
        assert_eq!(
            act(&mut host, 3, 5, Action::Wait)["state_hash"],
            expected_next["state_hash"]
        );
        error(
            &mut host,
            "episode.act",
            json!({"action":{"type":"wait"},"expected_episode_id":1,"expected_tick":6}),
            ErrorCode::StaleEpisode,
        );
        let replay = ok(&mut host, "replay.export", json!({}))["replay"].clone();
        assert!(!replay.to_string().contains("private author label"));
        assert_eq!(
            ok(&mut host, "replay.verify", json!({"replay":replay}))["verified"],
            true
        );
        error(
            &mut host,
            "snapshot.restore",
            json!({"snapshot_id":999,"expected_revision":4}),
            ErrorCode::SnapshotNotFound,
        );
        assert_eq!(host.revision(), 4);
    }

    #[test]
    fn empty_snapshots_and_snapshot_bound() {
        let mut host = PhysicsHost::author(None).expect("empty author");
        for _ in 0..MAX_SNAPSHOTS {
            ok(&mut host, "snapshot.capture", json!({}));
        }
        error(
            &mut host,
            "snapshot.capture",
            json!({}),
            ErrorCode::LimitExceeded,
        );
        ok(
            &mut host,
            "scene.set",
            json!({"expected_revision":0,"document":example_scene()}),
        );
        ok(
            &mut host,
            "snapshot.restore",
            json!({"snapshot_id":1,"expected_revision":1}),
        );
        assert_eq!(host.revision(), 2);
        assert!(host.scene.is_none());
        assert!(host.episode.is_none());
    }

    #[test]
    fn restore_rebuilds_contact_caches_and_latest_events() {
        let mut document: PhysicsSceneDocument =
            serde_json::from_str(include_str!("../../../fixtures/physics_drop.json"))
                .expect("drop fixture");
        // Keep the objective away from the falling ball so the ground contact persists.
        document.bodies[1].position.x = 8.0;
        let mut host = PhysicsHost::author(Some(document)).expect("author");
        ok(&mut host, "episode.start", json!({}));
        for tick in 0..75 {
            act(&mut host, 1, tick, Action::Wait);
        }
        let observation = ok(&mut host, "episode.observe", json!({}))["observation"].clone();
        assert!(
            !observation["contacts"]
                .as_array()
                .expect("contacts")
                .is_empty()
        );
        let events = ok(&mut host, "episode.events", json!({}))["events"].clone();
        ok(&mut host, "snapshot.capture", json!({}));
        let expected: Vec<Value> = (75..95)
            .map(|tick| act(&mut host, 1, tick, Action::Wait)["observation"].clone())
            .collect();
        ok(
            &mut host,
            "snapshot.restore",
            json!({"snapshot_id":1,"expected_revision":1}),
        );
        assert_eq!(
            ok(&mut host, "episode.observe", json!({}))["observation"],
            observation
        );
        assert_eq!(ok(&mut host, "episode.events", json!({}))["events"], events);
        for (tick, expected) in (75..95).zip(expected) {
            assert_eq!(
                act(&mut host, 2, tick, Action::Wait)["observation"],
                expected
            );
        }
    }

    #[test]
    fn bounded_transport_serves_physics_sessions() {
        let mut host = author();
        let mut output = Vec::new();
        let input = b"{\"request_id\":\"transport\",\"protocol_version\":1,\"operation\":\"session.status\"}\n";
        let stats =
            crate::server::serve(&mut host, &input[..], &mut output).expect("in-memory transport");
        assert_eq!(stats.requests, 1);
        assert_eq!(stats.errors, 0);
        let response: ResponseEnvelope = serde_json::from_slice(&output).expect("response");
        assert_eq!(response.request_id.as_deref(), Some("transport"));
        assert_eq!(response.revision, 1);
    }

    #[test]
    fn schemas_cover_exactly_supported_ops_with_typed_examples() {
        let value = schemas();
        let operations = value["operations"].as_array().expect("operations");
        assert_eq!(operations.len(), OPERATIONS.len());
        for op in operations {
            let name = op["name"].as_str().expect("name");
            assert!(author_only(name).is_some());
            assert!(op["input_schema"].is_object());
            assert!(op["output_schema"].is_object());
            assert!(op["example_result"].is_object());
            assert_eq!(op["input_schema"]["additionalProperties"], false);
            let mut host = author();
            let dir = tempfile::tempdir().expect("directory");
            let path = dir.path().join("example.json");
            save_project(&path, &example_scene()).expect("project");
            let mut args = op["example_arguments"].clone();
            if name == "scene.save" || name == "scene.load" {
                args["path"] = json!(path);
            }
            if name == "replay.verify_file" {
                let scene = PhysicsScene::from_document(example_scene()).expect("scene");
                let episode = Episode::start(&scene).expect("episode");
                storage::write_json(&path, &Replay::capture(&episode)).expect("replay");
                args["path"] = json!(path);
            }
            if name.starts_with("episode.") && name != "episode.start"
                || name == "replay.export"
                || name == "snapshot.restore"
            {
                ok(&mut host, "episode.start", json!({}));
            }
            if name == "snapshot.restore" {
                ok(&mut host, "snapshot.capture", json!({}));
            }
            ok(&mut host, name, args);
        }
    }
    #[test]
    fn long_recordings_verify_by_file_when_larger_than_request_limit() {
        let mut document: PhysicsSceneDocument =
            serde_json::from_str(include_str!("../../../fixtures/physics_launch.json"))
                .expect("fixture");
        document.settings.max_ticks = 9000;
        let mut host = PhysicsHost::author(Some(document)).expect("author");
        ok(&mut host, "episode.start", json!({}));
        for tick in 0..9000 {
            act(&mut host, 1, tick, Action::Wait);
        }
        let directory = tempfile::tempdir().expect("temp");
        let path = directory.path().join("long.replay.json");
        let replay = ok(&mut host, "replay.export", json!({"path":path}))["replay"].clone();
        let request=json!({"request_id":"large","protocol_version":1,"operation":"replay.verify","arguments":{"replay":replay}}).to_string();
        assert!(request.len() > MAX_REQUEST_BYTES);
        assert_eq!(
            host.handle_line(&request).error.expect("too large").code,
            ErrorCode::RequestTooLarge
        );
        let verified = ok(&mut host, "replay.verify_file", json!({"path":path}));
        assert_eq!(verified["verified"], true);
        assert_eq!(verified["steps_checked"], 9000);
    }
}
