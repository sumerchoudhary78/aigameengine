//! CLI entry points for the rigid-body simulation and its local protocol.
use super::RoleArg;
use agentforge_host::{
    physics::{self as host, PhysicsHost},
    server, storage,
};
use agentforge_physics::{
    episode::{Action, Episode},
    replay::Replay,
    scene::{PhysicsScene, PhysicsSceneDocument},
};
use clap::{Args, Subcommand};
use serde_json::json;
use std::{
    io::{self, BufWriter},
    path::{Path, PathBuf},
    process::ExitCode,
    time::Instant,
};

#[derive(Subcommand)]
pub enum PhysicsCommand {
    /// Run a fixed-role physics JSON Lines session over stdin/stdout.
    Serve(ServeArgs),
    /// Run scripted protocol clients: author, failing playtest, repair, win, fresh replay.
    Demo {
        #[arg(long, default_value = "out/physics")]
        out: PathBuf,
        #[arg(long, default_value = "fixtures")]
        fixtures: PathBuf,
    },
    /// Replay tools for physics recordings.
    #[command(subcommand)]
    Replay(ReplayCommand),
    /// Export generated schemas and examples of every physics operation.
    Schema {
        #[arg(long, default_value = "docs/physics.schema.json")]
        out: PathBuf,
        #[arg(long)]
        check: bool,
    },
    /// Measure simulation separately from in-process JSON protocol overhead.
    Bench {
        #[arg(long, default_value_t = 20)]
        worlds: u32,
        #[arg(long, default_value_t = 600)]
        ticks: u32,
    },
}
#[derive(Args)]
pub struct ServeArgs {
    #[arg(long, value_enum)]
    role: RoleArg,
    /// Versioned physics project saved by scene.save (required for players).
    #[arg(long)]
    project: Option<PathBuf>,
}
#[derive(Subcommand)]
pub enum ReplayCommand {
    Verify { replay: PathBuf },
}

pub fn run(command: PhysicsCommand) -> Result<ExitCode, String> {
    match command {
        PhysicsCommand::Serve(args) => {
            let scene = args
                .project
                .as_deref()
                .map(host::load_project)
                .transpose()?;
            let mut host = match args.role {
                RoleArg::Author => match (scene, args.project) {
                    (Some(scene), Some(path)) => PhysicsHost::author_with_project(scene, path)?,
                    (scene, _) => PhysicsHost::author(scene)?,
                },
                RoleArg::Player => {
                    PhysicsHost::player(scene.ok_or("physics player sessions require --project")?)?
                }
            };
            server::serve(
                &mut host,
                io::stdin().lock(),
                BufWriter::new(io::stdout().lock()),
            )
            .map_err(|e| e.to_string())?;
        }
        PhysicsCommand::Demo { out, fixtures } => {
            let engine = std::env::current_exe().map_err(|e| e.to_string())?;
            let report = agentforge_demo::physics::run(&engine, &out, &fixtures)?;
            let summary = json!({
                "clients": report["clients"],
                "first_attempt": report["first_attempt"],
                "diagnosis": report["diagnosis"]["summary"],
                "second_attempt": report["second_attempt"],
                "second_scene": report["second_scene"],
                "replay": report["replay"],
                "fresh_process_verification": report["fresh_process_verification"],
                "summary": out.join("summary.json"),
                "transcript": report["transcript"],
                "svg": report["svg"],
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())?
            );
        }
        PhysicsCommand::Replay(ReplayCommand::Verify { replay }) => {
            let value: serde_json::Value = read_bounded_json(&replay)?;
            if value["format"] != "agentforge.physics.replay" || value["format_version"] != 1 {
                return Err("incompatible physics replay format/version".into());
            }
            let replay: Replay = serde_json::from_value(value).map_err(|e| e.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&replay.verify().map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?
            );
        }
        PhysicsCommand::Schema { out, check } => {
            let text =
                serde_json::to_string_pretty(&host::schemas()).map_err(|e| e.to_string())? + "\n";
            if check {
                if std::fs::read_to_string(&out).ok().as_deref() != Some(text.as_str()) {
                    return Err(format!("{} is stale: run physics schema", out.display()));
                }
            } else {
                storage::write_atomic(&out, text.as_bytes()).map_err(|e| e.to_string())?;
            }
        }
        PhysicsCommand::Bench { worlds, ticks } => bench(worlds, ticks)?,
    }
    Ok(ExitCode::SUCCESS)
}
fn read_bounded_json(path: &Path) -> Result<serde_json::Value, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(storage::MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > storage::MAX_FILE_BYTES {
        return Err("physics file exceeds size limit".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
fn bench(worlds: u32, ticks: u32) -> Result<(), String> {
    if worlds == 0 || worlds > 10_000 || ticks == 0 || ticks > 36_000 {
        return Err("worlds must be 1..10000 and ticks 1..36000".into());
    }
    let mut document: PhysicsSceneDocument =
        serde_json::from_str(include_str!("../../../fixtures/physics_drop.json"))
            .map_err(|e| e.to_string())?;
    document.settings.max_ticks = ticks;
    // Keep the goal off the falling body's trajectory, letting it settle on the floor.
    document
        .bodies
        .iter_mut()
        .find(|b| b.sensor)
        .ok_or("no sensor")?
        .position
        .x = 8.0;
    let scene = PhysicsScene::from_document(document.clone()).map_err(|e| e.to_string())?;
    let mut direct = (0..worlds)
        .map(|_| Episode::start(&scene).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let start = Instant::now();
    for world in &mut direct {
        for _ in 0..ticks {
            std::hint::black_box(world.apply(Action::Wait).map_err(|e| e.to_string())?);
        }
    }
    let simulation = start.elapsed();
    let mut hosts = Vec::new();
    for _ in 0..worlds {
        let mut host = PhysicsHost::author(Some(document.clone()))?;
        let response=host.handle_line(&json!({"request_id":"start","protocol_version":1,"operation":"episode.start","arguments":{}}).to_string());
        if response.error.is_some() {
            return Err(format!("start failed: {:?}", response.error));
        }
        hosts.push(host);
    }
    let inputs=(0..ticks).map(|tick|json!({"request_id":format!("t{tick}"),"protocol_version":1,"operation":"episode.act","arguments":{"action":{"type":"wait"},"expected_episode_id":1,"expected_tick":tick}}).to_string()).collect::<Vec<_>>();
    let start = Instant::now();
    let mut response_bytes = 0usize;
    for host in &mut hosts {
        for input in &inputs {
            let response = host.handle_line(input);
            if response.error.is_some() {
                return Err(format!("tick failed: {:?}", response.error));
            }
            response_bytes += serde_json::to_vec(&response)
                .map_err(|e| e.to_string())?
                .len();
        }
    }
    let protocol = start.elapsed();
    let total = u64::from(worlds) * u64::from(ticks);
    println!(
        "{}",
        json!({"build_mode":if cfg!(debug_assertions){"debug"}else{"release"},"world_count":worlds,"ticks_per_world":ticks,"total_ticks":total,"workload":"three bodies, falling ball settles on fixed floor, one off-path sensor; fixed 60Hz, SHA-256 every tick; sequential one thread; initialization excluded","simulation_elapsed_ms":simulation.as_secs_f64()*1000.0,"protocol_elapsed_ms":protocol.as_secs_f64()*1000.0,"protocol_response_bytes":response_bytes,"protocol_scope":"in-process JSON parse/dispatch/response serialization including full observation; no pipes or process startup","os":std::env::consts::OS,"arch":std::env::consts::ARCH})
    );
    Ok(())
}
