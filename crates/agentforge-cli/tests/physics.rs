//! Exercise physics through real stdin/stdout child processes, including durable replays.
use agentforge_demo::client::EngineSession;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
};
const ENGINE: &str = env!("CARGO_BIN_EXE_agentforge");
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn launch() -> EngineSession {
    EngineSession::launch(
        Path::new(ENGINE),
        &["physics", "serve", "--role", "author"],
        "test",
        None,
    )
    .expect("launch")
}
fn call(s: &mut EngineSession, op: &str, args: Value) -> Value {
    s.call(op, args).expect(op)
}
fn setup(s: &mut EngineSession) {
    let document: Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("fixtures/physics_launch.json")).expect("fixture"),
    )
    .expect("JSON");
    call(
        s,
        "scene.set",
        json!({"document":document,"expected_revision":0}),
    );
}
#[test]
fn physics_demo_saves_repaired_project_and_replay_verifies_in_a_fresh_process() {
    let out = tempfile::tempdir().expect("temp");
    let result = Command::new(ENGINE)
        .current_dir(root())
        .args(["physics", "demo", "--out"])
        .arg(out.path())
        .output()
        .expect("demo");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let replay = out.path().join("replay.json");
    assert!(replay.is_file());
    let result = Command::new(ENGINE)
        .args(["physics", "replay", "verify"])
        .arg(&replay)
        .output()
        .expect("verify");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result: Value = serde_json::from_slice(&result.stdout).expect("JSON only");
    assert_eq!(result["verified"], true);
    assert_eq!(result["outcome"], "won");
    assert!(result["steps_checked"].as_u64().expect("ticks") > 1);
}
#[test]
fn physics_protocol_rejects_stale_writes_and_actions_and_reproduces_checkpoint_continuation() {
    let mut host = launch();
    setup(&mut host);
    let invalid = host
        .request(
            "scene.apply_batch",
            json!({"expected_revision":0,"edits":[]}),
        )
        .expect("response");
    assert_eq!(invalid["error"]["code"], "stale_revision");
    let start = call(&mut host, "episode.start", json!({}));
    let id = start["episode_id"].clone();
    let initial = call(&mut host, "episode.observe", json!({}));
    let invalid=host.request("episode.act",json!({"expected_episode_id":id,"expected_tick":0,"action":{"type":"impulse","impulse":{"x":50,"y":0}}})).expect("response");
    assert_eq!(invalid["status"], "error");
    assert_eq!(initial, call(&mut host, "episode.observe", json!({})));
    let snap = call(&mut host, "snapshot.capture", json!({}));
    let advance = call(
        &mut host,
        "episode.act",
        json!({"expected_episode_id":id,"expected_tick":0,"action":{"type":"impulse","impulse":{"x":1,"y":0}}}),
    );
    let duplicate = host
        .request(
            "episode.act",
            json!({"expected_episode_id":id,"expected_tick":0,"action":{"type":"wait"}}),
        )
        .expect("response");
    assert_eq!(duplicate["error"]["code"], "stale_tick");
    let revision = host.revision();
    let restore = call(
        &mut host,
        "snapshot.restore",
        json!({"snapshot_id":snap["snapshot_id"],"expected_revision":revision}),
    );
    assert!(host.revision() > revision);
    let stale = host
        .request(
            "episode.act",
            json!({"expected_episode_id":id,"expected_tick":0,"action":{"type":"wait"}}),
        )
        .expect("response");
    assert_eq!(stale["error"]["code"], "stale_episode");
    let repeat = call(
        &mut host,
        "episode.act",
        json!({"expected_episode_id":restore["episode_id"],"expected_tick":0,"action":{"type":"impulse","impulse":{"x":1,"y":0}}}),
    );
    assert_eq!(
        advance["observation"]["state_hash"],
        repeat["observation"]["state_hash"]
    );
    host.finish().expect("close");
}
#[test]
fn physics_player_cannot_author_or_read_authoring_labels() {
    let out = tempfile::tempdir().expect("temp");
    let project = out.path().join("physics.json");
    let mut author = launch();
    setup(&mut author);
    call(
        &mut author,
        "scene.save",
        json!({"path":project,"expected_revision":1}),
    );
    author.finish().expect("close");
    let mut player = EngineSession::launch(
        Path::new(ENGINE),
        &[
            "physics",
            "serve",
            "--role",
            "player",
            "--project",
            project.to_str().expect("path"),
        ],
        "player",
        None,
    )
    .expect("launch");
    for (op, args) in [
        ("scene.set", json!({})),
        ("scene.get", json!({})),
        (
            "snapshot.restore",
            json!({"snapshot_id":1,"expected_revision":1}),
        ),
        ("replay.verify", json!({})),
    ] {
        let response = player.request(op, args).expect("response");
        assert_eq!(response["error"]["code"], "permission_denied", "{op}");
    }
    let start = call(&mut player, "episode.start", json!({}));
    let labels = start["observation"]["scene"]["bodies"]
        .as_array()
        .expect("bodies");
    assert!(labels.iter().all(|b| b.get("label").is_none()));
    player.finish().expect("close");
}
#[test]
fn physics_schemas_match_committed_contract() {
    let result = Command::new(ENGINE)
        .current_dir(root())
        .args(["physics", "schema", "--check"])
        .output()
        .expect("check");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn physics_process_survives_malformed_requests_and_unsupported_versions() {
    use std::{io::Write, process::Stdio};
    let mut child = Command::new(ENGINE)
        .args(["physics", "serve", "--role", "author"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().expect("stdin");
    writeln!(stdin, "{{bad").expect("write");
    writeln!(stdin,"{}",json!({"request_id":"unsupported","protocol_version":99,"operation":"session.status","arguments":{}})).expect("write");
    writeln!(stdin,"{}",json!({"request_id":"alive","protocol_version":1,"operation":"session.status","arguments":{}})).expect("write");
    drop(stdin);
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success());
    let responses = String::from_utf8(output.stdout)
        .expect("UTF8")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("JSON only"))
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 3);
    assert_eq!(responses[0]["error"]["code"], "malformed_json");
    assert_eq!(responses[1]["request_id"], "unsupported");
    assert_eq!(
        responses[1]["error"]["code"],
        "unsupported_protocol_version"
    );
    assert_eq!(responses[2]["request_id"], "alive");
    assert_eq!(responses[2]["status"], "ok");
    assert_eq!(responses[2]["revision"], 0);
}

#[test]
fn physics_author_launcher_preserves_loaded_project_path_in_snapshots() {
    let out = tempfile::tempdir().expect("temp");
    let project = out.path().join("startup.json");
    let mut author = launch();
    setup(&mut author);
    call(
        &mut author,
        "scene.save",
        json!({"path":project,"expected_revision":1}),
    );
    author.finish().expect("close");
    let mut author = EngineSession::launch(
        Path::new(ENGINE),
        &[
            "physics",
            "serve",
            "--role",
            "author",
            "--project",
            project.to_str().expect("path"),
        ],
        "startup",
        None,
    )
    .expect("launch");
    assert_eq!(
        call(&mut author, "session.status", json!({}))["path"],
        project.to_str().expect("path")
    );
    let snapshot = call(&mut author, "snapshot.capture", json!({}));
    call(
        &mut author,
        "snapshot.restore",
        json!({"snapshot_id":snapshot["snapshot_id"],"expected_revision":1}),
    );
    assert_eq!(
        call(&mut author, "session.status", json!({}))["path"],
        project.to_str().expect("path")
    );
    author.finish().expect("close");
}
