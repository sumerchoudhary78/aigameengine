//! `agentforge bench`: reproducible headless throughput measurement.
//!
//! The workload is fixed and described in the output. Three layers are timed separately so
//! simulation cost is never confused with protocol or process overhead:
//!
//! 1. `simulation`: `Episode::apply` directly (includes the per-tick SHA-256 state hash).
//! 2. `protocol_in_process`: `Host::handle_line` on JSON strings (parse, dispatch,
//!    serialize), with and without the observation in each response.
//! 3. `process_pipe`: a child `agentforge serve` process over stdin/stdout pipes, one
//!    request in flight at a time.
//!
//! Worlds run sequentially on one thread. Results are measurements of one run on one
//! machine, not targets.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use agentforge_core::entity::EntityKind;
use agentforge_core::episode::{Action, Episode};
use agentforge_core::geometry::{Direction, GridSize, Position};
use agentforge_core::project::{Name, Project};
use agentforge_core::rules::{Rules, WinCondition};
use agentforge_core::scene::{Scene, SceneEdit};
use agentforge_host::{Host, storage};
use clap::Args;
use serde_json::{Value, json};

/// Arguments of `agentforge bench`.
#[derive(Args)]
pub struct BenchArgs {
    /// Independent worlds (episodes) for the simulation and in-process modes.
    #[arg(long, default_value_t = 200)]
    worlds: u32,
    /// Ticks per world (at most 9999, so no world ends early).
    #[arg(long, default_value_t = 2000)]
    ticks: u32,
    /// Worlds for the child-process mode (each runs `--ticks` ticks).
    #[arg(long, default_value_t = 1)]
    process_worlds: u32,
}

const SCENE: &str = "bench_room";
const MAX_TICKS: u32 = 10_000;

pub fn run(args: &BenchArgs) -> Result<ExitCode, String> {
    if args.ticks == 0 || args.ticks >= MAX_TICKS || args.worlds == 0 {
        return Err(format!(
            "--worlds must be >= 1 and --ticks between 1 and {}",
            MAX_TICKS - 1
        ));
    }
    let scene = bench_scene()?;
    let mut results = vec![
        simulation(&scene, args)?,
        protocol_in_process(&scene, args, false)?,
        protocol_in_process(&scene, args, true)?,
    ];
    if args.process_worlds > 0 {
        results.push(process_pipe(&scene, args)?);
    }
    let report = json!({
        "build_profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "engine_version": agentforge_core::ENGINE_VERSION,
        "environment": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "available_parallelism": std::thread::available_parallelism().map(usize::from).unwrap_or(1),
        },
        "workload": {
            "description": "16x16 room with border walls; the player alternates east/west moves, so every action is legal and no episode ends; every tick computes the SHA-256 state hash",
            "worlds": args.worlds,
            "ticks_per_world": args.ticks,
            "process_worlds": args.process_worlds,
            "threads": 1,
        },
        "results": results,
        "note": "Sequential, single-threaded measurements of this run on this machine; not targets or guarantees.",
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    );
    Ok(ExitCode::SUCCESS)
}

fn bench_scene() -> Result<Scene, String> {
    let size = GridSize::new(16, 16);
    let mut edits = Vec::new();
    for y in 0..16 {
        for x in 0..16 {
            let kind = if x == 0 || y == 0 || x == 15 || y == 15 {
                Some(EntityKind::Wall)
            } else if (x, y) == (1, 1) {
                Some(EntityKind::Player)
            } else if (x, y) == (14, 14) {
                Some(EntityKind::Exit)
            } else {
                None
            };
            if let Some(kind) = kind {
                edits.push(SceneEdit::CreateEntity {
                    kind,
                    position: Position::new(x, y),
                    label: None,
                });
            }
        }
    }
    let rules = Rules {
        max_ticks: MAX_TICKS,
        win_condition: WinCondition::ReachExit,
    };
    let scene = Scene::new(size, rules).map_err(|e| e.to_string())?;
    scene
        .apply_edits(&edits)
        .map(|(scene, _)| scene)
        .map_err(|e| e.to_string())
}

fn action(tick: u32) -> Action {
    let direction = if tick.is_multiple_of(2) {
        Direction::East
    } else {
        Direction::West
    };
    Action::Move { direction }
}

fn measurement(mode: &str, worlds: u32, ticks: u32, elapsed: Duration, notes: &str) -> Value {
    let total = u64::from(worlds) * u64::from(ticks);
    let seconds = elapsed.as_secs_f64();
    json!({
        "mode": mode,
        "worlds": worlds,
        "ticks_per_world": ticks,
        "total_ticks": total,
        "elapsed_ms": (seconds * 1000.0 * 1000.0).round() / 1000.0,
        "ticks_per_second": if seconds > 0.0 { (total as f64 / seconds).round() } else { 0.0 },
        "microseconds_per_tick": if total > 0 { (seconds * 1e6 / total as f64 * 1000.0).round() / 1000.0 } else { 0.0 },
        "notes": notes,
    })
}

fn simulation(scene: &Scene, args: &BenchArgs) -> Result<Value, String> {
    let mut final_hashes = Vec::new();
    let started = Instant::now();
    for _ in 0..args.worlds {
        let mut episode = Episode::start(scene).map_err(|e| e.to_string())?;
        for tick in 0..args.ticks {
            episode.apply(action(tick)).map_err(|e| e.to_string())?;
        }
        final_hashes.push(episode.current_hash());
    }
    let elapsed = started.elapsed();
    // Identical worlds must end in identical states; checking it also keeps the work observable.
    if final_hashes.windows(2).any(|pair| pair[0] != pair[1]) {
        return Err("determinism violation: identical worlds diverged".to_owned());
    }
    Ok(measurement(
        "simulation",
        args.worlds,
        args.ticks,
        elapsed,
        "agentforge_core Episode::apply only (rule resolution + state hash)",
    ))
}

fn protocol_in_process(
    scene: &Scene,
    args: &BenchArgs,
    with_observation: bool,
) -> Result<Value, String> {
    let mut project = Project::new(Name::new("bench").map_err(|e| e.to_string())?);
    project.set_scene(Name::new(SCENE).map_err(|e| e.to_string())?, scene.clone());
    let requests: Vec<String> = (0..args.ticks)
        .map(|tick| {
            json!({
                "request_id": tick.to_string(),
                "protocol_version": 1,
                "operation": "episode.act",
                "arguments": {"action": action(tick), "expected_tick": tick, "include_observation": with_observation},
            })
            .to_string()
        })
        .collect();
    let start_request = json!({"request_id": "start", "protocol_version": 1, "operation": "episode.start", "arguments": {"scene": SCENE}}).to_string();
    let mut response_bytes = 0usize;
    let started = Instant::now();
    for _ in 0..args.worlds {
        let mut host = Host::author_with_project(project.clone(), None);
        host.handle_line(&start_request);
        for request in &requests {
            let response = host.handle_line(request);
            if response.error.is_some() {
                return Err(format!("unexpected error response: {:?}", response.error));
            }
            response_bytes += serde_json::to_string(&response)
                .map_err(|e| e.to_string())?
                .len();
        }
    }
    let elapsed = started.elapsed();
    let mut result = measurement(
        if with_observation {
            "protocol_in_process_with_observation"
        } else {
            "protocol_in_process"
        },
        args.worlds,
        args.ticks,
        elapsed,
        if with_observation {
            "Host::handle_line incl. JSON parse/dispatch/serialize; each response includes the full observation"
        } else {
            "Host::handle_line incl. JSON parse/dispatch/serialize; include_observation=false"
        },
    );
    result["average_response_bytes"] =
        json!(response_bytes / (args.worlds as usize * args.ticks as usize).max(1));
    Ok(result)
}

fn process_pipe(scene: &Scene, args: &BenchArgs) -> Result<Value, String> {
    let directory = std::env::temp_dir().join(format!("agentforge-bench-{}", std::process::id()));
    std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let project_path = directory.join("bench.agentforge.json");
    let mut project = Project::new(Name::new("bench").map_err(|e| e.to_string())?);
    project.set_scene(Name::new(SCENE).map_err(|e| e.to_string())?, scene.clone());
    storage::save_project(&project_path, &project).map_err(|e| e.to_string())?;

    let engine = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(engine)
        .args(["serve", "--role", "author", "--project"])
        .arg(&project_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("cannot start engine process: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("no child stdin")?;
    let mut stdout = BufReader::new(child.stdout.take().ok_or("no child stdout")?);
    let mut line = String::new();
    let mut roundtrip = |request: &Value, line: &mut String| -> Result<(), String> {
        writeln!(stdin, "{request}").map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())?;
        line.clear();
        stdout.read_line(line).map_err(|e| e.to_string())?;
        if line.contains("\"status\":\"error\"") {
            return Err(format!("unexpected error response: {line}"));
        }
        Ok(())
    };
    let mut elapsed = Duration::ZERO;
    for world in 0..args.process_worlds {
        roundtrip(
            &json!({"request_id": format!("start-{world}"), "protocol_version": 1, "operation": "episode.start", "arguments": {"scene": SCENE}}),
            &mut line,
        )?;
        let started = Instant::now();
        for tick in 0..args.ticks {
            roundtrip(
                &json!({"request_id": tick.to_string(), "protocol_version": 1, "operation": "episode.act", "arguments": {"action": action(tick), "expected_tick": tick, "include_observation": false}}),
                &mut line,
            )?;
        }
        elapsed += started.elapsed();
    }
    drop(roundtrip);
    // Closing stdin ends the session; the child then exits.
    drop(stdin);
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&directory);
    if !status.success() {
        return Err(format!("engine process exited with {status}"));
    }
    Ok(measurement(
        "process_pipe",
        args.process_worlds,
        args.ticks,
        elapsed,
        "child `agentforge serve --role author` over stdin/stdout pipes; one request in flight; include_observation=false; process start-up excluded",
    ))
}
