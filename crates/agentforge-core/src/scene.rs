//! Scenes: authoring data guarded by structural invariants, edited in atomic batches.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::entity::{Entity, EntityKind, KeyColor, KeySet};
use crate::geometry::{GridSize, Position};
use crate::ids::{EntityId, IdAllocator};
use crate::rules::{Rules, RulesPatch, WinCondition};

/// Longest accepted entity label, in characters.
pub const MAX_LABEL_CHARS: usize = 64;

/// Most edits accepted in one batch.
pub const MAX_BATCH_EDITS: usize = 4096;

/// Authoring data of one level: grid size, rules and entities.
///
/// Invariants, enforced by every constructor and every edit:
/// * both grid dimensions lie in `1..=64`;
/// * every entity lies inside the grid, and a cell holds at most one entity;
/// * at most one player, and at most one key per color;
/// * every entity id is below the allocator's next id;
/// * rules and labels are valid.
///
/// Completeness (exactly one player, at least one exit) is deliberately *not* an
/// invariant, so scenes can be built incrementally. Validation reports it, and starting
/// an episode requires it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scene {
    size: GridSize,
    rules: Rules,
    entities: BTreeMap<EntityId, Entity>,
    ids: IdAllocator,
}

/// Serialized form of a [`Scene`], used by project files, snapshots and replays.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SceneDocument {
    /// Grid dimensions.
    pub size: GridSize,
    /// Rule parameters.
    pub rules: Rules,
    /// Identifier the next created entity will receive (ID allocator state).
    pub next_entity_id: u32,
    /// Entities in ascending id order.
    pub entities: Vec<Entity>,
}

/// Why a scene construction or edit was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SceneError {
    /// Grid dimensions outside `1..=64`.
    #[error("grid size {size} is not supported; width and height must be between 1 and {max}")]
    InvalidGridSize {
        /// The rejected size.
        size: GridSize,
        /// Largest supported dimension.
        max: u32,
    },
    /// A position outside the grid.
    #[error("position {position} is outside the {size} grid")]
    OutOfBounds {
        /// The rejected position.
        position: Position,
        /// The grid size.
        size: GridSize,
    },
    /// Target cell already holds an entity.
    #[error("cell {position} is already occupied by {occupant_kind} {occupant}")]
    CellOccupied {
        /// The contested cell.
        position: Position,
        /// The entity already there.
        occupant: EntityId,
        /// Its kind.
        occupant_kind: EntityKind,
    },
    /// No entity with this id exists.
    #[error("entity {id} does not exist in this scene")]
    EntityNotFound {
        /// The missing id.
        id: EntityId,
    },
    /// A second player was requested.
    #[error("the scene already has a player ({existing}); a scene holds at most one")]
    DuplicatePlayer {
        /// The existing player entity.
        existing: EntityId,
    },
    /// A second key of the same color was requested.
    #[error("the scene already has a {color} key ({existing}); each key color may appear once")]
    DuplicateKeyColor {
        /// The duplicated color.
        color: KeyColor,
        /// The existing key entity.
        existing: EntityId,
    },
    /// Label too long or containing control characters.
    #[error("invalid label: {reason}")]
    InvalidLabel {
        /// What is wrong with it.
        reason: String,
    },
    /// Rule parameter out of range.
    #[error("invalid rules: {field} {reason}")]
    InvalidRules {
        /// The offending field.
        field: &'static str,
        /// What is wrong with it.
        reason: String,
    },
    /// An update that would change nothing.
    #[error("{op} must change at least one field")]
    EmptyUpdate {
        /// The edit operation.
        op: &'static str,
    },
    /// The same id appears twice in a document.
    #[error("entity id {id} appears more than once")]
    DuplicateEntityId {
        /// The duplicated id.
        id: EntityId,
    },
    /// A document entity id outside `1..next_entity_id`.
    #[error("entity id {id} is outside the allocated range 1..{next_entity_id}")]
    IdOutsideAllocatedRange {
        /// The offending id.
        id: EntityId,
        /// The document's allocator state.
        next_entity_id: u32,
    },
    /// The allocator cannot produce another id.
    #[error("entity id space is exhausted")]
    IdSpaceExhausted,
}

/// One authoring edit. Batches apply atomically through [`Scene::apply_edits`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum SceneEdit {
    /// Create an entity; the engine assigns its id.
    CreateEntity {
        /// What to create.
        kind: EntityKind,
        /// Where to place it.
        position: Position,
        /// Optional authoring note (empty string means none).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Change an entity's kind, position and/or label. At least one field is required.
    UpdateEntity {
        /// The entity to change.
        id: EntityId,
        /// New kind.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<EntityKind>,
        /// New position.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<Position>,
        /// New label; an empty string clears it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Remove an entity. Its id is never reused.
    RemoveEntity {
        /// The entity to remove.
        id: EntityId,
    },
    /// Change rule parameters. At least one field is required.
    UpdateRules {
        /// New `max_ticks`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_ticks: Option<u32>,
        /// New win condition.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        win_condition: Option<WinCondition>,
    },
}

/// What an accepted edit did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum EditEffect {
    /// An entity was created.
    Created {
        /// The new entity, including its assigned id.
        entity: Entity,
    },
    /// An entity was changed.
    Updated {
        /// State before the edit.
        before: Entity,
        /// State after the edit.
        after: Entity,
    },
    /// An entity was removed.
    Removed {
        /// The removed entity.
        entity: Entity,
    },
    /// Rules were changed.
    RulesUpdated {
        /// Rules before the edit.
        before: Rules,
        /// Rules after the edit.
        after: Rules,
    },
}

/// A rejected batch: the first failing edit and why. The scene is left unchanged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("edit {index} was rejected: {error}")]
pub struct BatchError {
    /// Zero-based index of the failing edit within the batch.
    pub index: usize,
    /// Why it failed.
    pub error: SceneError,
}

impl Scene {
    /// Creates an empty scene.
    pub fn new(size: GridSize, rules: Rules) -> Result<Self, SceneError> {
        check_size(size)?;
        check_rules(rules)?;
        Ok(Self {
            size,
            rules,
            entities: BTreeMap::new(),
            ids: IdAllocator::new(),
        })
    }

    /// Rebuilds a scene from its document, checking every invariant.
    pub fn from_document(document: SceneDocument) -> Result<Self, SceneError> {
        let mut scene = Self::new(document.size, document.rules)?;
        scene.ids = IdAllocator::starting_at(document.next_entity_id);
        for entity in document.entities {
            let raw = entity.id.get();
            if raw < IdAllocator::FIRST || raw >= document.next_entity_id {
                return Err(SceneError::IdOutsideAllocatedRange {
                    id: entity.id,
                    next_entity_id: document.next_entity_id,
                });
            }
            if scene.entities.contains_key(&entity.id) {
                return Err(SceneError::DuplicateEntityId { id: entity.id });
            }
            if let Some(label) = &entity.label {
                check_label(label)?;
            }
            scene.check_placement(entity.kind, entity.position, None)?;
            scene.entities.insert(entity.id, entity);
        }
        Ok(scene)
    }

    /// The serializable form of this scene. Entities are listed in ascending id order.
    pub fn to_document(&self) -> SceneDocument {
        SceneDocument {
            size: self.size,
            rules: self.rules,
            next_entity_id: self.ids.next_raw(),
            entities: self.entities.values().cloned().collect(),
        }
    }

    /// Grid dimensions.
    pub const fn size(&self) -> GridSize {
        self.size
    }

    /// Rule parameters.
    pub const fn rules(&self) -> Rules {
        self.rules
    }

    /// Identifier the next created entity will receive.
    pub const fn next_entity_id(&self) -> u32 {
        self.ids.next_raw()
    }

    /// The entity with `id`.
    pub fn entity(&self, id: EntityId) -> Option<&Entity> {
        self.entities.get(&id)
    }

    /// All entities in ascending id order.
    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.entities.values()
    }

    /// Number of entities.
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    /// The entity occupying `position`.
    pub fn entity_at(&self, position: Position) -> Option<&Entity> {
        self.entities.values().find(|e| e.position == position)
    }

    /// The player spawn entity, if present.
    pub fn player(&self) -> Option<&Entity> {
        self.entities
            .values()
            .find(|e| e.kind == EntityKind::Player)
    }

    /// Exit entities in ascending id order.
    pub fn exits(&self) -> impl Iterator<Item = &Entity> {
        self.entities
            .values()
            .filter(|e| e.kind == EntityKind::Exit)
    }

    /// Colors of all keys present in the scene.
    pub fn key_colors(&self) -> KeySet {
        self.entities
            .values()
            .filter_map(|e| match e.kind {
                EntityKind::Key { color } => Some(color),
                _ => None,
            })
            .collect()
    }

    /// Applies `edits` in order to a copy of this scene. Returns the edited copy and one
    /// effect per edit, or the first failure. `self` is never modified, so a failed batch
    /// leaves no partial changes anywhere.
    pub fn apply_edits(&self, edits: &[SceneEdit]) -> Result<(Scene, Vec<EditEffect>), BatchError> {
        let mut draft = self.clone();
        let mut effects = Vec::with_capacity(edits.len());
        for (index, edit) in edits.iter().enumerate() {
            let effect = draft
                .apply_edit(edit)
                .map_err(|error| BatchError { index, error })?;
            effects.push(effect);
        }
        Ok((draft, effects))
    }

    fn apply_edit(&mut self, edit: &SceneEdit) -> Result<EditEffect, SceneError> {
        match edit {
            SceneEdit::CreateEntity {
                kind,
                position,
                label,
            } => {
                let label = normalize_label(label.as_deref())?;
                self.check_placement(*kind, *position, None)?;
                let id = self.ids.allocate().ok_or(SceneError::IdSpaceExhausted)?;
                let entity = Entity {
                    id,
                    kind: *kind,
                    position: *position,
                    label,
                };
                self.entities.insert(id, entity.clone());
                Ok(EditEffect::Created { entity })
            }
            SceneEdit::UpdateEntity {
                id,
                kind,
                position,
                label,
            } => {
                if kind.is_none() && position.is_none() && label.is_none() {
                    return Err(SceneError::EmptyUpdate {
                        op: "update_entity",
                    });
                }
                let before = self
                    .entities
                    .get(id)
                    .cloned()
                    .ok_or(SceneError::EntityNotFound { id: *id })?;
                let mut after = before.clone();
                if let Some(kind) = kind {
                    after.kind = *kind;
                }
                if let Some(position) = position {
                    after.position = *position;
                }
                if let Some(label) = label {
                    after.label = normalize_label(Some(label))?;
                }
                self.check_placement(after.kind, after.position, Some(*id))?;
                self.entities.insert(*id, after.clone());
                Ok(EditEffect::Updated { before, after })
            }
            SceneEdit::RemoveEntity { id } => {
                let entity = self
                    .entities
                    .remove(id)
                    .ok_or(SceneError::EntityNotFound { id: *id })?;
                Ok(EditEffect::Removed { entity })
            }
            SceneEdit::UpdateRules {
                max_ticks,
                win_condition,
            } => {
                let patch = RulesPatch {
                    max_ticks: *max_ticks,
                    win_condition: *win_condition,
                };
                if patch.is_empty() {
                    return Err(SceneError::EmptyUpdate { op: "update_rules" });
                }
                let before = self.rules;
                let after = patch.apply_to(before);
                check_rules(after)?;
                self.rules = after;
                Ok(EditEffect::RulesUpdated { before, after })
            }
        }
    }

    /// Checks that an entity of `kind` may occupy `position`, ignoring entity `ignore`
    /// (the entity being updated, which may keep its own cell).
    fn check_placement(
        &self,
        kind: EntityKind,
        position: Position,
        ignore: Option<EntityId>,
    ) -> Result<(), SceneError> {
        if !self.size.contains(position) {
            return Err(SceneError::OutOfBounds {
                position,
                size: self.size,
            });
        }
        let others = || self.entities.values().filter(|e| Some(e.id) != ignore);
        if let Some(occupant) = others().find(|e| e.position == position) {
            return Err(SceneError::CellOccupied {
                position,
                occupant: occupant.id,
                occupant_kind: occupant.kind,
            });
        }
        match kind {
            EntityKind::Player => {
                if let Some(existing) = others().find(|e| e.kind == EntityKind::Player) {
                    return Err(SceneError::DuplicatePlayer {
                        existing: existing.id,
                    });
                }
            }
            EntityKind::Key { color } => {
                if let Some(existing) = others().find(|e| e.kind == kind) {
                    return Err(SceneError::DuplicateKeyColor {
                        color,
                        existing: existing.id,
                    });
                }
            }
            EntityKind::Wall | EntityKind::Door { .. } | EntityKind::Exit => {}
        }
        Ok(())
    }
}

fn check_size(size: GridSize) -> Result<(), SceneError> {
    if size.is_supported() {
        Ok(())
    } else {
        Err(SceneError::InvalidGridSize {
            size,
            max: GridSize::MAX_DIMENSION,
        })
    }
}

fn check_rules(rules: Rules) -> Result<(), SceneError> {
    rules
        .check()
        .map_err(|(field, reason)| SceneError::InvalidRules { field, reason })
}

fn check_label(label: &str) -> Result<(), SceneError> {
    let chars = label.chars().count();
    if chars == 0 || chars > MAX_LABEL_CHARS {
        return Err(SceneError::InvalidLabel {
            reason: format!("must be 1 to {MAX_LABEL_CHARS} characters, got {chars}"),
        });
    }
    if label.chars().any(char::is_control) {
        return Err(SceneError::InvalidLabel {
            reason: "must not contain control characters".to_owned(),
        });
    }
    Ok(())
}

/// Empty labels mean "no label"; anything else must pass [`check_label`].
fn normalize_label(label: Option<&str>) -> Result<Option<String>, SceneError> {
    match label {
        None | Some("") => Ok(None),
        Some(text) => {
            check_label(text)?;
            Ok(Some(text.to_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> Scene {
        Scene::new(GridSize::new(4, 3), Rules::default()).expect("valid scene")
    }

    fn create(kind: EntityKind, x: i32, y: i32) -> SceneEdit {
        SceneEdit::CreateEntity {
            kind,
            position: Position::new(x, y),
            label: None,
        }
    }

    #[test]
    fn ids_are_assigned_in_order_and_never_reused() {
        let (scene, _) = scene()
            .apply_edits(&[
                create(EntityKind::Wall, 0, 0),
                create(EntityKind::Wall, 1, 0),
            ])
            .expect("valid batch");
        let (scene, _) = scene
            .apply_edits(&[
                SceneEdit::RemoveEntity {
                    id: EntityId::new(2),
                },
                create(EntityKind::Exit, 1, 0),
            ])
            .expect("valid batch");
        let ids: Vec<u32> = scene.entities().map(|e| e.id.get()).collect();
        assert_eq!(ids, vec![1, 3]);
        assert_eq!(scene.next_entity_id(), 4);
    }

    #[test]
    fn failed_batch_reports_index_and_leaves_scene_untouched() {
        let original = scene();
        let error = original
            .apply_edits(&[
                create(EntityKind::Wall, 0, 0),
                create(EntityKind::Exit, 0, 0),
            ])
            .expect_err("second edit collides");
        assert_eq!(error.index, 1);
        assert!(matches!(
            error.error,
            SceneError::CellOccupied { occupant, .. } if occupant == EntityId::new(1)
        ));
        assert_eq!(original, scene(), "receiver must not change");
        assert_eq!(original.next_entity_id(), 1, "allocator must not advance");
    }

    #[test]
    fn invariants_reject_out_of_bounds_duplicates_and_bad_labels() {
        let base = scene();
        let oob = base.apply_edits(&[create(EntityKind::Wall, 4, 0)]);
        assert!(matches!(
            oob.map_err(|e| e.error),
            Err(SceneError::OutOfBounds { .. })
        ));

        let two_players = base.apply_edits(&[
            create(EntityKind::Player, 0, 0),
            create(EntityKind::Player, 1, 0),
        ]);
        assert!(matches!(
            two_players.map_err(|e| e.error),
            Err(SceneError::DuplicatePlayer { .. })
        ));

        let red = EntityKind::Key {
            color: KeyColor::Red,
        };
        let two_red_keys = base.apply_edits(&[create(red, 0, 0), create(red, 1, 0)]);
        assert!(matches!(
            two_red_keys.map_err(|e| e.error),
            Err(SceneError::DuplicateKeyColor { .. })
        ));

        let bad_label = base.apply_edits(&[SceneEdit::CreateEntity {
            kind: EntityKind::Wall,
            position: Position::new(0, 0),
            label: Some("x".repeat(MAX_LABEL_CHARS + 1)),
        }]);
        assert!(matches!(
            bad_label.map_err(|e| e.error),
            Err(SceneError::InvalidLabel { .. })
        ));
    }

    #[test]
    fn an_entity_may_be_updated_in_place() {
        let (scene, _) = scene()
            .apply_edits(&[create(EntityKind::Wall, 0, 0)])
            .expect("valid");
        let (scene, effects) = scene
            .apply_edits(&[SceneEdit::UpdateEntity {
                id: EntityId::new(1),
                kind: Some(EntityKind::Door {
                    color: KeyColor::Blue,
                }),
                position: None,
                label: Some("gate".to_owned()),
            }])
            .expect("updating own cell is allowed");
        let door = scene.entity(EntityId::new(1)).expect("still present");
        assert_eq!(door.label.as_deref(), Some("gate"));
        assert!(matches!(effects[0], EditEffect::Updated { .. }));
    }

    #[test]
    fn empty_updates_are_rejected() {
        let error = scene()
            .apply_edits(&[SceneEdit::UpdateRules {
                max_ticks: None,
                win_condition: None,
            }])
            .expect_err("nothing to change");
        assert!(matches!(error.error, SceneError::EmptyUpdate { .. }));
    }

    #[test]
    fn document_round_trip_preserves_allocator_and_rejects_corruption() {
        let (scene, _) = scene()
            .apply_edits(&[
                create(EntityKind::Wall, 0, 0),
                create(EntityKind::Exit, 3, 2),
            ])
            .expect("valid");
        let document = scene.to_document();
        assert_eq!(
            Scene::from_document(document.clone()).expect("round trip"),
            scene
        );

        let mut beyond = document.clone();
        beyond.next_entity_id = 2;
        assert!(matches!(
            Scene::from_document(beyond),
            Err(SceneError::IdOutsideAllocatedRange { .. })
        ));

        let mut duplicate = document;
        let first = duplicate.entities[0].clone();
        duplicate.entities.push(first);
        assert!(Scene::from_document(duplicate).is_err());
    }
}
