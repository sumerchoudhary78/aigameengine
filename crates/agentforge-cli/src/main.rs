//! `agentforge`: the AgentForge engine command.
//!
//! * `serve` runs one session over JSON Lines on stdin/stdout (logs on stderr).
//! * `demo` runs the scripted author/player loop against child `serve` processes.
//! * `replay verify` re-executes a replay in this (fresh) process.
//! * `schema` writes or checks the generated protocol documentation.
//! * `bench` measures simulation and protocol throughput.
//! * `physics` serves, demonstrates, verifies and measures rigid-body worlds.

mod bench;
mod physics;

use std::io::{self, BufWriter};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use agentforge_core::hash::scene_digest;
use agentforge_core::project::Name;
use agentforge_host::protocol::catalog;
use agentforge_host::{Host, Role, mcp, server, storage};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "agentforge",
    version,
    about = "Agent-operated grid and rigid-body game engine"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fixed-timestep 2D rigid-body physics tools.
    #[command(subcommand)]
    Physics(physics::PhysicsCommand),
    /// Serve one session over JSON Lines on stdin/stdout. Logs go to stderr
    /// (filter with AGENTFORGE_LOG, e.g. AGENTFORGE_LOG=debug).
    Serve(ServeArgs),
    /// Serve one long-lived session as Model Context Protocol tools over stdin/stdout.
    /// Logs go to stderr; stdout carries only JSON-RPC messages.
    Mcp(ServeArgs),
    /// Run the end-to-end demo: author creates a broken dungeon, a player fails, the author
    /// repairs it through the protocol, the player wins, and a fresh process verifies the
    /// replay. Clients are deterministic scripts, not language models.
    Demo(DemoArgs),
    /// Replay tools.
    #[command(subcommand)]
    Replay(ReplayCommand),
    /// Write (or check) the generated protocol schema and operation reference.
    Schema(SchemaArgs),
    /// Measure simulation, in-process protocol and child-process throughput.
    Bench(bench::BenchArgs),
}

#[derive(Clone, Copy, ValueEnum)]
enum RoleArg {
    Author,
    Player,
}

#[derive(Args)]
struct ServeArgs {
    /// Session capability, fixed for the life of the process.
    #[arg(long, value_enum)]
    role: RoleArg,
    /// Project file to open at start (required for players).
    #[arg(long)]
    project: Option<PathBuf>,
    /// Scene a player session is bound to (players only).
    #[arg(long)]
    scene: Option<String>,
    /// Directory where player replay exports are written (players only).
    #[arg(long)]
    replay_dir: Option<PathBuf>,
}

#[derive(Args)]
struct DemoArgs {
    /// Directory for the project, replays, SVGs, transcript and summary.
    #[arg(long, default_value = "out/demo")]
    out: PathBuf,
    /// Directory containing key_vault.agentforge.json (the second scene).
    #[arg(long, default_value = "fixtures")]
    fixtures: PathBuf,
}

#[derive(Subcommand)]
enum ReplayCommand {
    /// Re-execute a replay and compare every state hash and the outcome. Prints a JSON
    /// report; exits 0 only if everything matched.
    Verify {
        /// Replay file.
        replay: PathBuf,
        /// Also check the replay starts from this project's scene.
        #[arg(long)]
        project: Option<PathBuf>,
        /// Scene in --project (defaults to the scene name recorded in the replay).
        #[arg(long)]
        scene: Option<String>,
    },
}

#[derive(Args)]
struct SchemaArgs {
    /// Output directory.
    #[arg(long, default_value = "docs/protocol")]
    out: PathBuf,
    /// Compare with the files in --out instead of writing; exit 1 if they differ.
    #[arg(long)]
    check: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging();
    let result = match cli.command {
        Command::Physics(command) => physics::run(command),
        Command::Serve(args) => serve(args),
        Command::Mcp(args) => serve_mcp(args),
        Command::Demo(args) => demo(&args),
        Command::Replay(ReplayCommand::Verify {
            replay,
            project,
            scene,
        }) => verify_replay(&replay, project.as_deref(), scene),
        Command::Schema(args) => schema(&args),
        Command::Bench(args) => bench::run(&args),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("agentforge: {message}");
            ExitCode::from(2)
        }
    }
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("AGENTFORGE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_target(false)
        .init();
}

fn build_host(args: ServeArgs) -> Result<Host, String> {
    let load = |path: &Path| storage::load_project(path).map_err(|e| e.to_string());
    match args.role {
        RoleArg::Author => {
            if args.scene.is_some() || args.replay_dir.is_some() {
                return Err("--scene and --replay-dir only apply to player sessions".to_owned());
            }
            match args.project {
                Some(path) => Ok(Host::author_with_project(load(&path)?, Some(path))),
                None => Ok(Host::author()),
            }
        }
        RoleArg::Player => {
            let path = args.project.ok_or("player sessions require --project")?;
            let scene = args.scene.ok_or("player sessions require --scene")?;
            if let Some(directory) = &args.replay_dir {
                std::fs::create_dir_all(directory)
                    .map_err(|e| format!("cannot create {}: {e}", directory.display()))?;
            }
            Host::player(load(&path)?, &scene, args.replay_dir).map_err(|e| e.to_string())
        }
    }
}

fn serve(args: ServeArgs) -> Result<ExitCode, String> {
    let mut host = build_host(args)?;
    let role = match host.role() {
        Role::Author => "author",
        Role::Player => "player",
    };
    tracing::info!(role, "serving JSON Lines on stdin/stdout");
    let stats = server::serve(
        &mut host,
        io::stdin().lock(),
        BufWriter::new(io::stdout().lock()),
    )
    .map_err(|e| format!("stream error: {e}"))?;
    tracing::info!(
        requests = stats.requests,
        errors = stats.errors,
        "input closed"
    );
    Ok(ExitCode::SUCCESS)
}

fn serve_mcp(args: ServeArgs) -> Result<ExitCode, String> {
    let mut host = build_host(args)?;
    let role = host.role();
    let tools = mcp::grid_tools(role);
    let info = mcp::ServerInfo {
        name: "agentforge".to_owned(),
        version: agentforge_core::ENGINE_VERSION.to_owned(),
        instructions: grid_instructions(role),
    };
    tracing::info!(
        role = role.as_str(),
        tools = tools.len(),
        "serving MCP on stdin/stdout"
    );
    let stats = mcp::serve(
        &mut host,
        tools,
        info,
        io::stdin().lock(),
        BufWriter::new(io::stdout().lock()),
    )
    .map_err(|e| format!("stream error: {e}"))?;
    tracing::info!(
        requests = stats.requests,
        errors = stats.errors,
        "input closed"
    );
    Ok(ExitCode::SUCCESS)
}

fn grid_instructions(role: Role) -> String {
    format!(
        "AgentForge grid engine (role: {}). Tools are engine operations with dots replaced by \
         underscores (entity.create is entity_create). Call discover first: it returns the \
         game model, conventions, limits and an example for every operation. Successful calls \
         return {{revision, result}} and engine failures return isError with {{revision, error}}; \
         mutating tools require expected_revision, so read the revision from the last response. \
         After stale_revision, read session_status or scene_get and retry with the current \
         revision instead of resending blindly. episode_act requires expected_episode_id and \
         expected_tick. The role is fixed by the launcher; a player session is bound to one \
         scene and cannot author.",
        role.as_str()
    )
}

fn demo(args: &DemoArgs) -> Result<ExitCode, String> {
    let engine =
        std::env::current_exe().map_err(|e| format!("cannot locate own executable: {e}"))?;
    let config = agentforge_demo::DemoConfig {
        engine,
        out_dir: args.out.clone(),
        fixtures_dir: args.fixtures.clone(),
    };
    match agentforge_demo::run(&config, &mut io::stdout()) {
        Ok(_) => Ok(ExitCode::SUCCESS),
        Err(error) => Err(format!("demo failed: {error}")),
    }
}

fn verify_replay(
    path: &Path,
    project: Option<&Path>,
    scene: Option<String>,
) -> Result<ExitCode, String> {
    let replay = storage::load_replay(path).map_err(|e| e.to_string())?;
    let verification = agentforge_core::replay::verify(&replay).map_err(|e| e.to_string())?;
    let scene_matches = match project {
        Some(project_path) => {
            let project = storage::load_project(project_path).map_err(|e| e.to_string())?;
            let scene_name = scene
                .or_else(|| replay.scene_name.clone())
                .ok_or("the replay records no scene name; pass --scene")?;
            let name = Name::new(&scene_name).map_err(|e| e.to_string())?;
            let scene = project
                .scene(&name)
                .ok_or_else(|| format!("project has no scene {scene_name:?}"))?;
            Some(scene_digest(scene) == replay.scene_digest)
        }
        None => None,
    };
    let ok = verification.verified && scene_matches != Some(false);
    let report = json!({
        "replay": path.display().to_string(),
        "verified": ok,
        "verification": verification,
        "project_scene_matches": scene_matches,
        "process_id": std::process::id(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    );
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn schema(args: &SchemaArgs) -> Result<ExitCode, String> {
    let schema = serde_json::to_string_pretty(&catalog::protocol_schema())
        .map_err(|e| e.to_string())?
        + "\n";
    let files = [
        (args.out.join("protocol.schema.json"), schema),
        (
            args.out.join("operations.md"),
            catalog::operations_markdown(),
        ),
    ];
    if args.check {
        let mut stale = Vec::new();
        for (path, expected) in &files {
            if std::fs::read_to_string(path).ok().as_deref() != Some(expected.as_str()) {
                stale.push(path.display().to_string());
            }
        }
        if stale.is_empty() {
            println!("protocol documentation is up to date");
            return Ok(ExitCode::SUCCESS);
        }
        eprintln!(
            "out of date: {} (run `cargo run -p agentforge-cli -- schema`)",
            stale.join(", ")
        );
        return Ok(ExitCode::FAILURE);
    }
    std::fs::create_dir_all(&args.out)
        .map_err(|e| format!("cannot create {}: {e}", args.out.display()))?;
    for (path, content) in &files {
        storage::write_atomic(path, content.as_bytes()).map_err(|e| e.to_string())?;
        println!("wrote {}", path.display());
    }
    Ok(ExitCode::SUCCESS)
}
