//! # agentforge-demo
//!
//! Deterministic, scripted **protocol clients** for AgentForge. They are not language
//! models and their success is not evidence of autonomous problem solving; they exist to
//! prove that the whole create → test → diagnose → repair → retest → save → replay loop
//! works through the public JSON Lines protocol alone.
//!
//! This crate deliberately depends on no engine crate. Clients start `agentforge serve`
//! child processes and exchange JSON lines with them, exactly as an external agent would,
//! so they cannot reach engine internals even by accident.
#![warn(missing_docs)]

pub mod builder;
pub mod client;
pub mod physics;
pub mod player;
pub mod scenario;

pub use scenario::{DemoConfig, DemoError, run};
