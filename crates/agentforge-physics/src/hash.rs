//! Canonical encoding: versioned domain, little-endian integers/IEEE-754 bits, ordered IDs.
//! Labels are excluded. Snapshot continuity uses replayed inputs, not this visible-state hash
//! as a serialized replacement for Rapier's hidden solver state.
use crate::{
    episode::{Action, Episode, Outcome},
    scene::{BodyKind, PhysicsScene, Shape, Vec2},
};
use agentforge_core::hash::{CanonicalEncoder as E, StateHash};
fn vec(e: &mut E, v: Vec2) {
    e.f64(v.x);
    e.f64(v.y);
}
pub fn scene(scene: &PhysicsScene) -> StateHash {
    let mut e = E::new("agentforge.physics.scene.v1");
    let s = scene.settings();
    vec(&mut e, s.gravity);
    e.u32(s.ticks_per_second);
    e.u32(s.max_ticks);
    vec(&mut e, s.bounds.min);
    vec(&mut e, s.bounds.max);
    e.u8(u8::from(s.objective.is_some()));
    if let Some(o) = s.objective {
        e.u32(o.body.get());
        e.u32(o.zone.get());
    }
    e.u8(u8::from(s.control.is_some()));
    if let Some(c) = s.control {
        e.u32(c.body.get());
        e.f64(c.max_impulse);
        e.u32(c.max_impulses);
    }
    e.u32(scene.next_body_id());
    e.u32(scene.body_count() as u32);
    for b in scene.bodies() {
        e.u32(b.id.get());
        e.u8(match b.kind {
            BodyKind::Dynamic => 1,
            BodyKind::Fixed => 2,
        });
        match b.shape {
            Shape::Ball { radius } => {
                e.u8(1);
                e.f64(radius);
            }
            Shape::Box {
                half_width,
                half_height,
            } => {
                e.u8(2);
                e.f64(half_width);
                e.f64(half_height);
            }
        }
        vec(&mut e, b.position);
        e.f64(b.rotation);
        vec(&mut e, b.linear_velocity);
        e.f64(b.angular_velocity);
        e.f64(b.material.density);
        e.f64(b.material.friction);
        e.f64(b.material.restitution);
        e.f64(b.linear_damping);
        e.f64(b.angular_damping);
        e.u8(u8::from(b.sensor));
    }
    e.finish()
}
pub fn state(episode: &Episode) -> StateHash {
    let mut e = E::new("agentforge.physics.state.v1");
    e.bytes(scene(episode.scene()).as_bytes());
    e.u32(episode.tick());
    e.u8(match episode.outcome() {
        Outcome::InProgress => 0,
        Outcome::Won => 1,
        Outcome::TickLimit => 2,
        Outcome::LeftBounds => 3,
        Outcome::Forfeited => 4,
        Outcome::NumericalFailure => 5,
    });
    e.u32(episode.impulses_used());
    for b in episode.states() {
        e.u32(b.id.get());
        vec(&mut e, b.position);
        for r in b.rotation {
            e.f64(r);
        }
        vec(&mut e, b.linear_velocity);
        e.f64(b.angular_velocity);
        e.u8(u8::from(b.sleeping));
    }
    for pairs in [episode.contacts(), episode.overlaps()] {
        e.u32(pairs.len() as u32);
        for (a, b) in pairs {
            e.u32(a.get());
            e.u32(b.get());
        }
    }
    e.finish()
}
pub fn action(e: &mut E, a: Action) {
    match a {
        Action::Wait => e.u8(0),
        Action::Impulse { impulse } => {
            e.u8(1);
            vec(e, impulse);
        }
        Action::Forfeit => e.u8(2),
    }
}
