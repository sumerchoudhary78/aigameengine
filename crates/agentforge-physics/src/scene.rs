//! Physics scenes: rigid bodies, materials and world settings, guarded by invariants and
//! edited in atomic batches (the same contract as grid scenes).
//!
//! Units are SI: meters, seconds, kilograms (density is kg/m², since the world is 2D),
//! newton-seconds for impulses, radians for angles. **`y` points up**, as is conventional
//! in physics (grid scenes use `y` down).

use std::collections::BTreeMap;

use agentforge_core::ids::{EntityId, IdAllocator};
use agentforge_core::scene::MAX_LABEL_CHARS;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Most bodies in one physics scene.
pub const MAX_BODIES: usize = 256;
/// Largest absolute coordinate, rotation (radians) or bounds value.
pub const MAX_COORDINATE: f64 = 10_000.0;
/// Largest absolute linear (m/s) or angular (rad/s) velocity component.
pub const MAX_SPEED: f64 = 1_000.0;
/// Smallest radius or half extent, in meters.
pub const MIN_EXTENT: f64 = 0.01;
/// Largest radius or half extent, in meters.
pub const MAX_EXTENT: f64 = 100.0;
/// Largest `max_ticks` of a physics scene.
pub const MAX_PHYSICS_TICKS: u32 = 36_000;
/// Largest impulse limit, in newton-seconds.
pub const MAX_IMPULSE_LIMIT: f64 = 10_000.0;

/// A 2D vector: meters, m/s, N·s or m/s² depending on the field. `y` points up.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Vec2 {
    /// East component.
    pub x: f64,
    /// North (up) component.
    pub y: f64,
}

impl Vec2 {
    /// The zero vector.
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };

    /// Creates a vector.
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Euclidean length. Uses only `sqrt`, which IEEE 754 requires to be correctly rounded,
    /// so the result is identical on every platform.
    pub fn length(self) -> f64 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    /// Distance to `other`.
    pub fn distance(self, other: Vec2) -> f64 {
        Vec2::new(self.x - other.x, self.y - other.y).length()
    }

    /// Whether both components are finite.
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// How a body moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BodyKind {
    /// Moved by gravity, contacts and impulses.
    Dynamic,
    /// Never moves (ground, walls, goal zones).
    Fixed,
}

/// Collision shape, centered on the body position.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Shape {
    /// A disc.
    Ball {
        /// Radius in meters (0.01-100).
        radius: f64,
    },
    /// A rectangle, rotated with the body.
    Box {
        /// Half the width in meters (0.01-100).
        half_width: f64,
        /// Half the height in meters (0.01-100).
        half_height: f64,
    },
}

/// Surface and mass properties.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Material {
    /// Mass per area in kg/m² (0.001-10000). Irrelevant for fixed bodies.
    pub density: f64,
    /// Coulomb friction coefficient (0-100).
    pub friction: f64,
    /// Bounciness (0 = no bounce, 1 = perfectly elastic).
    pub restitution: f64,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            density: 1.0,
            friction: 0.5,
            restitution: 0.0,
        }
    }
}

/// An axis-aligned rectangle of the world.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    /// Lower-left corner.
    pub min: Vec2,
    /// Upper-right corner.
    pub max: Vec2,
}

impl Bounds {
    /// Whether `point` lies inside (borders included).
    pub fn contains(self, point: Vec2) -> bool {
        point.x >= self.min.x
            && point.x <= self.max.x
            && point.y >= self.min.y
            && point.y <= self.max.y
    }
}

/// Win condition: the target body must overlap the zone (a sensor body).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Objective {
    /// A dynamic, non-sensor body.
    pub body: EntityId,
    /// A sensor body.
    pub zone: EntityId,
}

/// What the player may do: apply impulses to one body.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Control {
    /// The dynamic, non-sensor body the player pushes.
    pub body: EntityId,
    /// Largest impulse magnitude per action, in N·s (0.001-10000).
    pub max_impulse: f64,
    /// Most impulses per episode; 0 means unlimited.
    #[serde(default)]
    pub max_impulses: u32,
}

/// World settings of a physics scene.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PhysicsSettings {
    /// Gravity in m/s² (default `{"x": 0, "y": -9.81}`).
    pub gravity: Vec2,
    /// Simulation rate; one tick lasts `1 / ticks_per_second` seconds (30-240).
    pub ticks_per_second: u32,
    /// The episode fails once this many ticks pass without a win (1-36000).
    pub max_ticks: u32,
    /// The playing field. Bodies may leave it (a `left_bounds` event is recorded).
    pub bounds: Bounds,
    /// Win condition (required to play).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<Objective>,
    /// Player control (required to play).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Control>,
}

impl Default for PhysicsSettings {
    fn default() -> Self {
        Self {
            gravity: Vec2::new(0.0, -9.81),
            ticks_per_second: 60,
            max_ticks: 600,
            bounds: Bounds {
                min: Vec2::new(-20.0, -20.0),
                max: Vec2::new(20.0, 20.0),
            },
            objective: None,
            control: None,
        }
    }
}

/// Settings fields to change; absent fields keep their values.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    /// New gravity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gravity: Option<Vec2>,
    /// New tick rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticks_per_second: Option<u32>,
    /// New tick limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ticks: Option<u32>,
    /// New bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    /// New objective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<Objective>,
    /// New control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Control>,
}

/// A rigid body with its single collider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Body {
    /// Stable id, never reused within the scene.
    pub id: EntityId,
    /// Dynamic or fixed.
    pub kind: BodyKind,
    /// Collision shape.
    pub shape: Shape,
    /// Center position in meters.
    pub position: Vec2,
    /// Counter-clockwise rotation in radians.
    #[serde(default)]
    pub rotation: f64,
    /// Initial velocity in m/s (must be zero for fixed bodies).
    #[serde(default)]
    pub linear_velocity: Vec2,
    /// Initial angular velocity in rad/s (must be zero for fixed bodies).
    #[serde(default)]
    pub angular_velocity: f64,
    /// Mass and surface properties.
    #[serde(default)]
    pub material: Material,
    /// Linear velocity damping (0-100).
    #[serde(default)]
    pub linear_damping: f64,
    /// Angular velocity damping (0-100).
    #[serde(default)]
    pub angular_damping: f64,
    /// Sensors detect overlaps but never collide (use them as goal zones).
    #[serde(default)]
    pub sensor: bool,
    /// Authoring note; never shown to players or hashed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// A body to create; the engine assigns its id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BodyDraft {
    /// Dynamic or fixed.
    pub kind: BodyKind,
    /// Collision shape.
    pub shape: Shape,
    /// Center position in meters.
    pub position: Vec2,
    /// Counter-clockwise rotation in radians.
    #[serde(default)]
    pub rotation: f64,
    /// Initial velocity in m/s.
    #[serde(default)]
    pub linear_velocity: Vec2,
    /// Initial angular velocity in rad/s.
    #[serde(default)]
    pub angular_velocity: f64,
    /// Mass and surface properties (default density 1, friction 0.5, restitution 0).
    #[serde(default)]
    pub material: Material,
    /// Linear damping.
    #[serde(default)]
    pub linear_damping: f64,
    /// Angular damping.
    #[serde(default)]
    pub angular_damping: f64,
    /// Whether the body is a sensor.
    #[serde(default)]
    pub sensor: bool,
    /// Authoring note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Body fields to change; absent fields keep their values.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BodyPatch {
    /// New kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<BodyKind>,
    /// New shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<Shape>,
    /// New position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Vec2>,
    /// New rotation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<f64>,
    /// New initial velocity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear_velocity: Option<Vec2>,
    /// New initial angular velocity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angular_velocity: Option<f64>,
    /// New material.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<Material>,
    /// New linear damping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linear_damping: Option<f64>,
    /// New angular damping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angular_damping: Option<f64>,
    /// New sensor flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor: Option<bool>,
    /// New label; an empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl BodyPatch {
    fn is_empty(&self) -> bool {
        *self == BodyPatch::default()
    }
}

/// One physics authoring edit. Batches apply atomically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum PhysicsEdit {
    /// Create a body; ids are assigned sequentially from `next_body_id`.
    CreateBody {
        /// The body.
        body: BodyDraft,
    },
    /// Change some fields of a body.
    UpdateBody {
        /// Which body.
        id: EntityId,
        /// Fields to change (at least one).
        changes: BodyPatch,
    },
    /// Remove a body (rejected while the settings reference it). Its id is never reused.
    RemoveBody {
        /// Which body.
        id: EntityId,
    },
    /// Change world settings (at least one field).
    UpdateSettings {
        /// Fields to change.
        settings: SettingsPatch,
    },
}

/// What an accepted edit did.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum PhysicsEditEffect {
    /// A body was created.
    Created {
        /// The new body.
        body: Body,
    },
    /// A body changed.
    Updated {
        /// Before.
        before: Body,
        /// After.
        after: Body,
    },
    /// A body was removed.
    Removed {
        /// The removed body.
        body: Body,
    },
    /// Settings changed.
    SettingsUpdated {
        /// Before.
        before: PhysicsSettings,
        /// After.
        after: PhysicsSettings,
    },
}

/// Why a physics scene construction or edit was rejected.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PhysicsSceneError {
    /// A number is NaN or infinite.
    #[error("{field} must be a finite number")]
    NotFinite {
        /// Offending field.
        field: String,
    },
    /// A number is outside its range.
    #[error("{field} must be between {min} and {max}, got {value}")]
    OutOfRange {
        /// Offending field.
        field: String,
        /// Rejected value.
        value: f64,
        /// Smallest accepted value.
        min: f64,
        /// Largest accepted value.
        max: f64,
    },
    /// No body has this id.
    #[error("body {id} does not exist in this scene")]
    BodyNotFound {
        /// The missing id.
        id: EntityId,
    },
    /// Body limit reached.
    #[error("a physics scene holds at most {max} bodies")]
    TooManyBodies {
        /// The limit.
        max: usize,
    },
    /// A settings reference points at a body of the wrong kind.
    #[error("the {role} body {id} must be {requirement}")]
    InvalidReference {
        /// `objective.body`, `objective.zone` or `control.body`.
        role: &'static str,
        /// The referenced body.
        id: EntityId,
        /// What the body must be.
        requirement: &'static str,
    },
    /// The body is referenced by the settings and cannot be removed.
    #[error("body {id} is the {role}; change the scene settings before removing it")]
    BodyInUse {
        /// The body.
        id: EntityId,
        /// How the settings use it.
        role: &'static str,
    },
    /// A fixed body was given a velocity.
    #[error("fixed bodies cannot have an initial velocity")]
    FixedBodyMoving,
    /// Invalid label.
    #[error("invalid label: {reason}")]
    InvalidLabel {
        /// What is wrong.
        reason: String,
    },
    /// An update that changes nothing.
    #[error("{op} must change at least one field")]
    EmptyUpdate {
        /// The edit operation.
        op: &'static str,
    },
    /// `bounds.min` is not below `bounds.max`.
    #[error("bounds.min must be below bounds.max on both axes")]
    InvalidBounds,
    /// The same id appears twice in a document.
    #[error("body id {id} appears more than once")]
    DuplicateBodyId {
        /// The id.
        id: EntityId,
    },
    /// A document id outside `1..next_body_id`.
    #[error("body id {id} is outside the allocated range 1..{next_body_id}")]
    IdOutsideAllocatedRange {
        /// The id.
        id: EntityId,
        /// The document's allocator state.
        next_body_id: u32,
    },
    /// The allocator would hand out id 0.
    #[error("next_body_id must be at least 1, got {next_body_id}")]
    InvalidNextBodyId {
        /// The rejected value.
        next_body_id: u32,
    },
    /// The allocator cannot produce another id.
    #[error("body id space is exhausted")]
    IdSpaceExhausted,
}

/// A rejected batch: the first failing edit; the scene is unchanged.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("edit {index} was rejected: {error}")]
pub struct PhysicsBatchError {
    /// Zero-based index of the failing edit.
    pub index: usize,
    /// Why it failed.
    pub error: PhysicsSceneError,
}

/// Authoring data of one physics world.
///
/// Invariants enforced on construction and after every edit: every number is finite and in
/// range, fixed bodies have no velocity, labels are valid, at most [`MAX_BODIES`] bodies,
/// and the objective and control (when set) reference existing bodies of the right kind.
/// Completeness (objective and control both set) is required only to play.
#[derive(Debug, Clone, PartialEq)]
pub struct PhysicsScene {
    settings: PhysicsSettings,
    bodies: BTreeMap<EntityId, Body>,
    ids: IdAllocator,
}

/// Serialized form of a [`PhysicsScene`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PhysicsSceneDocument {
    /// World settings.
    pub settings: PhysicsSettings,
    /// Id the next created body receives.
    pub next_body_id: u32,
    /// Bodies in ascending id order.
    pub bodies: Vec<Body>,
}

impl PhysicsScene {
    /// Creates an empty scene. `settings` must not reference bodies yet.
    pub fn new(settings: PhysicsSettings) -> Result<Self, PhysicsSceneError> {
        let scene = Self {
            settings,
            bodies: BTreeMap::new(),
            ids: IdAllocator::new(),
        };
        check_settings(&settings)?;
        scene.check_references()?;
        Ok(scene)
    }

    /// Rebuilds a scene from its document, checking every invariant.
    pub fn from_document(document: PhysicsSceneDocument) -> Result<Self, PhysicsSceneError> {
        if document.next_body_id < IdAllocator::FIRST {
            return Err(PhysicsSceneError::InvalidNextBodyId {
                next_body_id: document.next_body_id,
            });
        }
        check_settings(&document.settings)?;
        if document.bodies.len() > MAX_BODIES {
            return Err(PhysicsSceneError::TooManyBodies { max: MAX_BODIES });
        }
        let mut bodies = BTreeMap::new();
        for body in document.bodies {
            let raw = body.id.get();
            if raw < IdAllocator::FIRST || raw >= document.next_body_id {
                return Err(PhysicsSceneError::IdOutsideAllocatedRange {
                    id: body.id,
                    next_body_id: document.next_body_id,
                });
            }
            check_body(&body)?;
            if bodies.insert(body.id, body.clone()).is_some() {
                return Err(PhysicsSceneError::DuplicateBodyId { id: body.id });
            }
        }
        let scene = Self {
            settings: document.settings,
            bodies,
            ids: IdAllocator::starting_at(document.next_body_id),
        };
        scene.check_references()?;
        Ok(scene)
    }

    /// The serializable form (bodies in ascending id order).
    pub fn to_document(&self) -> PhysicsSceneDocument {
        PhysicsSceneDocument {
            settings: self.settings,
            next_body_id: self.ids.next_raw(),
            bodies: self.bodies.values().cloned().collect(),
        }
    }

    /// World settings.
    pub fn settings(&self) -> &PhysicsSettings {
        &self.settings
    }

    /// The body with `id`.
    pub fn body(&self, id: EntityId) -> Option<&Body> {
        self.bodies.get(&id)
    }

    /// Bodies in ascending id order.
    pub fn bodies(&self) -> impl Iterator<Item = &Body> {
        self.bodies.values()
    }

    /// Number of bodies.
    pub fn body_count(&self) -> usize {
        self.bodies.len()
    }

    /// Id the next created body receives.
    pub fn next_body_id(&self) -> u32 {
        self.ids.next_raw()
    }

    /// Applies `edits` to a copy of the scene: all succeed, or the first failure is
    /// returned and `self` is untouched.
    pub fn apply_edits(
        &self,
        edits: &[PhysicsEdit],
    ) -> Result<(PhysicsScene, Vec<PhysicsEditEffect>), PhysicsBatchError> {
        let mut draft = self.clone();
        let mut effects = Vec::with_capacity(edits.len());
        for (index, edit) in edits.iter().enumerate() {
            let effect = draft
                .apply_edit(edit)
                .map_err(|error| PhysicsBatchError { index, error })?;
            effects.push(effect);
        }
        Ok((draft, effects))
    }

    fn apply_edit(&mut self, edit: &PhysicsEdit) -> Result<PhysicsEditEffect, PhysicsSceneError> {
        match edit {
            PhysicsEdit::CreateBody { body } => {
                if self.bodies.len() >= MAX_BODIES {
                    return Err(PhysicsSceneError::TooManyBodies { max: MAX_BODIES });
                }
                let mut created = Body {
                    id: EntityId::new(self.ids.next_raw()),
                    kind: body.kind,
                    shape: body.shape,
                    position: body.position,
                    rotation: body.rotation,
                    linear_velocity: body.linear_velocity,
                    angular_velocity: body.angular_velocity,
                    material: body.material,
                    linear_damping: body.linear_damping,
                    angular_damping: body.angular_damping,
                    sensor: body.sensor,
                    label: normalize_label(body.label.as_deref()),
                };
                check_body(&created)?;
                created.id = self
                    .ids
                    .allocate()
                    .ok_or(PhysicsSceneError::IdSpaceExhausted)?;
                self.bodies.insert(created.id, created.clone());
                Ok(PhysicsEditEffect::Created { body: created })
            }
            PhysicsEdit::UpdateBody { id, changes } => {
                if changes.is_empty() {
                    return Err(PhysicsSceneError::EmptyUpdate { op: "update_body" });
                }
                let before = self
                    .bodies
                    .get(id)
                    .cloned()
                    .ok_or(PhysicsSceneError::BodyNotFound { id: *id })?;
                let mut after = before.clone();
                let patch = changes.clone();
                after.kind = patch.kind.unwrap_or(after.kind);
                after.shape = patch.shape.unwrap_or(after.shape);
                after.position = patch.position.unwrap_or(after.position);
                after.rotation = patch.rotation.unwrap_or(after.rotation);
                after.linear_velocity = patch.linear_velocity.unwrap_or(after.linear_velocity);
                after.angular_velocity = patch.angular_velocity.unwrap_or(after.angular_velocity);
                after.material = patch.material.unwrap_or(after.material);
                after.linear_damping = patch.linear_damping.unwrap_or(after.linear_damping);
                after.angular_damping = patch.angular_damping.unwrap_or(after.angular_damping);
                after.sensor = patch.sensor.unwrap_or(after.sensor);
                if let Some(label) = patch.label.as_deref() {
                    after.label = normalize_label(Some(label));
                }
                check_body(&after)?;
                self.bodies.insert(*id, after.clone());
                self.check_references()?;
                Ok(PhysicsEditEffect::Updated { before, after })
            }
            PhysicsEdit::RemoveBody { id } => {
                if let Some(role) = self.reference_role(*id) {
                    return Err(PhysicsSceneError::BodyInUse { id: *id, role });
                }
                let body = self
                    .bodies
                    .remove(id)
                    .ok_or(PhysicsSceneError::BodyNotFound { id: *id })?;
                Ok(PhysicsEditEffect::Removed { body })
            }
            PhysicsEdit::UpdateSettings { settings } => {
                if *settings == SettingsPatch::default() {
                    return Err(PhysicsSceneError::EmptyUpdate {
                        op: "update_settings",
                    });
                }
                let before = self.settings;
                let after = PhysicsSettings {
                    gravity: settings.gravity.unwrap_or(before.gravity),
                    ticks_per_second: settings.ticks_per_second.unwrap_or(before.ticks_per_second),
                    max_ticks: settings.max_ticks.unwrap_or(before.max_ticks),
                    bounds: settings.bounds.unwrap_or(before.bounds),
                    objective: settings.objective.or(before.objective),
                    control: settings.control.or(before.control),
                };
                check_settings(&after)?;
                self.settings = after;
                self.check_references()?;
                Ok(PhysicsEditEffect::SettingsUpdated { before, after })
            }
        }
    }

    /// How the settings use body `id`, if at all.
    fn reference_role(&self, id: EntityId) -> Option<&'static str> {
        let objective = self.settings.objective;
        if objective.is_some_and(|o| o.body == id) {
            Some("objective target")
        } else if objective.is_some_and(|o| o.zone == id) {
            Some("objective zone")
        } else if self.settings.control.is_some_and(|c| c.body == id) {
            Some("controlled body")
        } else {
            None
        }
    }

    fn check_references(&self) -> Result<(), PhysicsSceneError> {
        let solid_dynamic = |role: &'static str, id: EntityId| match self.bodies.get(&id) {
            None => Err(PhysicsSceneError::BodyNotFound { id }),
            Some(body) if body.kind != BodyKind::Dynamic || body.sensor => {
                Err(PhysicsSceneError::InvalidReference {
                    role,
                    id,
                    requirement: "a dynamic, non-sensor body",
                })
            }
            Some(_) => Ok(()),
        };
        if let Some(objective) = self.settings.objective {
            solid_dynamic("objective target", objective.body)?;
            match self.bodies.get(&objective.zone) {
                None => return Err(PhysicsSceneError::BodyNotFound { id: objective.zone }),
                Some(zone) if !zone.sensor => {
                    return Err(PhysicsSceneError::InvalidReference {
                        role: "objective zone",
                        id: objective.zone,
                        requirement: "a sensor",
                    });
                }
                Some(_) => {}
            }
        }
        if let Some(control) = self.settings.control {
            solid_dynamic("controlled", control.body)?;
        }
        Ok(())
    }
}

fn finite(field: &str, value: f64) -> Result<(), PhysicsSceneError> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(PhysicsSceneError::NotFinite {
            field: field.to_owned(),
        })
    }
}

fn in_range(field: &str, value: f64, min: f64, max: f64) -> Result<(), PhysicsSceneError> {
    finite(field, value)?;
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(PhysicsSceneError::OutOfRange {
            field: field.to_owned(),
            value,
            min,
            max,
        })
    }
}

fn vector_in_range(field: &str, value: Vec2, limit: f64) -> Result<(), PhysicsSceneError> {
    in_range(&format!("{field}.x"), value.x, -limit, limit)?;
    in_range(&format!("{field}.y"), value.y, -limit, limit)
}

fn check_settings(settings: &PhysicsSettings) -> Result<(), PhysicsSceneError> {
    vector_in_range("gravity", settings.gravity, 1_000.0)?;
    in_range(
        "ticks_per_second",
        f64::from(settings.ticks_per_second),
        30.0,
        240.0,
    )?;
    in_range(
        "max_ticks",
        f64::from(settings.max_ticks),
        1.0,
        f64::from(MAX_PHYSICS_TICKS),
    )?;
    vector_in_range("bounds.min", settings.bounds.min, MAX_COORDINATE)?;
    vector_in_range("bounds.max", settings.bounds.max, MAX_COORDINATE)?;
    if settings.bounds.min.x >= settings.bounds.max.x
        || settings.bounds.min.y >= settings.bounds.max.y
    {
        return Err(PhysicsSceneError::InvalidBounds);
    }
    if let Some(control) = settings.control {
        in_range(
            "control.max_impulse",
            control.max_impulse,
            0.001,
            MAX_IMPULSE_LIMIT,
        )?;
    }
    Ok(())
}

fn check_body(body: &Body) -> Result<(), PhysicsSceneError> {
    match body.shape {
        Shape::Ball { radius } => in_range("shape.radius", radius, MIN_EXTENT, MAX_EXTENT)?,
        Shape::Box {
            half_width,
            half_height,
        } => {
            in_range("shape.half_width", half_width, MIN_EXTENT, MAX_EXTENT)?;
            in_range("shape.half_height", half_height, MIN_EXTENT, MAX_EXTENT)?;
        }
    }
    vector_in_range("position", body.position, MAX_COORDINATE)?;
    in_range("rotation", body.rotation, -MAX_COORDINATE, MAX_COORDINATE)?;
    vector_in_range("linear_velocity", body.linear_velocity, MAX_SPEED)?;
    in_range(
        "angular_velocity",
        body.angular_velocity,
        -MAX_SPEED,
        MAX_SPEED,
    )?;
    in_range("material.density", body.material.density, 0.001, 10_000.0)?;
    in_range("material.friction", body.material.friction, 0.0, 100.0)?;
    in_range("material.restitution", body.material.restitution, 0.0, 1.0)?;
    in_range("linear_damping", body.linear_damping, 0.0, 100.0)?;
    in_range("angular_damping", body.angular_damping, 0.0, 100.0)?;
    let moving = body.linear_velocity != Vec2::ZERO || body.angular_velocity != 0.0;
    if body.kind == BodyKind::Fixed && moving {
        return Err(PhysicsSceneError::FixedBodyMoving);
    }
    if let Some(label) = &body.label {
        let chars = label.chars().count();
        if chars == 0 || chars > MAX_LABEL_CHARS || label.chars().any(char::is_control) {
            return Err(PhysicsSceneError::InvalidLabel {
                reason: format!(
                    "must be 1 to {MAX_LABEL_CHARS} characters without control characters"
                ),
            });
        }
    }
    Ok(())
}

fn normalize_label(label: Option<&str>) -> Option<String> {
    label.filter(|text| !text.is_empty()).map(str::to_owned)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn ball(position: Vec2, radius: f64) -> BodyDraft {
        BodyDraft {
            kind: BodyKind::Dynamic,
            shape: Shape::Ball { radius },
            position,
            rotation: 0.0,
            linear_velocity: Vec2::ZERO,
            angular_velocity: 0.0,
            material: Material::default(),
            linear_damping: 0.0,
            angular_damping: 0.0,
            sensor: false,
            label: None,
        }
    }

    pub(crate) fn fixed_box(
        position: Vec2,
        half_width: f64,
        half_height: f64,
        sensor: bool,
    ) -> BodyDraft {
        BodyDraft {
            kind: BodyKind::Fixed,
            shape: Shape::Box {
                half_width,
                half_height,
            },
            sensor,
            ..ball(position, 1.0)
        }
    }

    fn create(body: BodyDraft) -> PhysicsEdit {
        PhysicsEdit::CreateBody { body }
    }

    #[test]
    fn references_are_checked_and_referenced_bodies_cannot_be_removed() {
        let scene = PhysicsScene::new(PhysicsSettings::default()).expect("valid");
        let (scene, _) = scene
            .apply_edits(&[
                create(ball(Vec2::new(0.0, 1.0), 0.5)),
                create(fixed_box(Vec2::new(5.0, 1.0), 1.0, 1.0, true)),
                PhysicsEdit::UpdateSettings {
                    settings: SettingsPatch {
                        objective: Some(Objective {
                            body: EntityId::new(1),
                            zone: EntityId::new(2),
                        }),
                        control: Some(Control {
                            body: EntityId::new(1),
                            max_impulse: 5.0,
                            max_impulses: 1,
                        }),
                        ..SettingsPatch::default()
                    },
                },
            ])
            .expect("valid batch");
        let removal = scene.apply_edits(&[PhysicsEdit::RemoveBody {
            id: EntityId::new(2),
        }]);
        assert!(matches!(
            removal.map_err(|e| e.error),
            Err(PhysicsSceneError::BodyInUse {
                role: "objective zone",
                ..
            })
        ));
        let not_sensor = scene.apply_edits(&[PhysicsEdit::UpdateBody {
            id: EntityId::new(2),
            changes: BodyPatch {
                sensor: Some(false),
                ..BodyPatch::default()
            },
        }]);
        assert!(matches!(
            not_sensor.map_err(|e| e.error),
            Err(PhysicsSceneError::InvalidReference { .. })
        ));
    }

    #[test]
    fn numbers_are_validated_and_failed_batches_change_nothing() {
        let scene = PhysicsScene::new(PhysicsSettings::default()).expect("valid");
        let mut bad = ball(Vec2::new(0.0, 0.0), 0.5);
        bad.material.restitution = 1.5;
        let error = scene
            .apply_edits(&[create(ball(Vec2::ZERO, 0.5)), create(bad)])
            .expect_err("restitution out of range");
        assert_eq!(error.index, 1);
        assert!(matches!(error.error, PhysicsSceneError::OutOfRange { .. }));
        assert_eq!(scene.body_count(), 0);
        assert_eq!(scene.next_body_id(), 1);

        let mut moving_wall = fixed_box(Vec2::ZERO, 1.0, 1.0, false);
        moving_wall.linear_velocity = Vec2::new(1.0, 0.0);
        assert!(matches!(
            scene
                .apply_edits(&[create(moving_wall)])
                .map_err(|e| e.error),
            Err(PhysicsSceneError::FixedBodyMoving)
        ));
        let mut nan = ball(Vec2::new(f64::NAN, 0.0), 0.5);
        nan.label = None;
        assert!(matches!(
            scene.apply_edits(&[create(nan)]).map_err(|e| e.error),
            Err(PhysicsSceneError::NotFinite { .. })
        ));
    }

    #[test]
    fn documents_round_trip_and_reject_corruption() {
        let (scene, _) = PhysicsScene::new(PhysicsSettings::default())
            .expect("valid")
            .apply_edits(&[create(ball(Vec2::new(1.0, 2.0), 0.25))])
            .expect("valid");
        let document = scene.to_document();
        let json = serde_json::to_string(&document).expect("serializable");
        let parsed: PhysicsSceneDocument = serde_json::from_str(&json).expect("parsable");
        assert_eq!(PhysicsScene::from_document(parsed).expect("valid"), scene);

        let mut zero = document;
        zero.next_body_id = 0;
        assert!(matches!(
            PhysicsScene::from_document(zero),
            Err(PhysicsSceneError::InvalidNextBodyId { .. })
        ));
    }
}
