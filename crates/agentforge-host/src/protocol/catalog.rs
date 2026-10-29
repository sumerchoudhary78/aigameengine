//! The operation catalog: names, permissions, examples and generated schemas.
//!
//! [`OpId`] is the single source of truth. The session dispatcher matches on it
//! exhaustively, `discover` and the published documentation are generated from it, and
//! tests check that every example parses into its operation's argument type, so code,
//! documentation and examples cannot drift apart.

use agentforge_core::analysis::{ReachabilityReport, ValidationReport};
use agentforge_core::entity::{EntityKind, KeyColor};
use agentforge_core::geometry::GridSize;
use agentforge_core::hash::STATE_HASH_VERSION;
use agentforge_core::layout;
use agentforge_core::project::MAX_NAME_CHARS;
use agentforge_core::rules::Rules;
use agentforge_core::scene::{MAX_BATCH_EDITS, MAX_LABEL_CHARS};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::ops::*;
use super::{
    ErrorCode, MAX_REQUEST_BYTES, MAX_REQUEST_ID_CHARS, PROTOCOL_VERSION, RequestEnvelope,
    ResponseEnvelope, Role, SUPPORTED_PROTOCOL_VERSIONS,
};

/// Most snapshots one session may hold.
pub const MAX_SNAPSHOTS: usize = 64;
/// Most events returned by one `episode.events` call.
pub const MAX_EVENTS_PER_QUERY: u32 = 1000;
/// Default `limit` of `episode.events`.
pub const DEFAULT_EVENTS_LIMIT: u32 = 100;
/// Most assertions per `episode.assert` call.
pub const MAX_ASSERTIONS: usize = 256;

const AUTHOR: &[Role] = &[Role::Author];
const BOTH: &[Role] = &[Role::Author, Role::Player];

/// Identifies an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpId {
    /// `discover`
    Discover,
    /// `session.status`
    SessionStatus,
    /// `project.create`
    ProjectCreate,
    /// `project.load`
    ProjectLoad,
    /// `project.save`
    ProjectSave,
    /// `scene.create`
    SceneCreate,
    /// `scene.get`
    SceneGet,
    /// `scene.validate`
    SceneValidate,
    /// `scene.apply_batch`
    SceneApplyBatch,
    /// `scene.update_rules`
    SceneUpdateRules,
    /// `scene.render`
    SceneRender,
    /// `entity.create`
    EntityCreate,
    /// `entity.get`
    EntityGet,
    /// `entity.update`
    EntityUpdate,
    /// `entity.remove`
    EntityRemove,
    /// `analysis.reachability`
    AnalysisReachability,
    /// `episode.start`
    EpisodeStart,
    /// `episode.reset`
    EpisodeReset,
    /// `episode.observe`
    EpisodeObserve,
    /// `episode.act`
    EpisodeAct,
    /// `episode.events`
    EpisodeEvents,
    /// `episode.assert`
    EpisodeAssert,
    /// `snapshot.capture`
    SnapshotCapture,
    /// `snapshot.restore`
    SnapshotRestore,
    /// `replay.export`
    ReplayExport,
    /// `replay.verify`
    ReplayVerify,
}

/// Static description of an operation.
#[derive(Clone, Copy)]
pub struct OperationSpec {
    /// Wire name.
    pub name: &'static str,
    /// One-line description.
    pub summary: &'static str,
    /// Roles allowed to call it.
    pub roles: &'static [Role],
    /// Whether an accepted call increments the session revision.
    pub mutates: bool,
    /// Example `arguments`.
    pub example_arguments: fn() -> Value,
    /// Schema of `arguments` as a subschema of a shared generator.
    pub arguments_schema: fn(&mut SchemaGenerator) -> Schema,
    /// Schema of `result` as a subschema of a shared generator.
    pub result_schema: fn(&mut SchemaGenerator) -> Schema,
    /// Standalone root schema of `arguments` (the type itself at the root).
    pub arguments_root: fn() -> Schema,
    /// Standalone root schema of `result`.
    pub result_root: fn() -> Schema,
    /// Checks that a value deserializes into the argument type.
    pub check_arguments: fn(Value) -> Result<(), serde_json::Error>,
}

fn schema_of<T: JsonSchema>(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<T>()
}

fn root_schema_of<T: JsonSchema>() -> Schema {
    SchemaGenerator::default().into_root_schema_for::<T>()
}

fn parses_as<T: DeserializeOwned>(value: Value) -> Result<(), serde_json::Error> {
    serde_json::from_value::<T>(value).map(|_| ())
}

macro_rules! spec {
    ($name:literal, $summary:literal, $roles:expr, $mutates:literal, $args:ty => $result:ty, $example:expr) => {
        OperationSpec {
            name: $name,
            summary: $summary,
            roles: $roles,
            mutates: $mutates,
            example_arguments: || $example,
            arguments_schema: schema_of::<$args>,
            result_schema: schema_of::<$result>,
            arguments_root: root_schema_of::<$args>,
            result_root: root_schema_of::<$result>,
            check_arguments: parses_as::<$args>,
        }
    };
}

impl OpId {
    /// Every operation, in documentation order.
    pub const ALL: [OpId; 26] = [
        OpId::Discover,
        OpId::SessionStatus,
        OpId::ProjectCreate,
        OpId::ProjectLoad,
        OpId::ProjectSave,
        OpId::SceneCreate,
        OpId::SceneGet,
        OpId::SceneValidate,
        OpId::SceneApplyBatch,
        OpId::SceneUpdateRules,
        OpId::SceneRender,
        OpId::EntityCreate,
        OpId::EntityGet,
        OpId::EntityUpdate,
        OpId::EntityRemove,
        OpId::AnalysisReachability,
        OpId::EpisodeStart,
        OpId::EpisodeReset,
        OpId::EpisodeObserve,
        OpId::EpisodeAct,
        OpId::EpisodeEvents,
        OpId::EpisodeAssert,
        OpId::SnapshotCapture,
        OpId::SnapshotRestore,
        OpId::ReplayExport,
        OpId::ReplayVerify,
    ];

    /// Looks an operation up by wire name.
    pub fn from_name(name: &str) -> Option<OpId> {
        Self::ALL.into_iter().find(|op| op.spec().name == name)
    }

    /// Wire name.
    pub fn name(self) -> &'static str {
        self.spec().name
    }

    /// Whether `role` may call this operation.
    pub fn allows(self, role: Role) -> bool {
        self.spec().roles.contains(&role)
    }

    /// A complete example request line for this operation.
    pub fn example_request(self) -> Value {
        let spec = self.spec();
        json!({
            "request_id": format!("example-{}", spec.name.replace('.', "-")),
            "protocol_version": PROTOCOL_VERSION,
            "operation": spec.name,
            "arguments": (spec.example_arguments)(),
        })
    }

    /// The operation's static description.
    pub fn spec(self) -> OperationSpec {
        match self {
            OpId::Discover => spec!(
                "discover",
                "Describe the engine: game rules, conventions, limits, the operations this session may call (with examples) and error codes. Pass `operation` for one operation's full schemas.",
                BOTH, false, DiscoverArgs => DiscoverResult,
                json!({"operation": "entity.create"})
            ),
            OpId::SessionStatus => spec!(
                "session.status",
                "Report this session's role, revision, open project, active episode and snapshots.",
                BOTH, false, NoArgs => SessionStatus,
                json!({})
            ),
            OpId::ProjectCreate => spec!(
                "project.create",
                "Open a new, empty in-memory project (replacing any open project; unsaved changes are discarded).",
                AUTHOR, true, ProjectCreateArgs => ProjectInfo,
                json!({"name": "demo", "expected_revision": 0})
            ),
            OpId::ProjectLoad => spec!(
                "project.load",
                "Open a project file. Invalid or incompatible files are rejected without changing the session.",
                AUTHOR, true, ProjectLoadArgs => ProjectInfo,
                json!({"path": "fixtures/key_vault.agentforge.json", "expected_revision": 0})
            ),
            OpId::ProjectSave => spec!(
                "project.save",
                "Write the open project to a file atomically (temporary file, fsync, rename).",
                AUTHOR, false, ProjectSaveArgs => ProjectSaved,
                json!({"path": "out/demo.agentforge.json", "expected_revision": 2})
            ),
            OpId::SceneCreate => spec!(
                "scene.create",
                "Create a scene from an ASCII layout or from width/height, with optional rules.",
                AUTHOR, true, SceneCreateArgs => SceneView,
                json!({
                    "name": "corridor",
                    "expected_revision": 1,
                    "layout": ["#######", "#@..#E#", "#######"],
                    "rules": {"max_ticks": 20, "win_condition": "reach_exit"}
                })
            ),
            OpId::SceneGet => spec!(
                "scene.get",
                "Return a scene with every entity (including labels), its rules, ASCII view and digest.",
                AUTHOR, false, SceneRef => SceneView,
                json!({"scene": "corridor"})
            ),
            OpId::SceneValidate => spec!(
                "scene.validate",
                "Check completeness (player, exit) and gameplay problems (unsolvable, too slow for max_ticks, unusable keys or doors).",
                AUTHOR, false, SceneRef => ValidationReport,
                json!({"scene": "corridor"})
            ),
            OpId::SceneApplyBatch => spec!(
                "scene.apply_batch",
                "Apply create/update/remove/update_rules edits atomically: all succeed, or the scene and revision stay unchanged and the failing edit index is reported.",
                AUTHOR, true, ApplyBatchArgs => BatchApplied,
                json!({
                    "scene": "corridor",
                    "expected_revision": 2,
                    "edits": [
                        {"op": "remove_entity", "id": 12},
                        {"op": "create_entity", "kind": {"type": "key", "color": "red"}, "position": {"x": 2, "y": 1}, "label": "bait"},
                        {"op": "update_rules", "max_ticks": 30}
                    ]
                })
            ),
            OpId::SceneUpdateRules => spec!(
                "scene.update_rules",
                "Change rule parameters (max_ticks, win_condition) of a scene.",
                AUTHOR, true, UpdateRulesArgs => RulesUpdated,
                json!({"scene": "corridor", "expected_revision": 3, "rules": {"win_condition": "collect_all_keys_then_exit"}})
            ),
            OpId::SceneRender => spec!(
                "scene.render",
                "Render a scene as SVG (entity ids labelled) or ASCII, inline or to a file. For human inspection; no GUI needed.",
                AUTHOR, false, RenderArgs => Rendered,
                json!({"scene": "corridor", "format": "svg", "path": "out/corridor.svg"})
            ),
            OpId::EntityCreate => spec!(
                "entity.create",
                "Create one entity; the engine assigns its id.",
                AUTHOR, true, EntityCreateArgs => EntityView,
                json!({"scene": "corridor", "expected_revision": 2, "kind": {"type": "door", "color": "red"}, "position": {"x": 3, "y": 1}, "label": "vault door"})
            ),
            OpId::EntityGet => spec!(
                "entity.get",
                "Return one entity, including its authoring label.",
                AUTHOR, false, EntityRef => EntityView,
                json!({"scene": "corridor", "id": 12})
            ),
            OpId::EntityUpdate => spec!(
                "entity.update",
                "Change an entity's kind, position and/or label (empty label clears it).",
                AUTHOR, true, EntityUpdateArgs => EntityUpdated,
                json!({"scene": "corridor", "expected_revision": 3, "id": 12, "position": {"x": 2, "y": 1}})
            ),
            OpId::EntityRemove => spec!(
                "entity.remove",
                "Remove an entity. Its id is never reused.",
                AUTHOR, true, EntityRemoveArgs => EntityRemoved,
                json!({"scene": "corridor", "expected_revision": 4, "id": 12})
            ),
            OpId::AnalysisReachability => spec!(
                "analysis.reachability",
                "Answer whether a target (default: the win condition) is reachable from a start (default: the player spawn), separately for walls only (geometric) and under the full rules (gameplay); when it is not, list the blockers and verified single-removal repairs.",
                AUTHOR, false, ReachabilityArgs => ReachabilityReport,
                json!({"scene": "corridor"})
            ),
            OpId::EpisodeStart => spec!(
                "episode.start",
                "Start a new episode (replacing the active one) on a frozen copy of a scene; later scene edits do not affect it.",
                BOTH, false, EpisodeStartArgs => EpisodeStarted,
                json!({"scene": "corridor"})
            ),
            OpId::EpisodeReset => spec!(
                "episode.reset",
                "Start a new episode from the active episode's frozen starting scene.",
                BOTH, false, NoArgs => EpisodeStarted,
                json!({})
            ),
            OpId::EpisodeObserve => spec!(
                "episode.observe",
                "Return the player's observation of the active episode, including legal actions.",
                BOTH, false, NoArgs => EpisodeObserved,
                json!({})
            ),
            OpId::EpisodeAct => spec!(
                "episode.act",
                "Submit one action and advance exactly one tick. Rejected actions change nothing and explain why.",
                BOTH, false, ActArgs => ActResult,
                json!({"action": {"type": "move", "direction": "east"}, "expected_episode_id": 1, "expected_tick": 0})
            ),
            OpId::EpisodeEvents => spec!(
                "episode.events",
                "Return the most recent events of the active episode, optionally only after a tick.",
                BOTH, false, EventsArgs => EventsResult,
                json!({"since_tick": 0, "limit": 20})
            ),
            OpId::EpisodeAssert => spec!(
                "episode.assert",
                "Evaluate assertions (outcome, position, keys, tick bound, event occurrence) against the active episode.",
                BOTH, false, AssertArgs => AssertResult,
                json!({"assertions": [
                    {"type": "outcome_is", "status": "won"},
                    {"type": "tick_at_most", "tick": 20},
                    {"type": "event_occurred", "event": "key_collected"}
                ]})
            ),
            OpId::SnapshotCapture => spec!(
                "snapshot.capture",
                "Capture the open project and active episode (simulation state only) in memory.",
                AUTHOR, false, SnapshotCaptureArgs => SnapshotInfo,
                json!({"label": "before-repair"})
            ),
            OpId::SnapshotRestore => spec!(
                "snapshot.restore",
                "Restore a snapshot's project and episode. The revision still increases, so writes based on older revisions stay stale.",
                AUTHOR, true, SnapshotRestoreArgs => SnapshotRestored,
                json!({"snapshot_id": 1, "expected_revision": 5})
            ),
            OpId::ReplayExport => spec!(
                "replay.export",
                "Export the active episode's replay: initial scene, accepted actions and per-tick hashes. Authors may pass a path; players write to the launcher's replay directory or receive it inline.",
                BOTH, false, ReplayExportArgs => ReplayExported,
                json!({"path": "out/corridor.replay.json"})
            ),
            OpId::ReplayVerify => spec!(
                "replay.verify",
                "Re-execute a replay's recorded actions and compare every state hash and the outcome; optionally check it belongs to a scene of the open project.",
                AUTHOR, false, ReplayVerifyArgs => ReplayVerified,
                json!({"path": "out/corridor.replay.json", "scene": "corridor"})
            ),
        }
    }
}

/// A standalone schema document as JSON.
pub fn standalone_schema(schema: fn() -> Schema) -> Value {
    schema().to_value()
}

/// One JSON document describing the whole protocol: envelopes, every operation's argument
/// and result schemas, examples, and shared definitions.
pub fn protocol_schema() -> Value {
    let mut generator = SchemaGenerator::default();
    let request = generator.subschema_for::<RequestEnvelope>().to_value();
    let response = generator.subschema_for::<ResponseEnvelope>().to_value();
    let operations: serde_json::Map<String, Value> = OpId::ALL
        .into_iter()
        .map(|op| {
            let spec = op.spec();
            let description = json!({
                "summary": spec.summary,
                "roles": spec.roles,
                "mutates": spec.mutates,
                "arguments": (spec.arguments_schema)(&mut generator).to_value(),
                "result": (spec.result_schema)(&mut generator).to_value(),
                "example": op.example_request(),
            });
            (spec.name.to_owned(), description)
        })
        .collect();
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "AgentForge JSON Lines protocol",
        "protocol_version": PROTOCOL_VERSION,
        "request": request,
        "response": response,
        "operations": operations,
        "error_codes": ErrorCode::ALL.iter().map(|code| json!({"code": code, "meaning": code.meaning()})).collect::<Vec<_>>(),
        "$defs": generator.take_definitions(true),
    })
}

/// Markdown reference of every operation, generated from the catalog.
pub fn operations_markdown() -> String {
    let mut out = String::from(
        "# AgentForge operations\n\n<!-- Generated by `agentforge schema`; do not edit by hand. -->\n\n\
         Every request is one JSON line. Full argument and result schemas: `protocol.schema.json` \
         (or `discover` with `{\"operation\": \"<name>\"}`).\n\n\
         | Operation | Roles | Mutates | Summary |\n|---|---|---|---|\n",
    );
    for op in OpId::ALL {
        let spec = op.spec();
        let roles: Vec<&str> = spec.roles.iter().map(|role| role.as_str()).collect();
        out.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            spec.name,
            roles.join(", "),
            if spec.mutates { "yes" } else { "no" },
            spec.summary
        ));
    }
    out.push_str("\n## Examples\n");
    for op in OpId::ALL {
        let example = serde_json::to_string(&op.example_request()).unwrap_or_default();
        out.push_str(&format!(
            "\n### `{}`\n\n```json\n{example}\n```\n",
            op.name()
        ));
    }
    out.push_str("\n## Error codes\n\n| Code | Meaning |\n|---|---|\n");
    for code in ErrorCode::ALL {
        out.push_str(&format!("| `{}` | {} |\n", code.as_str(), code.meaning()));
    }
    out
}

/// Plain-language capabilities of a role.
pub fn role_capabilities(role: Role) -> String {
    match role {
        Role::Author => "Trusted authoring session: create/load/save projects, create and edit scenes and entities atomically, validate, analyse reachability, render, snapshot/restore, verify replays, and play episodes. The role was fixed by the launcher (`agentforge serve --role author`).".to_owned(),
        Role::Player => "Restricted player session bound by the launcher to one scene: start/reset episodes, observe, act, read events, evaluate assertions and export replays. Authoring, analysis, snapshots and arbitrary file access are unavailable, and no request can change the role.".to_owned(),
    }
}

/// The game model published by `discover`.
pub fn game_model() -> GameModel {
    let kinds = [
        (
            EntityKind::Player,
            "Spawn point of the single player. Passable after the player leaves it.",
        ),
        (EntityKind::Wall, "Impassable."),
        (
            EntityKind::Key {
                color: KeyColor::Red,
            },
            "Collected by stepping onto it; never consumed. At most one key per color per scene.",
        ),
        (
            EntityKind::Door {
                color: KeyColor::Red,
            },
            "Passable only while holding the key of the same color. Stays in place.",
        ),
        (
            EntityKind::Exit,
            "Stepping onto it wins if the win condition holds; otherwise nothing happens.",
        ),
    ];
    GameModel {
        summary: "Turn-based 2D integer grid. One player moves through walls, colored keys, matching doors and exits. Scenes and rule parameters are data; nothing requires recompiling.".to_owned(),
        observability: "Fully observable: player observations contain the complete board. There is no hidden information in this version, so no hidden-information protection is claimed. Observations exclude authoring labels, revisions and engine internals.".to_owned(),
        timing: "Logical ticks only. Each accepted episode.act advances exactly one tick; rejected actions advance nothing. Nothing else moves, so there is no pending simulation work between ticks. No wall clock, randomness or thread scheduling affects outcomes.".to_owned(),
        coordinates: "Integer cells. x grows east, y grows south; (0, 0) is the north-west corner. Grids are 1-64 cells per side.".to_owned(),
        entity_kinds: kinds
            .into_iter()
            .map(|(example, description)| EntityKindInfo {
                kind: example.name().to_owned(),
                example,
                description: description.to_owned(),
            })
            .collect(),
        key_colors: KeyColor::ALL.to_vec(),
        win_conditions: win_conditions(),
        failure_conditions: vec![
            NamedDescription {
                name: "max_ticks_reached".to_owned(),
                description: "The tick count reaches rules.max_ticks without a win (a win on that tick still counts).".to_owned(),
            },
            NamedDescription {
                name: "forfeited".to_owned(),
                description: "The player submits {\"type\":\"forfeit\"}.".to_owned(),
            },
        ],
        tick_resolution_order: vec![
            "1. Reject the action if the episode ended, or if a move leaves the grid, enters a wall, or enters a door whose key is not held. Rejection changes nothing.".to_owned(),
            "2. Advance the tick by one.".to_owned(),
            "3. Apply the action: move relocates the player and collects an uncollected key on the target; wait does nothing; forfeit fails the episode immediately.".to_owned(),
            "4. If the player stands on an exit and the win condition holds, the episode is won.".to_owned(),
            "5. Otherwise, if the tick reached max_ticks, the episode fails.".to_owned(),
        ],
        ascii_legend: layout::legend(),
    }
}

/// Protocol conventions published by `discover`.
pub fn conventions() -> Conventions {
    Conventions {
        framing: "One JSON request object per stdin line; exactly one JSON response line per request on stdout, in order. Logs go to stderr only. Blank lines are ignored. Malformed lines get a structured error and the session continues.".to_owned(),
        roles: "The launcher fixes the session role (author or player) when starting the process; requests cannot change it. This is local capability separation between processes, not remote authentication.".to_owned(),
        revisions: "Every accepted authoring mutation increments the session revision, and every response reports it. Mutations must pass expected_revision equal to the current revision or fail with stale_revision and no changes. Restoring a snapshot also increments the revision, so writes based on pre-restore revisions stay stale.".to_owned(),
        retries: "request_id is correlation only; the engine never deduplicates. A retried mutation whose first delivery succeeded fails with stale_revision (check state, do not blindly resend). For episode.act pass expected_episode_id and expected_tick: a duplicate then fails with stale_tick, and an action meant for an episode that was since reset, restarted or restored fails with stale_episode, instead of being applied twice or to the wrong episode. Episode ids are never reused.".to_owned(),
        determinism: "Same initial state plus same accepted actions yields the same canonical state hashes (SHA-256 of a documented byte encoding; see docs/ARCHITECTURE.md). Reproducibility is verified for this engine version and replay format; cross-version or cross-platform identity is not claimed beyond what replay verification checks.".to_owned(),
        episodes: "An episode plays a frozen copy of a scene taken at episode.start. Scene edits never affect a running episode; start or reset an episode to play the edited scene. One active episode per session.".to_owned(),
    }
}

/// Limits published by `discover`.
pub fn constraints() -> Constraints {
    Constraints {
        max_grid_width: GridSize::MAX_DIMENSION,
        max_grid_height: GridSize::MAX_DIMENSION,
        max_ticks_limit: Rules::MAX_TICKS_LIMIT,
        max_entities_per_cell: 1,
        max_players_per_scene: 1,
        max_keys_per_color: 1,
        key_color_count: 8,
        max_request_bytes: MAX_REQUEST_BYTES as u64,
        max_request_id_chars: MAX_REQUEST_ID_CHARS as u32,
        max_batch_edits: MAX_BATCH_EDITS as u32,
        max_label_chars: MAX_LABEL_CHARS as u32,
        max_name_chars: MAX_NAME_CHARS as u32,
        max_snapshots: MAX_SNAPSHOTS as u32,
        max_events_per_query: MAX_EVENTS_PER_QUERY,
        max_assertions: MAX_ASSERTIONS as u32,
        max_repair_candidates: agentforge_core::analysis::MAX_REPAIR_CANDIDATES as u32,
    }
}

/// Engine identity published by `discover`.
pub fn engine_info() -> EngineInfo {
    EngineInfo {
        name: "agentforge".to_owned(),
        version: agentforge_core::ENGINE_VERSION.to_owned(),
        state_hash_version: STATE_HASH_VERSION,
    }
}

/// Supported protocol versions as a vector.
pub fn supported_versions() -> Vec<u32> {
    SUPPORTED_PROTOCOL_VERSIONS.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_resolvable() {
        let mut names: Vec<&str> = OpId::ALL.iter().map(|op| op.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), OpId::ALL.len());
        for op in OpId::ALL {
            assert_eq!(OpId::from_name(op.name()), Some(op));
        }
        assert_eq!(OpId::from_name("entity.destroy"), None);
    }

    #[test]
    fn every_example_parses_as_its_arguments() {
        for op in OpId::ALL {
            let request = op.example_request();
            let envelope: RequestEnvelope =
                serde_json::from_value(request).expect("example envelope is valid");
            assert_eq!(envelope.operation, op.name());
            (op.spec().check_arguments)(envelope.arguments)
                .unwrap_or_else(|error| panic!("example of {} is invalid: {error}", op.name()));
        }
    }

    #[test]
    fn schemas_are_generated_for_every_operation() {
        let schema = protocol_schema();
        let operations = schema["operations"].as_object().expect("operations map");
        assert_eq!(operations.len(), OpId::ALL.len());
        for op in OpId::ALL {
            let standalone = standalone_schema(op.spec().arguments_root);
            assert_eq!(
                standalone["type"],
                "object",
                "{} arguments schema",
                op.name()
            );
        }
        // Argument types reject unknown fields, and the schema says so.
        let create = standalone_schema(OpId::EntityCreate.spec().arguments_root);
        let text = create.to_string();
        assert!(text.contains("\"additionalProperties\":false"), "{text}");
    }
}
