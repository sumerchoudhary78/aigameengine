//! The Rapier world behind a physics episode.
//!
//! Construction order is fixed (bodies in ascending id, each with exactly one collider), and
//! Rapier is built with `enhanced-determinism`. Identical hashes are tested for identical
//! inputs on the pinned dependency set and tested platform; other platforms are unverified. Contacts and zone overlaps are read from the narrow phase
//! after each step into ordered sets instead of relying on event-callback ordering.

use std::collections::{BTreeMap, BTreeSet};

use agentforge_core::ids::EntityId;
use rapier2d_f64::math::Vector;
use rapier2d_f64::prelude::{
    ColliderBuilder, ColliderHandle, PhysicsWorld, RigidBodyBuilder, RigidBodyHandle,
};

use crate::scene::{BodyKind, PhysicsScene, Shape, Vec2};

/// Raw dynamic state of one body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RawState {
    pub position: Vec2,
    /// Unit complex number `(cos θ, sin θ)` of the rotation.
    pub rotation: (f64, f64),
    pub linear_velocity: Vec2,
    pub angular_velocity: f64,
    pub sleeping: bool,
}

pub(crate) struct World {
    inner: PhysicsWorld,
    handles: BTreeMap<EntityId, RigidBodyHandle>,
    colliders: BTreeMap<(u32, u32), EntityId>,
}

fn key(handle: ColliderHandle) -> (u32, u32) {
    handle.into_raw_parts()
}

impl World {
    pub(crate) fn build(scene: &PhysicsScene) -> Self {
        let settings = scene.settings();
        let mut inner = PhysicsWorld::new();
        inner.gravity = Vector::new(settings.gravity.x, settings.gravity.y);
        inner.integration_parameters.dt = 1.0 / f64::from(settings.ticks_per_second);
        let mut handles = BTreeMap::new();
        let mut colliders = BTreeMap::new();
        for body in scene.bodies() {
            let builder = match body.kind {
                BodyKind::Dynamic => RigidBodyBuilder::dynamic().ccd_enabled(true),
                BodyKind::Fixed => RigidBodyBuilder::fixed(),
            };
            let rigid_body = builder
                .translation(Vector::new(body.position.x, body.position.y))
                .rotation(body.rotation)
                .linvel(Vector::new(body.linear_velocity.x, body.linear_velocity.y))
                .angvel(body.angular_velocity)
                .linear_damping(body.linear_damping)
                .angular_damping(body.angular_damping)
                .build();
            let handle = inner.bodies.insert(rigid_body);
            let collider = match body.shape {
                Shape::Ball { radius } => ColliderBuilder::ball(radius),
                Shape::Box {
                    half_width,
                    half_height,
                } => ColliderBuilder::cuboid(half_width, half_height),
            }
            .density(body.material.density)
            .friction(body.material.friction)
            .restitution(body.material.restitution)
            .sensor(body.sensor)
            .build();
            let collider_handle =
                inner
                    .colliders
                    .insert_with_parent(collider, handle, &mut inner.bodies);
            handles.insert(body.id, handle);
            colliders.insert(key(collider_handle), body.id);
        }
        Self {
            inner,
            handles,
            colliders,
        }
    }

    /// Advances the simulation by one fixed timestep.
    pub(crate) fn step(&mut self) {
        self.inner.step();
        // Rapier solves contacts before integrating the final poses. Refresh queries so
        // gameplay sees the completed tick, including a goal entered on the last tick.
        self.inner.detect_collisions(&(), &());
    }

    /// Applies `impulse` (N·s) at the center of mass of `body` and wakes it.
    pub(crate) fn apply_impulse(&mut self, body: EntityId, impulse: Vec2) {
        if let Some(handle) = self.handles.get(&body)
            && let Some(rigid_body) = self.inner.bodies.get_mut(*handle)
        {
            rigid_body.apply_impulse(Vector::new(impulse.x, impulse.y), true);
        }
    }

    /// Current state of every body, in ascending id order.
    pub(crate) fn states(&self) -> Vec<(EntityId, RawState)> {
        self.handles
            .iter()
            .filter_map(|(id, handle)| self.inner.bodies.get(*handle).map(|rb| (*id, rb)))
            .map(|(id, rigid_body)| {
                let translation = rigid_body.translation();
                let rotation = rigid_body.rotation();
                let linvel = rigid_body.linvel();
                (
                    id,
                    RawState {
                        position: Vec2::new(translation.x, translation.y),
                        rotation: (rotation.re, rotation.im),
                        linear_velocity: Vec2::new(linvel.x, linvel.y),
                        angular_velocity: rigid_body.angvel(),
                        sleeping: rigid_body.is_sleeping(),
                    },
                )
            })
            .collect()
    }

    /// Pairs of solid bodies currently touching, as `(smaller id, larger id)`.
    pub(crate) fn contacts(&self) -> BTreeSet<(EntityId, EntityId)> {
        self.inner
            .narrow_phase
            .contact_pairs()
            .filter(|pair| pair.has_any_active_contact())
            .filter_map(|pair| {
                let a = *self.colliders.get(&key(pair.collider1))?;
                let b = *self.colliders.get(&key(pair.collider2))?;
                Some((a.min(b), a.max(b)))
            })
            .collect()
    }

    /// `(sensor, body)` pairs currently overlapping.
    pub(crate) fn overlaps(&self, sensors: &BTreeSet<EntityId>) -> BTreeSet<(EntityId, EntityId)> {
        let mut result = BTreeSet::new();
        for (first, second, intersecting) in self.inner.narrow_phase.intersection_pairs() {
            if !intersecting {
                continue;
            }
            let (Some(&a), Some(&b)) = (
                self.colliders.get(&key(first)),
                self.colliders.get(&key(second)),
            ) else {
                continue;
            };
            if sensors.contains(&a) {
                result.insert((a, b));
            }
            if sensors.contains(&b) {
                result.insert((b, a));
            }
        }
        result
    }
}
