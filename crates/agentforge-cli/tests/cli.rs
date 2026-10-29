//! Process-level tests of the `agentforge` binary: real child processes, real pipes,
//! real files.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const ENGINE: &str = env!("CARGO_BIN_EXE_agentforge");

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> PathBuf {
    repository()
        .join("fixtures")
        .join(format!("{name}.agentforge.json"))
}

/// Feeds `lines` to a `serve` process and returns its stdout lines (parsed) and stderr.
fn serve(args: &[&str], lines: &[&str]) -> (Vec<Value>, String, bool) {
    let mut child = Command::new(ENGINE)
        .arg("serve")
        .args(args)
        .env("AGENTFORGE_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("engine starts");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for line in lines {
            writeln!(stdin, "{line}").expect("write request");
        }
    }
    let output = child.wait_with_output().expect("engine exits");
    let responses = String::from_utf8(output.stdout)
        .expect("utf-8 stdout")
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|_| panic!("stdout line is not JSON: {line}"))
        })
        .collect();
    (
        responses,
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

#[test]
fn serve_answers_every_line_keeps_stdout_pure_and_survives_bad_input() {
    let (responses, stderr, success) = serve(
        &["--role", "author"],
        &[
            "this is not json",
            r#"{"request_id":"v9","protocol_version":9,"operation":"discover"}"#,
            "",
            r#"{"request_id":"ok","protocol_version":1,"operation":"session.status"}"#,
        ],
    );
    assert!(success);
    assert_eq!(
        responses.len(),
        3,
        "blank lines are ignored, every other line is answered"
    );
    assert_eq!(responses[0]["error"]["code"], "malformed_json");
    assert_eq!(
        responses[1]["error"]["code"],
        "unsupported_protocol_version"
    );
    assert_eq!(responses[1]["request_id"], "v9");
    assert_eq!(responses[2]["status"], "ok");
    assert!(
        stderr.contains("serving JSON Lines"),
        "logs go to stderr: {stderr}"
    );
}

#[test]
fn a_player_process_cannot_author_even_when_asked_politely() {
    let project = fixture("key_vault").display().to_string();
    let (responses, _, success) = serve(
        &[
            "--role",
            "player",
            "--project",
            &project,
            "--scene",
            "key_vault",
        ],
        &[
            r#"{"request_id":"1","protocol_version":1,"operation":"entity.remove","arguments":{"scene":"key_vault","expected_revision":1,"id":1}}"#,
            r#"{"request_id":"2","protocol_version":1,"operation":"episode.start"}"#,
            r#"{"request_id":"3","protocol_version":1,"operation":"episode.act","arguments":{"action":{"type":"move","direction":"south"},"expected_tick":0}}"#,
        ],
    );
    assert!(success);
    assert_eq!(responses[0]["error"]["code"], "permission_denied");
    assert_eq!(responses[1]["status"], "ok");
    assert_eq!(responses[2]["result"]["tick"], 1);
}

#[test]
fn player_sessions_require_a_project_and_scene_from_the_launcher() {
    let output = Command::new(ENGINE)
        .args(["serve", "--role", "player"])
        .output()
        .expect("runs");
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--project"));
}

#[test]
fn demo_runs_end_to_end_and_its_replay_verifies_in_a_fresh_process() {
    let out = tempfile::tempdir().expect("temp dir");
    let output = Command::new(ENGINE)
        .args(["demo", "--out"])
        .arg(out.path())
        .arg("--fixtures")
        .arg(repository().join("fixtures"))
        .output()
        .expect("demo runs");
    let narration = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "demo failed:\n{narration}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(narration.contains("no language model is involved"));

    let summary: Value = serde_json::from_str(
        &std::fs::read_to_string(out.path().join("summary.json")).expect("summary written"),
    )
    .expect("summary is JSON");
    assert_eq!(summary["first_attempt"]["outcome"]["status"], "failed");
    assert_eq!(summary["diagnosis"]["verdict"], "unsolvable");
    assert_eq!(summary["second_attempt"]["outcome"]["status"], "won");
    assert_eq!(summary["second_scene"]["outcome"]["status"], "won");
    assert_eq!(summary["fresh_process_verification"]["verified"], true);
    let pids = [
        &summary["first_attempt"]["process_id"],
        &summary["second_attempt"]["process_id"],
        &summary["fresh_process_verification"]["process_id"],
    ];
    assert!(
        pids[0] != pids[1] && pids[1] != pids[2],
        "each step ran in its own process"
    );

    // The transcript shows the repair went through the public protocol.
    let transcript =
        std::fs::read_to_string(out.path().join("transcript.jsonl")).expect("transcript");
    assert!(transcript.contains("\"operation\":\"entity.remove\""));

    // Independently re-verify from this test, then show tampering is caught.
    let replay_path = PathBuf::from(
        summary["second_attempt"]["replay"]
            .as_str()
            .expect("replay path"),
    );
    let project_path = out.path().join("project.agentforge.json");
    let verify = |replay: &Path| {
        Command::new(ENGINE)
            .args(["replay", "verify"])
            .arg(replay)
            .arg("--project")
            .arg(&project_path)
            .output()
            .expect("verify runs")
    };
    let verified = verify(&replay_path);
    assert!(verified.status.success());
    let report: Value = serde_json::from_slice(&verified.stdout).expect("JSON report");
    assert_eq!(
        report["verification"]["reproduced_outcome"]["status"],
        "won"
    );
    assert_eq!(report["project_scene_matches"], true);

    let mut replay: Value =
        serde_json::from_str(&std::fs::read_to_string(&replay_path).expect("replay readable"))
            .expect("JSON");
    replay["steps"][5]["hash"] = json!(format!("sha256:{}", "0".repeat(64)));
    let tampered_path = out.path().join("tampered.replay.json");
    std::fs::write(&tampered_path, replay.to_string()).expect("writable");
    let tampered = verify(&tampered_path);
    assert_eq!(tampered.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&tampered.stdout).expect("JSON report");
    assert_eq!(report["verification"]["divergence"]["kind"], "state_hash");
    assert_eq!(report["verification"]["divergence"]["step_index"], 5);

    // A replay of the same moves against the original (unrepaired) fixture does not match.
    let mismatch = Command::new(ENGINE)
        .args(["replay", "verify"])
        .arg(&replay_path)
        .arg("--project")
        .arg(fixture("blocked_dungeon"))
        .output()
        .expect("verify runs");
    assert_eq!(
        mismatch.status.code(),
        Some(1),
        "repaired replay must not match the broken scene"
    );
}

#[test]
fn incompatible_replays_are_rejected_explicitly() {
    let directory = tempfile::tempdir().expect("temp dir");
    let path = directory.path().join("future.replay.json");
    // A file from a hypothetical future version, with fields this build does not know.
    std::fs::write(
        &path,
        json!({"format": "agentforge.replay", "format_version": 2, "frames": []}).to_string(),
    )
    .expect("writable");
    let output = Command::new(ENGINE)
        .args(["replay", "verify"])
        .arg(&path)
        .output()
        .expect("runs");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("declares format \"agentforge.replay\" version 2")
            && stderr.contains("supports format \"agentforge.replay\" version 1"),
        "{stderr}"
    );
}

#[test]
fn committed_protocol_docs_match_the_generator() {
    let output = Command::new(ENGINE)
        .args(["schema", "--check", "--out"])
        .arg(repository().join("docs/protocol"))
        .output()
        .expect("runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn bench_reports_every_layer_separately() {
    let output = Command::new(ENGINE)
        .args([
            "bench",
            "--worlds",
            "2",
            "--ticks",
            "50",
            "--process-worlds",
            "1",
        ])
        .output()
        .expect("runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    let modes: Vec<&str> = report["results"]
        .as_array()
        .expect("results")
        .iter()
        .filter_map(|result| result["mode"].as_str())
        .collect();
    assert_eq!(
        modes,
        [
            "simulation",
            "protocol_in_process",
            "protocol_in_process_with_observation",
            "process_pipe"
        ]
    );
    assert!(report["build_profile"].is_string());
    assert_eq!(report["workload"]["worlds"], 2);
}

#[test]
fn a_long_session_can_be_driven_interactively_over_pipes() {
    let mut child = Command::new(ENGINE)
        .args(["serve", "--role", "author"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("engine starts");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let mut call = |request: Value| -> Value {
        writeln!(stdin, "{request}").expect("write");
        stdin.flush().expect("flush");
        let mut line = String::new();
        stdout.read_line(&mut line).expect("read");
        serde_json::from_str(&line).expect("JSON response")
    };
    let created = call(
        json!({"request_id": "1", "protocol_version": 1, "operation": "project.create", "arguments": {"name": "live", "expected_revision": 0}}),
    );
    assert_eq!(created["revision"], 1);
    // Each response arrives before the next request is written: the session is interactive.
    let scene = call(
        json!({"request_id": "2", "protocol_version": 1, "operation": "scene.create", "arguments": {"name": "s", "expected_revision": created["revision"], "layout": ["@.E"]}}),
    );
    assert_eq!(scene["status"], "ok");
    drop(call);
    drop(stdin);
    assert!(child.wait().expect("exits").success());
}
