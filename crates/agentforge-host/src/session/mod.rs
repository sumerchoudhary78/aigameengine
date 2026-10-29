//! Sessions: the authoritative state of one client connection and its dispatcher.
//!
//! A [`Host`] is created by the launcher with a fixed [`Role`]. All state changes flow
//! through [`Host::handle_line`] → `dispatch` → one handler. Handlers validate and compute
//! first and commit last with infallible assignments, so a rejected request (or an engine
//! panic, which is caught and reported as `internal_error`) leaves the session unchanged.

mod authoring;
mod errors;
mod play;

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use agentforge_core::episode::Episode;
use agentforge_core::hash::scene_digest;
use agentforge_core::project::{Name, Project};
use agentforge_core::scene::Scene;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

pub use errors::ApiError;
use errors::{invalid_arguments, name_error, scene_not_found};

use crate::protocol::catalog::{self, OpId};
use crate::protocol::ops::{
    DiscoverArgs, DiscoverResult, EpisodeBrief, ErrorCodeInfo, OperationInfo, ProjectInfo,
    SessionStatus, SnapshotInfo,
};
use crate::protocol::{
    ErrorCode, MAX_REQUEST_ID_CHARS, PROTOCOL_VERSION, RequestEnvelope, ResponseEnvelope, Role,
    SUPPORTED_PROTOCOL_VERSIONS,
};

/// Why a session could not be created.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostInitError {
    /// The scene name is invalid.
    #[error(transparent)]
    InvalidName(#[from] agentforge_core::project::NameError),
    /// The project has no such scene.
    #[error("the project has no scene named {scene:?} (available: {available:?})")]
    SceneNotFound {
        /// Requested scene.
        scene: String,
        /// Scenes in the project.
        available: Vec<String>,
    },
}

/// The open project of a session.
#[derive(Debug, Clone)]
struct Workspace {
    project: Project,
    path: Option<PathBuf>,
    dirty: bool,
}

/// The active episode of a session.
#[derive(Debug, Clone)]
struct ActiveEpisode {
    id: u64,
    scene: Name,
    scene_revision: u64,
    episode: Episode,
}

/// A captured snapshot: simulation state only (project content and episode), plus the
/// revision it was taken at as metadata.
#[derive(Debug, Clone)]
struct Snapshot {
    id: u64,
    label: Option<String>,
    captured_at_revision: u64,
    project: Option<Project>,
    episode: Option<ActiveEpisode>,
}

/// One session's authoritative state.
#[derive(Debug)]
pub struct Host {
    role: Role,
    replay_dir: Option<PathBuf>,
    bound_scene: Option<Name>,
    revision: u64,
    workspace: Option<Workspace>,
    episode: Option<ActiveEpisode>,
    next_episode_id: u64,
    snapshots: BTreeMap<u64, Snapshot>,
    next_snapshot_id: u64,
}

impl Host {
    /// A trusted authoring session with no open project (revision 0).
    pub fn author() -> Self {
        Self {
            role: Role::Author,
            replay_dir: None,
            bound_scene: None,
            revision: 0,
            workspace: None,
            episode: None,
            next_episode_id: 1,
            snapshots: BTreeMap::new(),
            next_snapshot_id: 1,
        }
    }

    /// A trusted authoring session with `project` already open (revision 1).
    pub fn author_with_project(project: Project, path: Option<PathBuf>) -> Self {
        Self {
            revision: 1,
            workspace: Some(Workspace {
                project,
                path,
                dirty: false,
            }),
            ..Self::author()
        }
    }

    /// A restricted player session bound to one scene of `project`. Replays exported by
    /// the player are written into `replay_dir` (if given) under engine-chosen names.
    pub fn player(
        project: Project,
        scene: &str,
        replay_dir: Option<PathBuf>,
    ) -> Result<Self, HostInitError> {
        let name = Name::new(scene)?;
        if project.scene(&name).is_none() {
            return Err(HostInitError::SceneNotFound {
                scene: scene.to_owned(),
                available: project.scene_names().map(Name::to_string).collect(),
            });
        }
        Ok(Self {
            role: Role::Player,
            replay_dir,
            bound_scene: Some(name),
            ..Self::author_with_project(project, None)
        })
    }

    /// This session's role.
    pub fn role(&self) -> Role {
        self.role
    }

    /// Current revision.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Handles one request line and returns exactly one response. Never panics.
    pub fn handle_line(&mut self, line: &str) -> ResponseEnvelope {
        match serde_json::from_str::<Value>(line) {
            Ok(value) => self.handle_value(value),
            Err(error) => self.respond_error(
                None,
                None,
                ApiError::new(
                    ErrorCode::MalformedJson,
                    format!("request is not valid JSON: {error}"),
                )
                .with("line", error.line())
                .with("column", error.column())
                .hint("Send exactly one JSON object per line."),
            ),
        }
    }

    /// Handles one already-parsed request value.
    pub fn handle_value(&mut self, value: Value) -> ResponseEnvelope {
        let request_id = value
            .get("request_id")
            .and_then(Value::as_str)
            .filter(|id| valid_request_id(id))
            .map(str::to_owned);
        let operation = value
            .get("operation")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let envelope: RequestEnvelope = match serde_json::from_value(value) {
            Ok(envelope) => envelope,
            Err(error) => {
                return self.respond_error(
                    request_id,
                    operation,
                    ApiError::new(ErrorCode::InvalidEnvelope, format!("invalid request envelope: {error}"))
                        .hint("A request is {\"request_id\":\"1\",\"protocol_version\":1,\"operation\":\"discover\",\"arguments\":{}}."),
                );
            }
        };
        if !valid_request_id(&envelope.request_id) {
            return self.respond_error(
                None,
                operation,
                ApiError::new(
                    ErrorCode::InvalidEnvelope,
                    format!("request_id must be 1-{MAX_REQUEST_ID_CHARS} characters"),
                ),
            );
        }
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&envelope.protocol_version) {
            return self.respond_error(
                request_id,
                operation,
                ApiError::new(
                    ErrorCode::UnsupportedProtocolVersion,
                    format!(
                        "protocol_version {} is not supported",
                        envelope.protocol_version
                    ),
                )
                .with("requested", envelope.protocol_version)
                .with("supported", SUPPORTED_PROTOCOL_VERSIONS)
                .hint(format!("Use protocol_version {PROTOCOL_VERSION}.")),
            );
        }
        let Some(op) = OpId::from_name(&envelope.operation) else {
            let suggestion = closest_operation(&envelope.operation, self.role);
            let mut error = ApiError::new(
                ErrorCode::UnknownOperation,
                format!("unknown operation {:?}", envelope.operation),
            )
            .hint("Call discover to list the operations this session may use.");
            if let Some(name) = suggestion {
                error = error.with("did_you_mean", name);
            }
            return self.respond_error(request_id, operation, error);
        };
        if !op.allows(self.role) {
            return self.respond_error(
                request_id,
                operation,
                ApiError::new(
                    ErrorCode::PermissionDenied,
                    format!("a {} session may not call {}", self.role.as_str(), op.name()),
                )
                .with("role", self.role)
                .with("operation", op.name())
                .hint("The role is fixed by the launcher (agentforge serve --role ...) and cannot be changed by requests."),
            );
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| self.dispatch(op, envelope.arguments)));
        let result = match outcome {
            Ok(result) => result,
            Err(_) => Err(ApiError::new(
                ErrorCode::InternalError,
                format!(
                    "internal error while handling {}; the request had no effect",
                    op.name()
                ),
            )),
        };
        tracing::debug!(
            operation = op.name(),
            ok = result.is_ok(),
            revision = self.revision,
            "request handled"
        );
        match result {
            Ok(value) => ResponseEnvelope::ok(request_id, operation, self.revision, value),
            Err(error) => self.respond_error(request_id, operation, error),
        }
    }

    fn respond_error(
        &self,
        request_id: Option<String>,
        operation: Option<String>,
        error: ApiError,
    ) -> ResponseEnvelope {
        tracing::debug!(code = error.code().as_str(), "request rejected");
        ResponseEnvelope::error(request_id, operation, self.revision, error.into_body())
    }

    fn dispatch(&mut self, op: OpId, arguments: Value) -> Result<Value, ApiError> {
        match op {
            OpId::Discover => run(op, arguments, |a| self.discover(a)),
            OpId::SessionStatus => run(op, arguments, |_: crate::protocol::ops::NoArgs| {
                Ok(self.status())
            }),
            OpId::ProjectCreate => run(op, arguments, |a| self.project_create(a)),
            OpId::ProjectLoad => run(op, arguments, |a| self.project_load(a)),
            OpId::ProjectSave => run(op, arguments, |a| self.project_save(a)),
            OpId::SceneCreate => run(op, arguments, |a| self.scene_create(a)),
            OpId::SceneGet => run(op, arguments, |a| self.scene_get(a)),
            OpId::SceneValidate => run(op, arguments, |a| self.scene_validate(a)),
            OpId::SceneApplyBatch => run(op, arguments, |a| self.scene_apply_batch(a)),
            OpId::SceneUpdateRules => run(op, arguments, |a| self.scene_update_rules(a)),
            OpId::SceneRender => run(op, arguments, |a| self.scene_render(a)),
            OpId::EntityCreate => run(op, arguments, |a| self.entity_create(a)),
            OpId::EntityGet => run(op, arguments, |a| self.entity_get(a)),
            OpId::EntityUpdate => run(op, arguments, |a| self.entity_update(a)),
            OpId::EntityRemove => run(op, arguments, |a| self.entity_remove(a)),
            OpId::AnalysisReachability => run(op, arguments, |a| self.analysis_reachability(a)),
            OpId::EpisodeStart => run(op, arguments, |a| self.episode_start(a)),
            OpId::EpisodeReset => run(op, arguments, |_: crate::protocol::ops::NoArgs| {
                self.episode_reset()
            }),
            OpId::EpisodeObserve => run(op, arguments, |_: crate::protocol::ops::NoArgs| {
                self.episode_observe()
            }),
            OpId::EpisodeAct => run(op, arguments, |a| self.episode_act(a)),
            OpId::EpisodeEvents => run(op, arguments, |a| self.episode_events(a)),
            OpId::EpisodeAssert => run(op, arguments, |a| self.episode_assert(a)),
            OpId::SnapshotCapture => run(op, arguments, |a| self.snapshot_capture(a)),
            OpId::SnapshotRestore => run(op, arguments, |a| self.snapshot_restore(a)),
            OpId::ReplayExport => run(op, arguments, |a| self.replay_export(a)),
            OpId::ReplayVerify => run(op, arguments, |a| self.replay_verify(a)),
        }
    }

    // -- shared helpers ------------------------------------------------------------------

    fn check_revision(&self, expected: u64) -> Result<(), ApiError> {
        if expected == self.revision {
            return Ok(());
        }
        Err(ApiError::new(
            ErrorCode::StaleRevision,
            format!(
                "expected_revision {expected} does not match the current revision {}; nothing was changed",
                self.revision
            ),
        )
        .with("expected_revision", expected)
        .with("current_revision", self.revision)
        .hint("Re-read the state you depend on (session.status, scene.get), then retry with the current revision if your change still applies."))
    }

    fn workspace(&self) -> Result<&Workspace, ApiError> {
        self.workspace.as_ref().ok_or_else(|| {
            ApiError::new(ErrorCode::NoProjectOpen, "no project is open")
                .hint("Open one with project.create or project.load.")
        })
    }

    /// Resolves a scene name in the open project.
    fn scene(&self, scene: &str) -> Result<(Name, &Scene), ApiError> {
        let name = Name::new(scene).map_err(|e| name_error(e, "scene"))?;
        let workspace = self.workspace()?;
        match workspace.project.scene(&name) {
            Some(found) => Ok((name, found)),
            None => Err(scene_not_found(
                &name,
                workspace
                    .project
                    .scene_names()
                    .map(Name::to_string)
                    .collect(),
            )),
        }
    }

    fn active_episode(&self) -> Result<&ActiveEpisode, ApiError> {
        self.episode.as_ref().ok_or_else(no_active_episode)
    }

    fn project_info(workspace: &Workspace) -> ProjectInfo {
        ProjectInfo {
            name: workspace.project.name().to_string(),
            path: workspace.path.as_ref().map(|p| p.display().to_string()),
            dirty: workspace.dirty,
            scenes: workspace
                .project
                .scene_names()
                .map(Name::to_string)
                .collect(),
        }
    }

    fn snapshot_info(snapshot: &Snapshot) -> SnapshotInfo {
        SnapshotInfo {
            snapshot_id: snapshot.id,
            label: snapshot.label.clone(),
            captured_at_revision: snapshot.captured_at_revision,
            project: snapshot.project.as_ref().map(|p| p.name().to_string()),
            scene_digests: snapshot
                .project
                .iter()
                .flat_map(|project| {
                    project.scene_names().filter_map(move |name| {
                        project
                            .scene(name)
                            .map(|scene| (name.to_string(), scene_digest(scene)))
                    })
                })
                .collect(),
            episode: snapshot.episode.as_ref().map(episode_brief),
        }
    }

    // -- discover / status ---------------------------------------------------------------

    fn discover(&self, args: DiscoverArgs) -> Result<DiscoverResult, ApiError> {
        let operations: Vec<OpId> = match &args.operation {
            Some(name) => {
                let op = OpId::from_name(name).ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::UnknownOperation,
                        format!("unknown operation {name:?}"),
                    )
                })?;
                if !op.allows(self.role) {
                    return Err(ApiError::new(
                        ErrorCode::PermissionDenied,
                        format!("a {} session may not call {name}", self.role.as_str()),
                    ));
                }
                vec![op]
            }
            None => OpId::ALL
                .into_iter()
                .filter(|op| op.allows(self.role))
                .collect(),
        };
        let include_schemas = args.include_schemas || args.operation.is_some();
        Ok(DiscoverResult {
            engine: catalog::engine_info(),
            protocol_version: PROTOCOL_VERSION,
            supported_protocol_versions: catalog::supported_versions(),
            role: self.role,
            role_capabilities: catalog::role_capabilities(self.role),
            game: catalog::game_model(),
            conventions: catalog::conventions(),
            constraints: catalog::constraints(),
            operations: operations
                .into_iter()
                .map(|op| {
                    let spec = op.spec();
                    OperationInfo {
                        name: spec.name.to_owned(),
                        summary: spec.summary.to_owned(),
                        roles: spec.roles.to_vec(),
                        mutates: spec.mutates,
                        example: op.example_request(),
                        arguments_schema: include_schemas
                            .then(|| catalog::standalone_schema(spec.arguments_root)),
                        result_schema: include_schemas
                            .then(|| catalog::standalone_schema(spec.result_root)),
                    }
                })
                .collect(),
            error_codes: ErrorCode::ALL
                .iter()
                .map(|code| ErrorCodeInfo {
                    code: *code,
                    meaning: code.meaning().to_owned(),
                })
                .collect(),
        })
    }

    fn status(&self) -> SessionStatus {
        SessionStatus {
            role: self.role,
            revision: self.revision,
            bound_scene: self.bound_scene.as_ref().map(Name::to_string),
            project: self.workspace.as_ref().map(Self::project_info),
            episode: self.episode.as_ref().map(episode_brief),
            snapshots: self.snapshots.values().map(Self::snapshot_info).collect(),
        }
    }
}

fn episode_brief(active: &ActiveEpisode) -> EpisodeBrief {
    EpisodeBrief {
        episode_id: active.id,
        scene: active.scene.to_string(),
        scene_revision: active.scene_revision,
        tick: active.episode.tick(),
        outcome: active.episode.outcome(),
        state_hash: active.episode.current_hash(),
    }
}

fn no_active_episode() -> ApiError {
    ApiError::new(
        ErrorCode::NoActiveEpisode,
        "no episode has been started in this session",
    )
    .hint("Start one with episode.start.")
}

fn valid_request_id(id: &str) -> bool {
    let chars = id.chars().count();
    (1..=MAX_REQUEST_ID_CHARS).contains(&chars)
}

/// Parses arguments into the operation's type, runs the handler and serializes its result.
fn run<A: DeserializeOwned, R: Serialize>(
    op: OpId,
    arguments: Value,
    handler: impl FnOnce(A) -> Result<R, ApiError>,
) -> Result<Value, ApiError> {
    let arguments = if arguments.is_null() {
        Value::Object(Map::new())
    } else {
        arguments
    };
    let parsed: A = serde_json::from_value(arguments).map_err(|error| {
        invalid_arguments(format!("invalid arguments for {}: {error}", op.name()))
            .with("operation", op.name())
            .hint(format!(
                "Call discover with {{\"operation\":\"{}\"}} for the argument schema and an example.",
                op.name()
            ))
    })?;
    let result = handler(parsed)?;
    serde_json::to_value(result).map_err(|error| {
        ApiError::new(
            ErrorCode::InternalError,
            format!("could not serialize result: {error}"),
        )
    })
}

/// The allowed operation with the smallest edit distance to `name`, if reasonably close.
fn closest_operation(name: &str, role: Role) -> Option<&'static str> {
    OpId::ALL
        .into_iter()
        .filter(|op| op.allows(role))
        .map(|op| (edit_distance(name, op.name()), op.name()))
        .min()
        .filter(|(distance, _)| *distance <= 4)
        .map(|(_, candidate)| candidate)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != *cb);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_suggests_near_misses() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(
            closest_operation("entity.crate", Role::Author),
            Some("entity.create")
        );
        assert_eq!(closest_operation("entity.create", Role::Player), None);
        assert_eq!(
            closest_operation("completely.unrelated.name", Role::Author),
            None
        );
    }
}
