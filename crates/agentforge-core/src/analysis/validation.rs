//! Scene validation: completeness errors and gameplay warnings.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;

use super::{AnalysisError, UnreachableReason, Verdict, reachability};
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
    /// A portal pair id occurs exactly once.
    PortalUnpaired,
    /// A portal pair id occurs more than twice.
    PortalOverpaired,
    /// The player spawn shares a cell with a gate.
    PlayerOnGate,
    /// The player spawn shares a cell with a switch.
    PlayerOnSwitch,
    /// A gate group has no switch; its gates can never open.
    GateWithoutSwitch,
    /// A switch group has no gate; pressing it does nothing.
    SwitchWithoutGate,
    /// The solvability search refused to run over the state cap; no proof was attempted.
    AnalysisLimit,
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
    let mut portals: BTreeMap<u8, Vec<(EntityId, Position)>> = BTreeMap::new();
    for entity in scene.entities() {
        if let EntityKind::Portal { pair } = entity.kind {
            portals
                .entry(pair)
                .or_default()
                .push((entity.id, entity.position));
        }
    }
    if let Some(player) = scene.player() {
        for entity in scene.entities() {
            if entity.position != player.position {
                continue;
            }
            let issue = match entity.kind {
                EntityKind::Gate { group } => Some(Issue {
                    severity: Severity::Error,
                    code: IssueCode::PlayerOnGate,
                    message: format!(
                        "The player spawn at {} shares a cell with gate {} (group {group}); every gate starts closed, so the episode could not begin.",
                        player.position, entity.id
                    ),
                    entities: vec![entity.id],
                    position: Some(player.position),
                    hint: None,
                }),
                EntityKind::Switch { group } => Some(Issue {
                    severity: Severity::Error,
                    code: IssueCode::PlayerOnSwitch,
                    message: format!(
                        "The player spawn at {} shares a cell with switch {} (group {group}); toggles require an entry, so the start would silently differ from every subsequent tick.",
                        player.position, entity.id
                    ),
                    entities: vec![entity.id],
                    position: Some(player.position),
                    hint: None,
                }),
                _ => None,
            };
            issues.extend(issue);
        }
    }
    for (pair, members) in portals {
        match members.as_slice() {
            [(id, position)] => issues.push(Issue {
                severity: Severity::Error,
                code: IssueCode::PortalUnpaired,
                message: format!(
                    "Portal {id} at {position} is the only portal with pair {pair}; portals must come in pairs."
                ),
                entities: vec![*id],
                position: Some(*position),
                hint: Some(format!(
                    "Create a second portal with pair {pair}, or remove portal {id}."
                )),
            }),
            [_, _] => {}
            members => issues.push(Issue {
                severity: Severity::Error,
                code: IssueCode::PortalOverpaired,
                message: format!(
                    "Portal pair {pair} has {} portals ({}); exactly two are required.",
                    members.len(),
                    members
                        .iter()
                        .map(|(id, position)| format!("{id} at {position}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                entities: members.iter().map(|(id, _)| *id).collect(),
                position: None,
                hint: Some(format!(
                    "Keep exactly two portals with pair {pair} and remove or relink the others."
                )),
            }),
        }
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
    let mut switch_groups: BTreeMap<u8, Vec<(EntityId, Position)>> = BTreeMap::new();
    let mut gate_groups: BTreeMap<u8, Vec<(EntityId, Position)>> = BTreeMap::new();
    for entity in scene.entities() {
        match entity.kind {
            EntityKind::Switch { group } => {
                switch_groups
                    .entry(group)
                    .or_default()
                    .push((entity.id, entity.position));
            }
            EntityKind::Gate { group } => {
                gate_groups
                    .entry(group)
                    .or_default()
                    .push((entity.id, entity.position));
            }
            _ => {}
        }
    }

    if valid {
        match reachability(scene, None, None) {
            Ok(report) => {
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
            Err(AnalysisError::StateSpaceTooLarge { states, cap }) => {
                issues.push(Issue {
                    severity: Severity::Warning,
                    code: IssueCode::AnalysisLimit,
                    message: format!(
                        "The exhaustive solvability search would visit {states} (cell, keys, gates) states, over the cap of {cap}; no solvability proof was attempted."
                    ),
                    entities: Vec::new(),
                    position: None,
                    hint: Some(
                        "Reduce the number of distinct key colors or gate groups reachable, then ask analysis.reachability again."
                            .to_owned(),
                    ),
                });
            }
            Err(_) => {}
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

    for (group, members) in &gate_groups {
        if !switch_groups.contains_key(group) {
            issues.push(Issue {
                severity: Severity::Warning,
                code: IssueCode::GateWithoutSwitch,
                message: format!(
                    "Gate group {group} has no switch, so its {} gate(s) can never open. A deliberate fixed barrier should be a wall.",
                    members.len()
                ),
                entities: members.iter().map(|(id, _)| *id).collect(),
                position: None,
                hint: Some(
                    "Add a switch of this group somewhere reachable, or replace the gates with walls.".to_owned(),
                ),
            });
        }
    }
    for (group, members) in &switch_groups {
        if !gate_groups.contains_key(group) {
            issues.push(Issue {
                severity: Severity::Warning,
                code: IssueCode::SwitchWithoutGate,
                message: "Switch groups with no gate make pressing the switch a no-op.".to_owned(),
                entities: members.iter().map(|(id, _)| *id).collect(),
                position: None,
                hint: Some("Add a gate of this group, or relabel the switch's group.".to_owned()),
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
