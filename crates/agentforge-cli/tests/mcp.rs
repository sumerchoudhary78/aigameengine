//! Process-level MCP tests: real child processes, real stdio, one long-lived session.
//!
//! The point of the adapter is that state survives across tool calls, so these tests author,
//! play and inspect inside a single `mcp` process rather than one process per request.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const ENGINE: &str = env!("CARGO_BIN_EXE_agentforge");

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Writes every message, closes stdin, and returns the parsed replies plus stderr and status.
fn exchange(args: &[&str], messages: &[Value]) -> (Vec<Value>, String, bool) {
    let mut child = Command::new(ENGINE)
        .args(args)
        .env("AGENTFORGE_LOG", "info")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("engine starts");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        for message in messages {
            writeln!(stdin, "{message}").expect("write message");
        }
    }
    let output = child.wait_with_output().expect("engine exits");
    let replies = String::from_utf8(output.stdout)
        .expect("utf-8 stdout")
        .lines()
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|_| panic!("stdout line is not JSON: {line}"))
        })
        .collect();
    (
        replies,
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
    )
}

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "cli-test", "version": "0"},
        },
    })
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    })
}

#[test]
fn one_mcp_session_authors_plays_and_reports_revisions() {
    let messages = vec![
        initialize(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        call(
            3,
            "project_create",
            json!({"name": "demo", "expected_revision": 0}),
        ),
        call(
            4,
            "scene_create",
            json!({
                "name": "hall",
                "expected_revision": 1,
                "layout": ["####", "#@E#", "####"],
                "rules": {"max_ticks": 5, "win_condition": "reach_exit"},
            }),
        ),
        call(5, "episode_start", json!({"scene": "hall"})),
        call(
            6,
            "episode_act",
            json!({
                "action": {"type": "move", "direction": "east"},
                "expected_episode_id": 1,
                "expected_tick": 0,
            }),
        ),
        call(7, "session_status", json!({})),
    ];
    let (replies, stderr, success) = exchange(&["mcp", "--role", "author"], &messages);

    assert!(success, "{stderr}");
    assert_eq!(
        replies.len(),
        7,
        "the notification gets no reply, every request gets one"
    );
    let tools = replies[1]["result"]["tools"]
        .as_array()
        .expect("tools array");
    assert!(tools.iter().any(|tool| tool["name"] == "scene_create"));
    assert!(tools.iter().any(|tool| tool["name"] == "episode_act"));
    assert_eq!(replies[2]["result"]["structuredContent"]["revision"], 1);
    assert_eq!(
        replies[4]["result"]["structuredContent"]["result"]["episode_id"],
        1
    );

    let act = &replies[5]["result"]["structuredContent"]["result"];
    assert_eq!(act["outcome"]["status"], "won");
    assert_eq!(act["tick"], 1);

    let status = &replies[6]["result"]["structuredContent"]["result"];
    assert_eq!(status["revision"], 2);
    assert_eq!(status["episode"]["tick"], 1);
    assert_eq!(status["episode"]["outcome"]["status"], "won");
    assert!(
        stderr.contains("serving MCP"),
        "logs go to stderr: {stderr}"
    );
}

#[test]
fn player_mcp_is_bound_to_one_scene_and_hides_authoring_tools() {
    let project = root().join("fixtures/key_vault.agentforge.json");
    let project = project.to_str().expect("utf-8 path");
    let messages = vec![
        initialize(),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        call(
            3,
            "project_create",
            json!({"name": "nope", "expected_revision": 1}),
        ),
        call(4, "episode_start", json!({"scene": "key_vault"})),
    ];
    let (replies, stderr, success) = exchange(
        &[
            "mcp",
            "--role",
            "player",
            "--project",
            project,
            "--scene",
            "key_vault",
        ],
        &messages,
    );

    assert!(success, "{stderr}");
    let tools = replies[1]["result"]["tools"]
        .as_array()
        .expect("tools array");
    assert!(tools.iter().all(|tool| tool["name"] != "project_create"));
    assert!(tools.iter().any(|tool| tool["name"] == "episode_act"));
    assert_eq!(
        replies[2]["error"]["code"], -32602,
        "an unadvertised tool is a protocol error, not an engine call"
    );
    assert_eq!(replies[3]["result"]["isError"], false);
    assert_eq!(
        replies[3]["result"]["structuredContent"]["result"]["episode_id"],
        1
    );
}

#[test]
fn physics_mcp_exposes_rigid_body_tools_and_advances_a_tick() {
    let document: Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("fixtures/physics_drop.json")).expect("fixture"),
    )
    .expect("fixture is JSON");
    let messages = vec![
        initialize(),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        call(
            3,
            "scene_set",
            json!({"document": document, "expected_revision": 0}),
        ),
        call(4, "episode_start", json!({})),
        call(
            5,
            "episode_act",
            json!({
                "action": {"type": "wait"},
                "expected_episode_id": 1,
                "expected_tick": 0,
            }),
        ),
        call(6, "episode_observe", json!({})),
    ];
    let (replies, stderr, success) = exchange(&["physics", "mcp", "--role", "author"], &messages);

    assert!(success, "{stderr}");
    assert_eq!(replies.len(), 6, "every request gets exactly one reply");
    let tools = replies[1]["result"]["tools"]
        .as_array()
        .expect("tools array");
    assert!(tools.iter().any(|tool| tool["name"] == "scene_set"));
    assert!(tools.iter().any(|tool| tool["name"] == "episode_act"));
    assert_eq!(replies[2]["result"]["structuredContent"]["revision"], 1);

    let act = &replies[4]["result"]["structuredContent"]["result"];
    assert_eq!(act["tick"], 1);
    assert_eq!(act["observation"]["tick"], 1);
    let observed = &replies[5]["result"]["structuredContent"]["result"];
    assert_eq!(observed["episode_id"], 1);
    assert_eq!(observed["observation"]["tick"], 1);
}
