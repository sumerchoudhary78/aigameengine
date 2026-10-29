//! The end-to-end demo: author-create → player-test → diagnose → author-repair →
//! player-success → save/replay → fresh-process verification, plus a second scene.
//!
//! Every step runs through engine child processes. Author and player use separate
//! processes whose roles are fixed on the command line, and the player sees the author's
//! work only through the saved project file.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::builder::{self, BLOCKED_DUNGEON, BLOCKED_DUNGEON_LAYOUT};
use crate::client::{ClientError, EngineSession, Transcript, open_transcript};
use crate::player::{self, PlayReport};

/// Where the engine is and where the demo writes.
#[derive(Debug, Clone)]
pub struct DemoConfig {
    /// The `agentforge` executable.
    pub engine: PathBuf,
    /// Output directory (created if missing).
    pub out_dir: PathBuf,
    /// Directory containing `key_vault.agentforge.json`.
    pub fixtures_dir: PathBuf,
}

/// A demo failure.
#[derive(Debug, thiserror::Error)]
pub enum DemoError {
    /// Talking to the engine failed or the engine rejected a request.
    #[error(transparent)]
    Client(#[from] ClientError),
    /// A local file operation failed.
    #[error("{action} {path}: {source}")]
    Io {
        /// What was attempted.
        action: &'static str,
        /// The file involved.
        path: String,
        /// Underlying error.
        source: io::Error,
    },
    /// The engine behaved differently from what the demo asserts.
    #[error("expectation failed: {0}")]
    Expectation(String),
}

fn io_error<'a>(action: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> DemoError + 'a {
    move |source| DemoError::Io {
        action,
        path: path.display().to_string(),
        source,
    }
}

fn expect(condition: bool, message: impl FnOnce() -> String) -> Result<(), DemoError> {
    if condition {
        Ok(())
    } else {
        Err(DemoError::Expectation(message()))
    }
}

/// Runs the demo, narrating to `out`. Returns the summary also written to `summary.json`.
pub fn run(config: &DemoConfig, out: &mut dyn Write) -> Result<Value, DemoError> {
    let out_dir = &config.out_dir;
    let replay_dir = out_dir.join("replays");
    std::fs::create_dir_all(&replay_dir).map_err(io_error("create", &replay_dir))?;
    let project_path = out_dir.join("project.agentforge.json");
    let transcript_path = out_dir.join("transcript.jsonl");
    let transcript =
        open_transcript(&transcript_path).map_err(io_error("create", &transcript_path))?;
    let mut say =
        |line: String| writeln!(out, "{line}").map_err(io_error("write", Path::new("<stdout>")));

    say(
        "AgentForge demo: deterministic scripted protocol clients (no language model is involved)."
            .to_owned(),
    )?;
    say(format!("engine: {}", config.engine.display()))?;

    // 1. Author creates the dungeon through the protocol and saves it.
    let mut author = EngineSession::launch(
        &config.engine,
        &["serve", "--role", "author"],
        "author",
        Some(transcript.clone()),
    )?;
    builder::create_project(&mut author, "demo")?;
    let created = builder::create_scene(
        &mut author,
        BLOCKED_DUNGEON,
        &BLOCKED_DUNGEON_LAYOUT,
        builder::blocked_dungeon_rules(),
    )?;
    say(format!(
        "\n[1] author (pid {}): created scene {BLOCKED_DUNGEON:?} from an ASCII layout -> revision {}",
        author.process_id(),
        author.revision()
    ))?;
    for row in created["ascii"].as_array().into_iter().flatten() {
        say(format!("      {}", row.as_str().unwrap_or_default()))?;
    }
    builder::save(&mut author, &project_path)?;
    let before_svg = out_dir.join("blocked_dungeon.before.svg");
    builder::render_svg(&mut author, BLOCKED_DUNGEON, &before_svg)?;
    say(format!(
        "    saved {} and rendered {}",
        project_path.display(),
        before_svg.display()
    ))?;

    // 2. A player process tries the saved dungeon.
    let first = playtest(
        config,
        &project_path,
        BLOCKED_DUNGEON,
        &replay_dir,
        "player-1",
        &transcript,
    )?;
    say(format!(
        "\n[2] player (separate process, --role player): planner found {}; submitted {} action(s): {}",
        if first.0.planned {
            "a route"
        } else {
            "no route"
        },
        first.0.actions.len(),
        first.0.actions.join(", ")
    ))?;
    say(format!("    engine outcome: {}", first.0.outcome))?;
    expect(first.0.status() == "failed", || {
        format!("first attempt should fail, got {}", first.0.outcome)
    })?;

    // 3. Author asks the engine why.
    let diagnosis = builder::diagnose(&mut author, BLOCKED_DUNGEON)?;
    say(format!(
        "\n[3] author: analysis.reachability -> verdict {}",
        diagnosis["verdict"]
    ))?;
    say(format!(
        "    {}",
        diagnosis["summary"].as_str().unwrap_or_default()
    ))?;
    say(format!(
        "    geometric reachable: {} | gameplay reachable: {} | blockers on frontier: {}",
        diagnosis["geometric"]["reachable"],
        diagnosis["gameplay"]["reachable"],
        diagnosis["blockers"].as_array().map_or(0, Vec::len)
    ))?;
    expect(diagnosis["verdict"] == "unsolvable", || {
        "diagnosis should prove the dungeon unsolvable".to_owned()
    })?;

    // 4. Author applies the engine-verified repair and re-validates.
    let revision_before_repair = author.revision();
    let (candidate, removed) = builder::apply_verified_repair(
        &mut author,
        BLOCKED_DUNGEON,
        &diagnosis,
    )?
    .ok_or_else(|| {
        DemoError::Expectation("engine reported no verified single-removal repair".to_owned())
    })?;
    say(format!(
        "\n[4] author: entity.remove id {} ({} at ({}, {})), the engine-verified repair, with expected_revision {} -> revision {}",
        candidate["remove"],
        candidate["kind"]["type"].as_str().unwrap_or("entity"),
        candidate["position"]["x"],
        candidate["position"]["y"],
        revision_before_repair,
        author.revision()
    ))?;
    let validation = builder::validate(&mut author, BLOCKED_DUNGEON)?;
    say(format!(
        "    scene.validate: valid {}, playable {}, shortest solution {} moves",
        validation["valid"], validation["playable"], validation["shortest_solution_length"]
    ))?;
    expect(validation["playable"] == true, || {
        format!("repaired scene should be playable: {validation}")
    })?;
    builder::save(&mut author, &project_path)?;
    let after_svg = out_dir.join("blocked_dungeon.after.svg");
    builder::render_svg(&mut author, BLOCKED_DUNGEON, &after_svg)?;
    say(format!(
        "    saved repaired project; rendered {}",
        after_svg.display()
    ))?;
    author.finish()?;

    // 5. A fresh player process plays the repaired dungeon.
    let second = playtest(
        config,
        &project_path,
        BLOCKED_DUNGEON,
        &replay_dir,
        "player-2",
        &transcript,
    )?;
    say(format!(
        "\n[5] player (fresh process): {} action(s) -> engine outcome {}",
        second.0.actions.len(),
        second.0.outcome
    ))?;
    expect(second.0.status() == "won", || {
        format!("repaired dungeon should be won, got {}", second.0.outcome)
    })?;
    let winning_replay = second.0.replay["path"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| {
            DemoError::Expectation("player session should have written its replay file".to_owned())
        })?;
    say(format!("    replay: {}", winning_replay.display()))?;

    // 6. The second scene works through the same commands, unchanged engine.
    let vault_project = config.fixtures_dir.join("key_vault.agentforge.json");
    let vault = playtest(
        config,
        &vault_project,
        "key_vault",
        &replay_dir,
        "player-3",
        &transcript,
    )?;
    say(format!(
        "\n[6] player (fresh process) on fixture scene \"key_vault\" (collect_all_keys_then_exit): {} action(s) -> {}",
        vault.0.actions.len(),
        vault.0.outcome
    ))?;
    expect(vault.0.status() == "won", || {
        format!("key_vault should be won, got {}", vault.0.outcome)
    })?;

    // 7. A fresh process re-executes the winning replay against the saved project.
    let verification = verify_in_fresh_process(&config.engine, &winning_replay, &project_path)?;
    say(format!(
        "\n[7] fresh process (pid {}): agentforge replay verify -> verified {}, {}/{} per-tick hashes matched, outcome {}",
        verification["process_id"],
        verification["verified"],
        verification["verification"]["steps_checked"],
        verification["verification"]["steps_recorded"],
        verification["verification"]["reproduced_outcome"]
    ))?;
    expect(verification["verified"] == true, || {
        format!("replay verification failed: {verification}")
    })?;

    let summary = json!({
        "clients": "deterministic scripted protocol clients, not language models",
        "project": project_path.display().to_string(),
        "transcript": transcript_path.display().to_string(),
        "first_attempt": play_summary(&first),
        "diagnosis": {
            "verdict": diagnosis["verdict"],
            "summary": diagnosis["summary"],
            "geometric_reachable": diagnosis["geometric"]["reachable"],
            "gameplay_reachable": diagnosis["gameplay"]["reachable"],
            "repairs": diagnosis["repairs"],
        },
        "repair": {"candidate": candidate, "removed": removed["removed"], "validation": validation},
        "second_attempt": play_summary(&second),
        "second_scene": play_summary(&vault),
        "fresh_process_verification": verification,
        "svg": [before_svg.display().to_string(), after_svg.display().to_string()],
    });
    let summary_path = out_dir.join("summary.json");
    let text = serde_json::to_string_pretty(&summary).unwrap_or_default() + "\n";
    std::fs::write(&summary_path, text).map_err(io_error("write", &summary_path))?;
    say(format!(
        "\nDemo complete. Summary: {} | protocol transcript: {}",
        summary_path.display(),
        transcript_path.display()
    ))?;
    Ok(summary)
}

/// Runs one playtest in a fresh player process bound to `scene` of `project`.
fn playtest(
    config: &DemoConfig,
    project: &Path,
    scene: &str,
    replay_dir: &Path,
    label: &str,
    transcript: &Transcript,
) -> Result<(PlayReport, u32), DemoError> {
    let project_arg = project.display().to_string();
    let replay_arg = replay_dir.display().to_string();
    let args = [
        "serve",
        "--role",
        "player",
        "--project",
        &project_arg,
        "--scene",
        scene,
        "--replay-dir",
        &replay_arg,
    ];
    let mut session =
        EngineSession::launch(&config.engine, &args, label, Some(transcript.clone()))?;
    let process_id = session.process_id();
    let report = player::play(&mut session)?;
    session.finish()?;
    Ok((report, process_id))
}

fn play_summary((report, process_id): &(PlayReport, u32)) -> Value {
    json!({
        "process_id": process_id,
        "episode_id": report.episode_id,
        "planner_found_route": report.planned,
        "actions": report.actions,
        "outcome": report.outcome,
        "final_tick": report.final_tick,
        "replay": report.replay["path"],
    })
}

/// Runs `agentforge replay verify` as a separate process and parses its JSON report.
fn verify_in_fresh_process(
    engine: &Path,
    replay: &Path,
    project: &Path,
) -> Result<Value, DemoError> {
    let output = Command::new(engine)
        .args(["replay", "verify"])
        .arg(replay)
        .arg("--project")
        .arg(project)
        .output()
        .map_err(io_error("run", engine))?;
    let report: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        DemoError::Expectation(format!(
            "replay verify printed no JSON report (status {}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ))
    })?;
    expect(output.status.success(), || {
        format!("replay verify exited with {}: {report}", output.status)
    })?;
    Ok(report)
}
