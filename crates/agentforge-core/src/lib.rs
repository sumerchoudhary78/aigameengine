//! # agentforge-core
//!
//! The deterministic simulation kernel of AgentForge: a turn-based, fully observable
//! 2D integer-grid world with players, walls, keys, doors and exits.
//!
//! This crate performs no I/O and knows nothing about transports, sessions or model
//! providers. Everything it computes is a pure function of its inputs:
//!
//! * [`scene::Scene`] holds authoring data (grid, [`rules::Rules`], entities) and enforces
//!   structural invariants on every edit; batches of [`scene::SceneEdit`]s apply atomically.
//! * [`episode::Episode`] runs one play-through of a frozen scene. One accepted
//!   [`episode::Action`] advances exactly one logical tick; rejected actions change nothing.
//! * [`analysis`] answers reachability questions exhaustively and validates scenes.
//! * [`hash`] defines the canonical byte encoding behind every state hash.
//! * [`replay`] records accepted inputs plus per-tick hashes and re-verifies them.
//!
//! Determinism rules followed throughout: integer coordinates only, no floating point in
//! simulation state, ordered collections (`BTreeMap`/`Vec`) only, no randomness, no clocks.
#![warn(missing_docs)]
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod analysis;
pub mod board;
pub mod entity;
pub mod episode;
pub mod geometry;
pub mod hash;
pub mod ids;
pub mod layout;
pub mod project;
pub mod render;
pub mod replay;
pub mod rules;
pub mod scene;

pub(crate) mod movement;

/// Version of this engine build, recorded in saved projects and replays.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
