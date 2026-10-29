//! Scene validation: completeness errors and gameplay warnings.

use schemars::JsonSchema;
use serde::Serialize;

use super::{UnreachableReason, Verdict, reachability};
use crate::entity::EntityKind;
use crate::geometry::Position;
use crate::ids::EntityId;
use crate::scene::Scene;

/// How serious an issue is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Prevents starting an episode.
    Error,
    /// The scene can be played but has a problem (for example, it cannot be won).
    Warning,
}

/// Stable machine-readable issue identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IssueCode {
    /// No player spawn.
    MissingPlayer,
    /// No exit.
    MissingExit,
    /// No gameplay path from the spawn satisfies the win condition.
    ObjectiveUnreachable,
    /// The objective is reachable, but not within `max_ticks`.
    ExceedsMaxTicks,
    /// A door whose key color does not exist in the scene.
    DoorWithoutKey,
    /// A key the player can never pick up.
    KeyUnobtainable,
}

/// One validation finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Issue {
    /// Error or warning.
    pub severity: Severity,
    /// Stable identifier.
    pub code: IssueCode,
    /// Human- and model-readable explanation.
    pub message: String,
    /// Entities involved.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<EntityId>,
    /// Cell involved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    /// Suggested next step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Result of [`validate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ValidationReport {
    /// No errors: an episode can start.
    pub valid: bool,
    /// Valid and winnable within `max_ticks`.
    pub playable: bool,
    /// Length of the shortest winning move sequence, ignoring `max_ticks`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shortest_solution_length: Option<u32>,
    /// Findings, errors first.
    pub issues: Vec<Issue>,
}

/// Errors that prevent an episode from starting.
pub fn completeness_errors(scene: &Scene) -> Vec<Issue> {
    let mut issues = Vec::new();
    if scene.player().is_none() {
        issues.push(Issue {
            severity: Severity::Error,
            code: IssueCode::MissingPlayer,
            message: "The scene has no player spawn.".to_owned(),
            entities: Vec::new(),
            position: None,
            hint: Some("Create an entity with kind {\"type\":\"player\"}.".to_owned()),
        });
    }
    if scene.exits().next().is_none() {
        issues.push(Issue {
            severity: Severity::Error,
            code: IssueCode::MissingExit,
            message: "The scene has no exit.".to_owned(),
            entities: Vec::new(),
            position: None,
            hint: Some("Create an entity with kind {\"type\":\"exit\"}.".to_owned()),
        });
    }
    issues
}

/// Validates a scene: completeness errors, then gameplay warnings from an exhaustive
/// solvability search.
pub fn validate(scene: &Scene) -> ValidationReport {
    let mut issues = completeness_errors(scene);
    let valid = issues.is_empty();
    let mut playable = false;
    let mut shortest_solution_length = None;
    let key_colors = scene.key_colors();

    if valid && let Ok(report) = reachability(scene, None, None) {
        playable = report.verdict == Verdict::Solvable;
        shortest_solution_length = report.gameplay.path_length;
        match report.gameplay.unreachable_reason {
            Some(UnreachableReason::NoPath) => issues.push(Issue {
                severity: Severity::Warning,
                code: IssueCode::ObjectiveUnreachable,
                message: report.summary.clone(),
                entities: report.blockers.iter().map(|b| b.entity).collect(),
                position: None,
                hint: Some(
                    "Call analysis.reachability for every blocker and the verified single-removal repairs."
                        .to_owned(),
                ),
            }),
            Some(UnreachableReason::ExceedsMaxTicks {
                shortest_path_length,
                max_ticks,
            }) => issues.push(Issue {
                severity: Severity::Warning,
                code: IssueCode::ExceedsMaxTicks,
                message: format!(
                    "The shortest solution needs {shortest_path_length} moves but max_ticks is {max_ticks}."
                ),
                entities: Vec::new(),
                position: None,
                hint: Some(format!(
                    "Raise rules.max_ticks to at least {shortest_path_length}, or shorten the route."
                )),
            }),
            None => {}
        }
        for entity in scene.entities() {
            if let EntityKind::Key { color } = entity.kind
                && !report.gameplay.obtainable_keys.contains(&color)
            {
                issues.push(Issue {
                    severity: Severity::Warning,
                    code: IssueCode::KeyUnobtainable,
                    message: format!(
                        "The {color} key {} at {} cannot be reached from the player spawn.",
                        entity.id, entity.position
                    ),
                    entities: vec![entity.id],
                    position: Some(entity.position),
                    hint: None,
                });
            }
        }
    }

    for entity in scene.entities() {
        if let EntityKind::Door { color } = entity.kind
            && !key_colors.contains(color)
        {
            issues.push(Issue {
                severity: Severity::Warning,
                code: IssueCode::DoorWithoutKey,
                message: format!(
                    "Door {} at {} needs a {color} key, but the scene has no {color} key.",
                    entity.id, entity.position
                ),
                entities: vec![entity.id],
                position: Some(entity.position),
                hint: Some(format!(
                    "Add a {color} key the player can reach, or remove/recolor the door."
                )),
            });
        }
    }

    ValidationReport {
        valid,
        playable,
        shortest_solution_length,
        issues,
    }
}
