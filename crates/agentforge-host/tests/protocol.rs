//! Protocol-boundary tests: every request goes through `Host::handle_line` as a JSON
//! string, exactly as it would arrive on stdin. Nothing is mocked.

use std::path::{Path, PathBuf};

use agentforge_host::{Host, storage};
use serde_json::{Value, json};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(format!("{name}.agentforge.json"))
}

struct Session {
    host: Host,
    next: u64,
}

impl Session {
    fn author() -> Self {
        Self {
            host: Host::author(),
            next: 0,
        }
    }

    fn author_with(fixture_name: &str) -> Self {
        let mut session = Self::author();
        session.ok(
            "project.load",
            json!({"path": fixture(fixture_name), "expected_revision": 0}),
        );
        session
    }

    fn player(fixture_name: &str, scene: &str) -> Self {
        let project = storage::load_project(&fixture(fixture_name)).expect("fixture loads");
        Self {
            host: Host::player(project, scene, None).expect("scene exists"),
            next: 0,
        }
    }

    fn raw(&mut self, line: &str) -> Value {
        serde_json::to_value(self.host.handle_line(line)).expect("response serializes")
    }

    fn send(&mut self, operation: &str, arguments: Value) -> Value {
        self.next += 1;
        let request = json!({
            "request_id": format!("t{}", self.next),
            "protocol_version": 1,
            "operation": operation,
            "arguments": arguments,
        });
        let response = self.raw(&request.to_string());
        assert_eq!(response["request_id"], format!("t{}", self.next));
        response
    }

    fn ok(&mut self, operation: &str, arguments: Value) -> Value {
        let response = self.send(operation, arguments);
        assert_eq!(response["status"], "ok", "{operation} failed: {response:#}");
        response["result"].clone()
    }

    fn err(&mut self, operation: &str, arguments: Value) -> Value {
        let response = self.send(operation, arguments);
        assert_eq!(
            response["status"], "error",
            "{operation} unexpectedly succeeded: {response:#}"
        );
        response["error"].clone()
    }

    fn revision(&self) -> u64 {
        self.host.revision()
    }

    fn act(&mut self, action: Value) -> Value {
        self.ok(
            "episode.act",
            json!({"action": action, "include_observation": false}),
        )
    }

    fn moves(&mut self, directions: &[Value]) -> Vec<String> {
        directions
            .iter()
            .map(|direction| {
                let result = self.act(json!({"type": "move", "direction": direction}));
                result["state_hash"].as_str().expect("hash").to_owned()
            })
            .collect()
    }

    fn episode_brief(&mut self) -> Value {
        self.ok("session.status", json!({}))["episode"].clone()
    }

    fn scene_digest(&mut self, scene: &str) -> Value {
        self.ok("scene.get", json!({"scene": scene}))["digest"].clone()
    }
}

fn solution(session: &mut Session, scene: &str) -> Vec<Value> {
    let report = session.ok("analysis.reachability", json!({"scene": scene}));
    assert_eq!(report["verdict"], "solvable", "{report:#}");
    report["gameplay"]["moves"]
        .as_array()
        .expect("moves")
        .clone()
}

#[test]
fn identical_initial_state_and_inputs_yield_identical_hashes() {
    let run = || {
        let mut session = Session::author_with("key_vault");
        let moves = solution(&mut session, "key_vault");
        let started = session.ok("episode.start", json!({"scene": "key_vault"}));
        let mut hashes = vec![started["initial_hash"].as_str().expect("hash").to_owned()];
        hashes.extend(session.moves(&moves));
        let outcome = session.episode_brief()["outcome"].clone();
        (hashes, outcome)
    };
    let (first, outcome) = run();
    let (second, _) = run();
    assert_eq!(first, second);
    assert_eq!(outcome["status"], "won");
    assert_eq!(first.len(), 21, "initial state plus 20 moves");
}

#[test]
fn snapshot_advance_restore_and_repeat_reproduces_the_outcome() {
    let mut session = Session::author_with("key_vault");
    let moves = solution(&mut session, "key_vault");
    session.ok("episode.start", json!({"scene": "key_vault"}));
    session.moves(&moves[..7]);
    let snapshot = session.ok("snapshot.capture", json!({"label": "mid-run"}));
    assert_eq!(snapshot["episode"]["tick"], 7);

    let first_tail = session.moves(&moves[7..]);
    let first_outcome = session.episode_brief()["outcome"].clone();
    assert_eq!(first_outcome["status"], "won");

    let revision = session.revision();
    let restored = session.ok(
        "snapshot.restore",
        json!({"snapshot_id": snapshot["snapshot_id"], "expected_revision": revision}),
    );
    assert_eq!(
        restored["revision"],
        revision + 1,
        "restore moves the revision forward"
    );
    assert_eq!(session.episode_brief()["tick"], 7);

    let second_tail = session.moves(&moves[7..]);
    assert_eq!(first_tail, second_tail);
    assert_eq!(session.episode_brief()["outcome"], first_outcome);
}

#[test]
fn stale_revisions_are_rejected_including_after_snapshot_restore() {
    let mut session = Session::author();
    session.ok(
        "project.create",
        json!({"name": "demo", "expected_revision": 0}),
    );
    session.ok(
        "scene.create",
        json!({"name": "room", "expected_revision": 1, "layout": ["@..", "..E"]}),
    );
    let snapshot = session.ok("snapshot.capture", json!({}));
    assert_eq!(snapshot["captured_at_revision"], 2);
    session.ok(
        "entity.create",
        json!({"scene": "room", "expected_revision": 2, "kind": {"type": "wall"}, "position": {"x": 1, "y": 0}}),
    );
    assert_eq!(session.revision(), 3);

    let stale = session.err(
        "entity.create",
        json!({"scene": "room", "expected_revision": 2, "kind": {"type": "wall"}, "position": {"x": 2, "y": 0}}),
    );
    assert_eq!(stale["code"], "stale_revision");
    assert_eq!(stale["details"]["current_revision"], 3);

    session.ok(
        "snapshot.restore",
        json!({"snapshot_id": snapshot["snapshot_id"], "expected_revision": 3}),
    );
    assert_eq!(session.revision(), 4);
    let digest_after_restore = session.scene_digest("room");

    // Both the snapshot-era revision and the pre-restore revision are now stale.
    for old in [2, 3] {
        let error = session.err(
            "entity.remove",
            json!({"scene": "room", "expected_revision": old, "id": 1}),
        );
        assert_eq!(
            error["code"], "stale_revision",
            "revision {old} must stay stale"
        );
    }
    assert_eq!(
        session.scene_digest("room"),
        digest_after_restore,
        "stale writes change nothing"
    );
    assert_eq!(session.revision(), 4);
    session.ok(
        "entity.create",
        json!({"scene": "room", "expected_revision": 4, "kind": {"type": "wall"}, "position": {"x": 2, "y": 0}}),
    );
}

#[test]
fn failed_batches_leave_scene_revision_and_allocator_unchanged() {
    let mut session = Session::author_with("blocked_dungeon");
    let before = session.ok("scene.get", json!({"scene": "blocked_dungeon"}));
    let revision = session.revision();
    let error = session.err(
        "scene.apply_batch",
        json!({
            "scene": "blocked_dungeon",
            "expected_revision": revision,
            "edits": [
                {"op": "remove_entity", "id": 29},
                {"op": "create_entity", "kind": {"type": "key", "color": "red"}, "position": {"x": 2, "y": 1}},
                {"op": "create_entity", "kind": {"type": "wall"}, "position": {"x": 2, "y": 1}},
            ],
        }),
    );
    assert_eq!(error["code"], "cell_occupied");
    assert_eq!(error["details"]["edit_index"], 2);
    assert!(
        error["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("atomically"))
    );
    let after = session.ok("scene.get", json!({"scene": "blocked_dungeon"}));
    assert_eq!(after, before, "no edit of the failed batch may persist");
    assert_eq!(session.revision(), revision);

    // The same edits minus the collision are accepted together.
    let applied = session.ok(
        "scene.apply_batch",
        json!({
            "scene": "blocked_dungeon",
            "expected_revision": revision,
            "edits": [
                {"op": "remove_entity", "id": 29},
                {"op": "create_entity", "kind": {"type": "key", "color": "red"}, "position": {"x": 2, "y": 1}},
            ],
        }),
    );
    assert_eq!(applied["applied"], 2);
    assert_eq!(
        applied["effects"][1]["entity"]["id"],
        before["next_entity_id"]
    );
}

#[test]
fn invalid_actions_leave_the_episode_unchanged_and_explain_why() {
    let mut session = Session::author();
    session.ok(
        "project.create",
        json!({"name": "rules", "expected_revision": 0}),
    );
    session.ok(
        "scene.create",
        json!({"name": "door", "expected_revision": 1, "layout": ["#####", "#r@RE", "#####"]}),
    );
    session.ok("episode.start", json!({"scene": "door"}));
    let before = session.episode_brief();

    let locked = session.err(
        "episode.act",
        json!({"action": {"type": "move", "direction": "east"}}),
    );
    assert_eq!(locked["code"], "door_locked");
    assert_eq!(locked["details"]["required_key"], "red");
    assert!(
        locked["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("red key"))
    );

    let wall = session.err(
        "episode.act",
        json!({"action": {"type": "move", "direction": "north"}}),
    );
    assert_eq!(wall["code"], "move_blocked");
    assert_eq!(wall["details"]["blocker_kind"], "wall");
    let legal: Vec<Value> = wall["details"]["legal_actions"]
        .as_array()
        .expect("listed")
        .clone();
    assert!(legal.contains(&json!({"type": "move", "direction": "west"})));

    let stale = session.err(
        "episode.act",
        json!({"action": {"type": "move", "direction": "west"}, "expected_tick": 3}),
    );
    assert_eq!(stale["code"], "stale_tick");

    let typo = session.err(
        "episode.act",
        json!({"action": {"type": "move", "direction": "up"}}),
    );
    assert_eq!(typo["code"], "invalid_arguments");

    assert_eq!(
        session.episode_brief(),
        before,
        "rejected actions must not change state"
    );

    session.act(json!({"type": "move", "direction": "west"}));
    session.act(json!({"type": "move", "direction": "east"}));
    session.act(json!({"type": "move", "direction": "east"}));
    let won = session.act(json!({"type": "move", "direction": "east"}));
    assert_eq!(won["outcome"]["status"], "won");
    let finished = session.err("episode.act", json!({"action": {"type": "wait"}}));
    assert_eq!(finished["code"], "episode_finished");
}

#[test]
fn terrain_slides_portals_hazards_and_one_way_errors_cross_the_protocol() {
    let mut session = Session::author();
    session.ok(
        "project.create",
        json!({"name": "terrain", "expected_revision": 0}),
    );
    let rules = json!({"max_ticks": 10, "win_condition": "reach_exit"});
    session.ok(
        "scene.create",
        json!({
            "name": "portal",
            "expected_revision": session.revision(),
            "layout": ["@.0#E", "####0"],
            "rules": rules,
        }),
    );
    session.ok("episode.start", json!({"scene": "portal"}));
    assert_eq!(
        session.act(json!({"type": "move", "direction": "east"}))["tick"],
        1
    );
    let hop = session.act(json!({"type": "move", "direction": "east"}));
    assert_eq!(hop["tick"], 2);
    assert!(
        hop["events"]
            .as_array()
            .expect("events")
            .iter()
            .any(|event| {
                event["type"] == "portal_used"
                    && event["pair"] == 0
                    && event["to"] == json!({"x": 4, "y": 1})
            })
    );
    let win = session.act(json!({"type": "move", "direction": "north"}));
    assert_eq!(win["outcome"], json!({"status": "won", "tick": 3}));

    session.ok(
        "scene.create",
        json!({
            "name": "hazard",
            "expected_revision": session.revision(),
            "layout": ["@!E"],
            "rules": rules,
        }),
    );
    session.ok("episode.start", json!({"scene": "hazard"}));
    let lost = session.act(json!({"type": "move", "direction": "east"}));
    assert_eq!(
        lost["outcome"],
        json!({"status": "failed", "tick": 1, "reason": "hazard"})
    );

    session.ok(
        "scene.create",
        json!({
            "name": "one_way",
            "expected_revision": session.revision(),
            "layout": ["@<E"],
            "rules": rules,
        }),
    );
    session.ok("episode.start", json!({"scene": "one_way"}));
    let blocked = session.err(
        "episode.act",
        json!({"action": {"type": "move", "direction": "east"}}),
    );
    assert_eq!(blocked["code"], "one_way_blocked");
    assert_eq!(blocked["details"]["allowed_direction"], "west");
    assert_eq!(blocked["details"]["tick"], 0);
}

#[test]
fn player_sessions_cannot_author_restore_or_escape_their_scene() {
    let mut player = Session::player("key_vault", "key_vault");
    for (operation, arguments) in [
        (
            "entity.create",
            json!({"scene": "key_vault", "expected_revision": 1, "kind": {"type": "wall"}, "position": {"x": 1, "y": 1}}),
        ),
        (
            "scene.apply_batch",
            json!({"scene": "key_vault", "expected_revision": 1, "edits": [{"op": "update_rules", "max_ticks": 9999}]}),
        ),
        (
            "scene.update_rules",
            json!({"scene": "key_vault", "expected_revision": 1, "rules": {"max_ticks": 9999}}),
        ),
        ("scene.get", json!({"scene": "key_vault"})),
        ("analysis.reachability", json!({"scene": "key_vault"})),
        ("snapshot.capture", json!({})),
        (
            "snapshot.restore",
            json!({"snapshot_id": 1, "expected_revision": 1}),
        ),
        (
            "project.load",
            json!({"path": "anything.json", "expected_revision": 1}),
        ),
        ("project.save", json!({"path": "anything.json"})),
        ("replay.verify", json!({"path": "anything.json"})),
    ] {
        let error = player.err(operation, arguments);
        assert_eq!(error["code"], "permission_denied", "{operation}");
    }
    let revision = player.revision();

    let other_scene = player.err("episode.start", json!({"scene": "blocked_dungeon"}));
    assert_eq!(other_scene["code"], "permission_denied");
    player.ok("episode.start", json!({}));
    let path = player.err("replay.export", json!({"path": "/tmp/escape.json"}));
    assert_eq!(path["code"], "permission_denied");

    // A role claim inside the request is not a capability; it is an invalid envelope.
    let claim = player.raw(
        r#"{"request_id":"x","protocol_version":1,"operation":"entity.get","arguments":{"scene":"key_vault","id":1},"role":"author"}"#,
    );
    assert_eq!(claim["error"]["code"], "invalid_envelope");

    let discovered = player.ok("discover", json!({}));
    let names: Vec<&str> = discovered["operations"]
        .as_array()
        .expect("operations")
        .iter()
        .filter_map(|op| op["name"].as_str())
        .collect();
    assert!(names.contains(&"episode.act"));
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with("entity.") || name.starts_with("scene."))
    );
    assert_eq!(
        player.revision(),
        revision,
        "nothing a player did changed the world"
    );
}

#[test]
fn malformed_json_and_unsupported_versions_get_structured_errors() {
    let mut session = Session::author();
    let malformed = session.raw("{\"request_id\": \"1\", \"protocol_version\": 1,");
    assert_eq!(malformed["status"], "error");
    assert_eq!(malformed["error"]["code"], "malformed_json");
    assert!(malformed["request_id"].is_null());

    let version = session.raw(r#"{"request_id":"v2","protocol_version":2,"operation":"discover"}"#);
    assert_eq!(version["request_id"], "v2");
    assert_eq!(version["error"]["code"], "unsupported_protocol_version");
    assert_eq!(version["error"]["details"]["supported"], json!([1]));

    let not_object = session.raw("[1, 2, 3]");
    assert_eq!(not_object["error"]["code"], "invalid_envelope");

    let typo = session.raw(r#"{"request_id":"t","protocol_version":1,"operation":"entity.crate"}"#);
    assert_eq!(typo["error"]["code"], "unknown_operation");
    assert_eq!(typo["error"]["details"]["did_you_mean"], "entity.create");

    let field_typo = session.err(
        "project.create",
        json!({"name": "demo", "expected_revison": 0}),
    );
    assert_eq!(field_typo["code"], "invalid_arguments");
    assert!(
        field_typo["message"]
            .as_str()
            .is_some_and(|m| m.contains("expected_revison"))
    );

    // The session is still healthy and nothing changed.
    assert_eq!(session.revision(), 0);
    session.ok(
        "project.create",
        json!({"name": "demo", "expected_revision": 0}),
    );
}

#[test]
fn blocked_fixture_fails_public_repair_succeeds_and_replay_verifies() {
    let mut session = Session::author_with("blocked_dungeon");
    let validation = session.ok("scene.validate", json!({"scene": "blocked_dungeon"}));
    assert_eq!(validation["valid"], true);
    assert_eq!(validation["playable"], false);

    // A player that never reaches the exit fails when max_ticks runs out.
    session.ok("episode.start", json!({"scene": "blocked_dungeon"}));
    let mut last = Value::Null;
    for _ in 0..40 {
        last = session.act(json!({"type": "wait"}));
    }
    assert_eq!(
        last["outcome"],
        json!({"status": "failed", "tick": 40, "reason": "max_ticks_reached"})
    );

    let diagnosis = session.ok("analysis.reachability", json!({"scene": "blocked_dungeon"}));
    assert_eq!(diagnosis["verdict"], "unsolvable");
    assert_eq!(diagnosis["geometric"]["reachable"], false);
    let repairs = diagnosis["repairs"]["single_removals"]
        .as_array()
        .expect("repairs");
    assert_eq!(
        repairs.len(),
        1,
        "exactly one wall plugs the gap: {repairs:#?}"
    );
    assert_eq!(repairs[0]["position"], json!({"x": 6, "y": 3}));

    let revision = session.revision();
    session.ok(
        "entity.remove",
        json!({"scene": "blocked_dungeon", "expected_revision": revision, "id": repairs[0]["remove"]}),
    );
    let moves = solution(&mut session, "blocked_dungeon");
    assert_eq!(moves.len(), 14);
    session.ok("episode.start", json!({"scene": "blocked_dungeon"}));
    session.moves(&moves);
    let checks = session.ok(
        "episode.assert",
        json!({"assertions": [
            {"type": "outcome_is", "status": "won"},
            {"type": "player_at", "position": {"x": 11, "y": 5}},
            {"type": "tick_at_most", "tick": 40},
            {"type": "event_occurred", "event": "exit_reached"}
        ]}),
    );
    assert_eq!(checks["all_passed"], true, "{checks:#}");

    let exported = session.ok("replay.export", json!({}));
    let replay = exported["replay"].clone();
    let verified = session.ok(
        "replay.verify",
        json!({"replay": replay, "scene": "blocked_dungeon"}),
    );
    assert_eq!(verified["verified"], true);
    assert_eq!(verified["scene_matches"], true);
    assert_eq!(verified["steps_checked"], 14);

    let mut tampered = replay;
    tampered["steps"][3]["action"] = json!({"type": "wait"});
    let rejected = session.ok("replay.verify", json!({"replay": tampered}));
    assert_eq!(rejected["verified"], false);
    assert_eq!(rejected["divergence"]["kind"], "state_hash");
    assert_eq!(rejected["divergence"]["step_index"], 3);
}

#[test]
fn second_scene_with_keys_and_doors_works_through_the_same_commands() {
    let mut player = Session::player("key_vault", "key_vault");
    let started = player.ok("episode.start", json!({}));
    assert_eq!(
        started["observation"]["objective"]["keys_required"],
        json!(["red", "blue", "yellow"])
    );

    // The author's analysis provides the route; the player executes it via episode.act.
    let mut author = Session::author_with("key_vault");
    let moves = solution(&mut author, "key_vault");
    player.moves(&moves);
    let events = player.ok("episode.events", json!({"limit": 1000}));
    let types: Vec<&str> = events["events"]
        .as_array()
        .expect("events")
        .iter()
        .filter_map(|event| event["type"].as_str())
        .collect();
    assert_eq!(types.iter().filter(|t| **t == "key_collected").count(), 3);
    assert_eq!(types.iter().filter(|t| **t == "door_passed").count(), 2);
    assert_eq!(types.last(), Some(&"won"));
}

#[test]
fn scene_edits_never_affect_a_running_episode() {
    let mut session = Session::author();
    session.ok(
        "project.create",
        json!({"name": "play", "expected_revision": 0}),
    );
    session.ok(
        "scene.create",
        json!({"name": "hall", "expected_revision": 1, "layout": ["@#E"]}),
    );
    session.ok("episode.start", json!({"scene": "hall"}));
    session.ok(
        "entity.remove",
        json!({"scene": "hall", "expected_revision": 2, "id": 2}),
    );

    let blocked = session.err(
        "episode.act",
        json!({"action": {"type": "move", "direction": "east"}}),
    );
    assert_eq!(
        blocked["code"], "move_blocked",
        "the running episode keeps its frozen scene"
    );
    session.ok("episode.reset", json!({}));
    let still = session.err(
        "episode.act",
        json!({"action": {"type": "move", "direction": "east"}}),
    );
    assert_eq!(
        still["code"], "move_blocked",
        "reset replays the same frozen scene"
    );

    let fresh = session.ok("episode.start", json!({"scene": "hall"}));
    assert_eq!(fresh["scene_revision"], 3);
    session.act(json!({"type": "move", "direction": "east"}));
    let won = session.act(json!({"type": "move", "direction": "east"}));
    assert_eq!(won["outcome"]["status"], "won");
}

#[test]
fn save_and_load_preserve_the_simulation_outcome() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("saved.agentforge.json");
    let mut author = Session::author_with("key_vault");
    let moves = solution(&mut author, "key_vault");
    author.ok("project.save", json!({"path": path}));
    author.ok("episode.start", json!({"scene": "key_vault"}));
    let original = author.moves(&moves);

    let project = storage::load_project(&path).expect("saved project loads");
    let mut reloaded = Session {
        host: Host::player(project, "key_vault", None).expect("scene exists"),
        next: 0,
    };
    reloaded.ok("episode.start", json!({}));
    assert_eq!(reloaded.moves(&moves), original);
}

#[test]
fn invalid_project_files_are_rejected_without_changing_the_session() {
    let directory = tempfile::tempdir().expect("temp dir");
    let mut session = Session::author_with("key_vault");
    let revision = session.revision();
    let digest = session.scene_digest("key_vault");

    let mut document: Value = serde_json::from_str(
        &std::fs::read_to_string(fixture("key_vault")).expect("fixture readable"),
    )
    .expect("fixture is JSON");
    document["scenes"]["key_vault"]["entities"][0]["position"] = json!({"x": 99, "y": 0});
    let corrupt = directory.path().join("corrupt.json");
    std::fs::write(&corrupt, document.to_string()).expect("writable");
    let error = session.err(
        "project.load",
        json!({"path": corrupt, "expected_revision": revision}),
    );
    assert_eq!(error["code"], "invalid_project_file");

    document["format_version"] = json!(99);
    let future = directory.path().join("future.json");
    std::fs::write(&future, document.to_string()).expect("writable");
    let error = session.err(
        "project.load",
        json!({"path": future, "expected_revision": revision}),
    );
    assert_eq!(error["code"], "incompatible_version");

    assert_eq!(session.revision(), revision);
    assert_eq!(session.scene_digest("key_vault"), digest);
}

#[test]
fn discover_publishes_every_operation_with_examples_and_schemas() {
    let mut author = Session::author();
    let discovered = author.ok("discover", json!({}));
    let operations = discovered["operations"].as_array().expect("operations");
    assert_eq!(operations.len(), 26);
    for operation in operations {
        assert_eq!(operation["example"]["operation"], operation["name"]);
        assert!(
            operation.get("arguments_schema").is_none(),
            "schemas only on request"
        );
    }
    assert_eq!(
        discovered["game"]["observability"]
            .as_str()
            .map(|s| s.starts_with("Fully observable")),
        Some(true)
    );
    let kinds: Vec<&str> = discovered["game"]["entity_kinds"]
        .as_array()
        .expect("entity kinds")
        .iter()
        .map(|k| k["kind"].as_str().expect("kind name"))
        .collect();
    assert_eq!(
        kinds,
        vec![
            "player", "wall", "key", "door", "exit", "ice", "one_way", "hazard", "portal"
        ]
    );
    let failures: Vec<&str> = discovered["game"]["failure_conditions"]
        .as_array()
        .expect("failure conditions")
        .iter()
        .map(|f| f["name"].as_str().expect("failure name"))
        .collect();
    assert_eq!(failures, vec!["max_ticks_reached", "forfeited", "hazard"]);
    let glyphs = discovered["game"]["ascii_legend"]
        .as_array()
        .expect("legend")
        .iter()
        .map(|entry| entry["glyph"].as_str().expect("glyph"))
        .collect::<Vec<_>>();
    for glyph in ["~", "!", "^", ">", "v", "<", "0", "9"] {
        assert!(glyphs.contains(&glyph), "legend is missing {glyph}");
    }
    assert_eq!(discovered["constraints"]["max_portal_pairs"], json!(10));

    let one = author.ok("discover", json!({"operation": "scene.apply_batch"}));
    let schema = &one["operations"][0]["arguments_schema"];
    assert_eq!(
        schema["required"],
        json!(["scene", "expected_revision", "edits"])
    );
    assert!(one["operations"][0]["result_schema"].is_object());
}

#[test]
fn generated_protocol_docs_are_up_to_date() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/protocol");
    let schema_file = std::fs::read_to_string(directory.join("protocol.schema.json")).expect(
        "docs/protocol/protocol.schema.json exists; run `cargo run -p agentforge-cli -- schema`",
    );
    let committed: Value = serde_json::from_str(&schema_file).expect("valid JSON");
    assert_eq!(
        committed,
        agentforge_host::protocol::catalog::protocol_schema(),
        "protocol.schema.json is stale; run `cargo run -p agentforge-cli -- schema`"
    );
    let operations =
        std::fs::read_to_string(directory.join("operations.md")).expect("operations.md exists");
    assert_eq!(
        operations,
        agentforge_host::protocol::catalog::operations_markdown(),
        "operations.md is stale; run `cargo run -p agentforge-cli -- schema`"
    );
}

/// Regression: restoring a snapshot kept the current save path, so `project.save` could
/// write the restored project over a different project's file.
#[test]
fn restoring_a_snapshot_never_redirects_saves_to_another_projects_file() {
    let directory = tempfile::tempdir().expect("temp dir");
    let alpha_path = directory.path().join("alpha.agentforge.json");
    let beta_path = directory.path().join("beta.agentforge.json");
    let mut session = Session::author();
    session.ok(
        "project.create",
        json!({"name": "alpha", "expected_revision": 0}),
    );
    session.ok("project.save", json!({"path": alpha_path}));
    let snapshot = session.ok("snapshot.capture", json!({}));
    session.ok(
        "project.create",
        json!({"name": "beta", "expected_revision": 1}),
    );
    session.ok(
        "scene.create",
        json!({"name": "only_in_beta", "expected_revision": 2, "layout": ["@E"]}),
    );
    session.ok("project.save", json!({"path": beta_path}));

    session.ok(
        "snapshot.restore",
        json!({"snapshot_id": snapshot["snapshot_id"], "expected_revision": 3}),
    );
    let saved = session.ok("project.save", json!({}));
    assert_eq!(saved["path"], alpha_path.display().to_string());
    let beta = storage::load_project(&beta_path).expect("beta still loads");
    assert_eq!(
        beta.name().as_str(),
        "beta",
        "beta's file was not overwritten"
    );
    assert_eq!(beta.scene_names().count(), 1);
}

/// Regression: after a reset or restore, tick numbers repeat, so `expected_tick` alone let a
/// delayed duplicate act on a different episode.
#[test]
fn expected_episode_id_rejects_actions_meant_for_an_earlier_episode() {
    let mut session = Session::author_with("key_vault");
    let first = session.ok("episode.start", json!({"scene": "key_vault"}));
    let first_id = first["episode_id"].clone();
    let snapshot = session.ok("snapshot.capture", json!({}));
    let reset = session.ok("episode.reset", json!({}));
    assert_ne!(reset["episode_id"], first_id, "reset issues a new id");

    let stale = session.err(
        "episode.act",
        json!({"action": {"type": "move", "direction": "south"}, "expected_episode_id": first_id, "expected_tick": 0}),
    );
    assert_eq!(stale["code"], "stale_episode");
    assert_eq!(session.episode_brief()["tick"], 0, "nothing was applied");

    let revision = session.revision();
    let restored = session.ok(
        "snapshot.restore",
        json!({"snapshot_id": snapshot["snapshot_id"], "expected_revision": revision}),
    );
    let restored_id = restored["active_episode"]["episode_id"].clone();
    assert_ne!(
        restored_id, first_id,
        "a restored episode is a new timeline with a new id"
    );
    assert_ne!(restored_id, reset["episode_id"]);
    let accepted = session.ok(
        "episode.act",
        json!({"action": {"type": "move", "direction": "south"}, "expected_episode_id": restored_id, "expected_tick": 0}),
    );
    assert_eq!(accepted["tick"], 1);
}

/// Regressions: player replays leaked authoring labels, and replay files were named after
/// the final state, so different routes to the same end state overwrote each other.
#[test]
fn player_replays_hide_labels_and_never_overwrite_different_recordings() {
    let directory = tempfile::tempdir().expect("temp dir");
    let project_path = directory.path().join("labelled.agentforge.json");
    let replay_dir = directory.path().join("replays");
    std::fs::create_dir_all(&replay_dir).expect("creatable");
    let mut author = Session::author();
    author.ok(
        "project.create",
        json!({"name": "labelled", "expected_revision": 0}),
    );
    author.ok(
        "scene.create",
        json!({"name": "line", "expected_revision": 1, "layout": ["@.E"]}),
    );
    author.ok(
        "entity.update",
        json!({"scene": "line", "expected_revision": 2, "id": 2, "label": "secret author note"}),
    );
    author.ok("project.save", json!({"path": project_path}));

    let mut exports = Vec::new();
    for route in [["wait", "east", "east"], ["east", "wait", "east"]] {
        let project = storage::load_project(&project_path).expect("loads");
        let mut player = Session {
            host: Host::player(project, "line", Some(replay_dir.clone())).expect("bound"),
            next: 0,
        };
        player.ok("episode.start", json!({}));
        for step in route {
            let action = if step == "wait" {
                json!({"type": "wait"})
            } else {
                json!({"type": "move", "direction": step})
            };
            player.act(action);
        }
        exports.push(player.ok("replay.export", json!({})));
    }
    assert_eq!(
        exports[0]["final_hash"], exports[1]["final_hash"],
        "same end state"
    );
    assert_ne!(
        exports[0]["path"], exports[1]["path"],
        "different recordings, different files"
    );
    for export in &exports {
        let path = PathBuf::from(export["path"].as_str().expect("written"));
        let text = std::fs::read_to_string(&path).expect("readable");
        assert!(
            !text.contains("secret author note"),
            "labels must not reach players"
        );
        let replay = storage::load_replay(&path).expect("parses");
        assert_eq!(
            replay.fingerprint().to_string(),
            export["fingerprint"].as_str().expect("present")
        );
    }
}
