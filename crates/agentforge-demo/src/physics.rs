//! Scripted physics clients using only the public JSON Lines protocol.
//!
//! An observed failed launch motivates an authoring repair; fresh player processes
//! retest the saved scene and export embedded replays for command-line verification.

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

use crate::client::{EngineSession, Transcript, open_transcript};

/// Runs the physics author → player → diagnose → repair → replay demonstration.
///
/// `engine` is the `agentforge` executable, `out` receives the artifacts, and
/// `fixtures` contains `physics_launch.json` and `physics_drop.json`. All engine
/// interaction uses child processes and JSON; this crate has no engine dependency.
/// Returns the report also saved as `out/summary.json`.
pub fn run(engine: &Path, out: &Path, fixtures: &Path) -> Result<Value, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    // Absolute saved paths also work when the caller passes a relative output directory.
    let out = std::fs::canonicalize(out).map_err(|e| e.to_string())?;
    let transcript_path = out.join("transcript.jsonl");
    let transcript = open_transcript(&transcript_path).map_err(|e| e.to_string())?;
    let mut document = read_json(&fixtures.join("physics_launch.json"))?;
    let goal_id = document["settings"]["objective"]["zone"].clone();
    let goal = document["bodies"]
        .as_array_mut()
        .and_then(|bodies| bodies.iter_mut().find(|body| body["id"] == goal_id))
        .ok_or("launch fixture lacks its objective zone")?;
    let goal_x = goal["position"]["x"]
        .as_f64()
        .ok_or("launch goal lacks position.x")?;
    goal["position"]["y"] = json!(3);

    let project = out.join("launch.physics.json");
    let before_svg = out.join("launch.before.svg");
    let after_svg = out.join("launch.after.svg");
    let mut author = launch(engine, "author", None, "launch-author", &transcript)?;
    let author_pid = author.process_id();
    let revision = author.revision();
    call(
        &mut author,
        "scene.set",
        json!({"document": document, "expected_revision": revision}),
    )?;
    save(&mut author, &project)?;
    svg(&mut author, &before_svg)?;

    let first = play(engine, &project, "launch-failed", true, &transcript)?;
    let outcome = first["outcome"].as_str().ok_or("missing failed outcome")?;
    if !matches!(outcome, "tick_limit" | "left_bounds") {
        return Err(format!("expected tick_limit or left_bounds, got {outcome}"));
    }

    // Diagnose the actual controlled body's observed path, not a solver verdict or
    // a claim that every possible action sequence is unsolvable.
    let current_scene = call(&mut author, "scene.get", json!({}))?;
    let observed_goal = current_scene["document"]["bodies"]
        .as_array()
        .and_then(|bodies| bodies.iter().find(|body| body["id"] == goal_id))
        .ok_or("scene.get result lacks the objective zone in document.bodies")?;
    if observed_goal["position"]["y"].as_f64() != Some(3.0) {
        return Err(format!(
            "expected goal at y=3, got {}",
            observed_goal["position"]
        ));
    }
    let trajectory = first["trajectory"]
        .as_array()
        .ok_or("failed playtest lacks trajectory")?;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for sample in trajectory {
        let y = sample["position"]["y"]
            .as_f64()
            .ok_or("trajectory sample lacks position.y")?;
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    if min_y.abs() > 1e-6 || max_y.abs() > 1e-6 {
        return Err(format!(
            "expected horizontal trajectory at y=0, observed {min_y}..{max_y}"
        ));
    }
    let diagnosis = json!({
        "basis": "observed trajectory, not a general solvability proof",
        "summary": "The horizontal impulse kept the controlled body at y=0 while the goal was at y=3. Move the goal onto the observed path and retest.",
        "observed_y_range": [min_y, max_y],
        "goal_position_before": observed_goal["position"],
        "terminal_outcome": outcome,
        "scene": current_scene,
        "trajectory": trajectory,
    });
    let revision = author.revision();
    call(
        &mut author,
        "scene.apply_batch",
        json!({
            "edits": [{"op": "update_body", "id": goal_id,
                "changes": {"position": {"x": goal_x, "y": 0}}}],
            "expected_revision": revision,
        }),
    )?;
    save(&mut author, &project)?;
    svg(&mut author, &after_svg)?;
    author.finish().map_err(|e| e.to_string())?;

    let second = play(engine, &project, "launch-won", true, &transcript)?;
    require_won(&second, "repaired launch")?;
    let replay_path = out.join("replay.json");
    write_json(&replay_path, &second["replay"])?;
    let verification = verify(engine, &replay_path)?;

    // A second, unmodified fixture exercises gravity through the same public ops.
    let drop_project = out.join("drop.physics.json");
    let mut drop_author = launch(engine, "author", None, "drop-author", &transcript)?;
    let revision = drop_author.revision();
    call(
        &mut drop_author,
        "scene.set",
        json!({"document": read_json(&fixtures.join("physics_drop.json"))?,
            "expected_revision": revision}),
    )?;
    save(&mut drop_author, &drop_project)?;
    drop_author.finish().map_err(|e| e.to_string())?;
    let drop = play(engine, &drop_project, "drop-won", false, &transcript)?;
    require_won(&drop, "drop fixture")?;
    let drop_replay = out.join("drop.replay.json");
    write_json(&drop_replay, &drop["replay"])?;
    let drop_verification = verify(engine, &drop_replay)?;

    let summary = json!({
        "clients": "deterministic scripted protocol clients, not language models",
        "author_process_id": author_pid,
        "project": project,
        "transcript": transcript_path,
        "first_attempt": report(&first),
        "diagnosis": diagnosis,
        "repair": {"op": "update_body", "id": goal_id,
            "changes": {"position": {"x": goal_x, "y": 0}}},
        "second_attempt": report(&second),
        "replay": replay_path,
        "fresh_process_verification": verification,
        "second_scene": report(&drop),
        "second_scene_project": drop_project,
        "second_scene_replay": drop_replay,
        "second_scene_verification": drop_verification,
        "svg": [before_svg, after_svg],
    });
    write_json(&out.join("summary.json"), &summary)?;
    Ok(summary)
}

fn launch(
    engine: &Path,
    role: &str,
    project: Option<&Path>,
    label: &str,
    transcript: &Transcript,
) -> Result<EngineSession, String> {
    let project_arg = project.map(|p| p.to_string_lossy().into_owned());
    let mut args = vec!["physics", "serve", "--role", role];
    if let Some(path) = project_arg.as_deref() {
        args.extend(["--project", path]);
    }
    EngineSession::launch(engine, &args, label, Some(transcript.clone())).map_err(|e| e.to_string())
}

fn call(session: &mut EngineSession, op: &str, args: Value) -> Result<Value, String> {
    session.call(op, args).map_err(|e| e.to_string())
}

fn save(session: &mut EngineSession, path: &Path) -> Result<(), String> {
    call(
        session,
        "scene.save",
        json!({"path": path, "expected_revision": session.revision()}),
    )?;
    Ok(())
}

fn svg(session: &mut EngineSession, path: &Path) -> Result<(), String> {
    let result = call(session, "scene.svg", json!({}))?;
    let svg = result["svg"].as_str().ok_or("scene.svg result lacks svg")?;
    std::fs::write(path, svg).map_err(|e| format!("write {}: {e}", path.display()))
}

fn play(
    engine: &Path,
    project: &Path,
    label: &str,
    impulse: bool,
    transcript: &Transcript,
) -> Result<Value, String> {
    let mut player = launch(engine, "player", Some(project), label, transcript)?;
    let pid = player.process_id();
    let started = call(&mut player, "episode.start", json!({}))?;
    let episode_id = started
        .get("episode_id")
        .cloned()
        .ok_or("episode.start lacks episode_id")?;
    let mut observation = started
        .get("observation")
        .cloned()
        .ok_or("episode.start lacks observation")?;
    let body_id = observation["settings"]["control"]["body"].clone();
    if body_id.is_null() {
        return Err("observation lacks settings.control.body".into());
    }
    let max_ticks = observation["settings"]["max_ticks"]
        .as_u64()
        .ok_or("observation lacks max_ticks")?;
    let mut trajectory = Vec::new();
    loop {
        let tick = observation["tick"]
            .as_u64()
            .ok_or("observation lacks tick")?;
        let body = observation["bodies"]
            .as_array()
            .and_then(|bodies| bodies.iter().find(|body| body["id"] == body_id))
            .ok_or("observation lacks controlled body")?;
        trajectory.push(json!({"tick": tick, "position": body["position"],
            "linear_velocity": body["linear_velocity"]}));
        let outcome = observation["outcome"]
            .as_str()
            .ok_or("observation lacks outcome")?;
        match outcome {
            "won" | "tick_limit" | "left_bounds" | "forfeited" | "numerical_failure" => break,
            "in_progress" => {}
            other => return Err(format!("unknown physics outcome: {other}")),
        }
        if tick >= max_ticks {
            return Err(format!(
                "episode still in progress at tick limit {max_ticks}"
            ));
        }
        let action = if impulse && tick == 0 {
            json!({"type": "impulse", "impulse": {"x": 1, "y": 0}})
        } else {
            json!({"type": "wait"})
        };
        let result = call(
            &mut player,
            "episode.act",
            json!({"action": action,
            "expected_episode_id": episode_id, "expected_tick": tick}),
        )?;
        observation = result
            .get("observation")
            .cloned()
            .ok_or("episode.act lacks observation")?;
        if observation["tick"].as_u64() != Some(tick + 1) {
            return Err(format!(
                "episode.act did not advance exactly one tick after {tick}"
            ));
        }
    }
    let exported = call(&mut player, "replay.export", json!({}))?;
    let replay = exported
        .get("replay")
        .filter(|value| value.is_object())
        .ok_or("replay.export lacks embedded replay")?;
    let result = json!({"process_id": pid, "episode_id": episode_id,
        "outcome": observation["outcome"], "final_tick": observation["tick"],
        "trajectory": trajectory, "replay": replay});
    player.finish().map_err(|e| e.to_string())?;
    Ok(result)
}

fn require_won(result: &Value, label: &str) -> Result<(), String> {
    if result["outcome"] != "won" {
        return Err(format!("{label} should be won, got {}", result["outcome"]));
    }
    Ok(())
}

fn report(play: &Value) -> Value {
    json!({"process_id": play["process_id"], "episode_id": play["episode_id"],
        "outcome": play["outcome"], "final_tick": play["final_tick"]})
}

fn verify(engine: &Path, replay: &Path) -> Result<Value, String> {
    let child = Command::new(engine)
        .args(["physics", "replay", "verify"])
        .arg(replay)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("launch replay verification: {e}"))?;
    let pid = child.id();
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "physics replay verify failed ({}): {} {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let verification: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("invalid replay verification JSON: {e}"))?;
    if verification["verified"] != true {
        return Err(format!("physics replay was not verified: {verification}"));
    }
    Ok(json!({"process_id": pid, "verified": true, "verification": verification}))
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n";
    std::fs::write(path, text).map_err(|e| format!("write {}: {e}", path.display()))
}
