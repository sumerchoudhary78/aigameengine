use agentforge_core::ids::EntityId;
use agentforge_physics::{
    episode::{Action, Episode, Outcome},
    replay::Replay,
    scene::{PhysicsScene, PhysicsSceneDocument, Vec2},
};
fn scene(name: &str) -> PhysicsScene {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name);
    let document: PhysicsSceneDocument =
        serde_json::from_str(&std::fs::read_to_string(path).expect("fixture")).expect("typed");
    PhysicsScene::from_document(document).expect("valid")
}
fn run(mut e: Episode, actions: &[Action]) -> Episode {
    for action in actions {
        e.apply(*action).expect("legal");
    }
    e
}
#[test]
fn same_scene_and_actions_produce_identical_hashes_and_roundtrip_replays() {
    let scene = scene("physics_launch.json");
    let actions = std::iter::once(Action::Impulse {
        impulse: Vec2::new(1.0, 0.0),
    })
    .chain(std::iter::repeat_n(Action::Wait, 25))
    .collect::<Vec<_>>();
    let a = run(Episode::start(&scene).expect("playable"), &actions);
    let b = run(Episode::start(&scene).expect("playable"), &actions);
    assert_eq!(a.steps(), b.steps());
    assert_eq!(a.state_hash(), b.state_hash());
    let serialized = serde_json::to_string(&Replay::capture(&a)).expect("serialize");
    let replay: Replay = serde_json::from_str(&serialized).expect("deserialize");
    assert!(replay.verify().expect("verify").verified);
}
#[test]
fn snapshots_reconstruct_solver_caches_and_continue_identically() {
    let scene = scene("physics_drop.json");
    let mut a = Episode::start(&scene).expect("playable");
    for _ in 0..20 {
        a.apply(Action::Wait).expect("legal");
    }
    let snapshot = Replay::capture(&a);
    let mut b = snapshot.restore().expect("restore");
    while a.outcome() == Outcome::InProgress {
        assert_eq!(
            a.apply(Action::Wait).expect("legal"),
            b.apply(Action::Wait).expect("same step")
        );
    }
    assert_eq!(a.outcome(), Outcome::Won);
    assert_eq!(a.state_hash(), b.state_hash());
}
#[test]
fn invalid_actions_change_no_state_or_history() {
    let scene = scene("physics_launch.json");
    let mut e = Episode::start(&scene).expect("playable");
    let original = e.state_hash();
    for impulse in [
        Vec2::new(100.0, 0.0),
        Vec2::new(f64::NAN, 0.0),
        Vec2::new(f64::INFINITY, 0.0),
    ] {
        assert!(e.apply(Action::Impulse { impulse }).is_err());
        assert_eq!(e.state_hash(), original);
        assert!(e.steps().is_empty());
    }
    e.apply(Action::Impulse {
        impulse: Vec2::new(0.1, 0.0),
    })
    .expect("budget");
    let before = e.state_hash();
    assert!(
        e.apply(Action::Impulse {
            impulse: Vec2::new(0.1, 0.0)
        })
        .is_err()
    );
    assert_eq!(before, e.state_hash());
}
#[test]
fn gravity_accelerates_bodies_and_collisions_keep_them_above_the_floor() {
    let mut document = scene("physics_drop.json").to_document();
    document
        .bodies
        .iter_mut()
        .find(|b| b.id == EntityId::new(2))
        .expect("zone")
        .position
        .x = 8.0;
    document.settings.max_ticks = 180;
    let scene = PhysicsScene::from_document(document).expect("valid");
    let mut e = Episode::start(&scene).expect("playable");
    for _ in 0..20 {
        e.apply(Action::Wait).expect("tick");
    }
    let falling = e
        .states()
        .into_iter()
        .find(|b| b.id == EntityId::new(1))
        .expect("ball");
    assert!(falling.position.y < 4.0 && falling.linear_velocity.y < -2.0);
    let mut touched = false;
    for _ in 20..180 {
        e.apply(Action::Wait).expect("tick");
        touched |= e.contacts().contains(&(EntityId::new(1), EntityId::new(3)));
    }
    let resting = e
        .states()
        .into_iter()
        .find(|b| b.id == EntityId::new(1))
        .expect("ball");
    assert!(touched, "Rapier must resolve a real contact");
    assert!((resting.position.y - 0.25).abs() < 0.04, "{resting:?}");
    assert!(resting.linear_velocity.y.abs() < 0.1);
}
#[test]
fn replay_tampering_and_incompatible_versions_are_rejected() {
    let e = run(
        Episode::start(&scene("physics_launch.json")).expect("start"),
        &[Action::Wait],
    );
    let mut replay = Replay::capture(&e);
    replay.steps[0].action = Action::Forfeit;
    assert!(replay.verify().is_err());
    let mut replay = Replay::capture(&e);
    replay.physics_version = "different Rapier".into();
    assert!(matches!(
        replay.verify(),
        Err(agentforge_physics::replay::ReplayError::Incompatible)
    ));
}
#[test]
fn sensor_goal_is_won_by_real_motion_and_forfeit_and_tick_limits_are_terminal() {
    let scene = scene("physics_launch.json");
    let mut e = Episode::start(&scene).expect("start");
    e.apply(Action::Impulse {
        impulse: Vec2::new(1.0, 0.0),
    })
    .expect("push");
    while e.outcome() == Outcome::InProgress {
        e.apply(Action::Wait).expect("tick");
    }
    assert_eq!(e.outcome(), Outcome::Won);
    assert!(e.tick() > 1);
    assert!(e.apply(Action::Wait).is_err());
    let mut e = Episode::start(&scene).expect("start");
    let result = e.apply(Action::Forfeit).expect("forfeit");
    assert_eq!(result.tick, 1);
    assert_eq!(result.outcome, Outcome::Forfeited);
    let mut e = Episode::start(&scene).expect("start");
    for _ in 0..scene.settings().max_ticks {
        e.apply(Action::Wait).expect("wait");
    }
    assert_eq!(e.outcome(), Outcome::TickLimit);
}

#[test]
fn canonical_encoding_matches_an_independent_python_implementation() {
    // hashlib/struct encoder, independently computed from physics_launch.json.
    let scene = scene("physics_launch.json");
    assert_eq!(
        agentforge_physics::hash::scene(&scene).to_string(),
        "sha256:8dbc12d82c1419454820aa79a7e970cf9c6565069e4b1490212853b905cda6b3"
    );
    assert_eq!(
        Episode::start(&scene)
            .expect("playable")
            .state_hash()
            .to_string(),
        "sha256:63a023b83ca0638b8f537853f76406c38cb2da5d0c9d326585c2843a86e38bb9"
    );
}

#[test]
fn entering_the_goal_on_the_last_tick_wins_at_the_post_step_pose() {
    let mut d = scene("physics_launch.json").to_document();
    d.settings.max_ticks = 1;
    d.bodies[0].linear_velocity = Vec2::new(60.0, 0.0);
    d.bodies[1].position.x = 1.0;
    let scene = PhysicsScene::from_document(d).expect("valid");
    let mut e = Episode::start(&scene).expect("playable");
    let result = e.apply(Action::Wait).expect("tick");
    assert_eq!(e.states()[0].position.x, 1.0);
    assert_eq!(result.outcome, Outcome::Won);
}
#[test]
fn sensor_pairs_report_overlaps_in_both_directions() {
    use agentforge_physics::scene::{BodyKind, Shape};
    let mut d = scene("physics_launch.json").to_document();
    let mut a = d.bodies[1].clone();
    a.id = EntityId::new(3);
    a.kind = BodyKind::Dynamic;
    a.position = Vec2::new(6.0, 0.0);
    a.shape = Shape::Ball { radius: 0.5 };
    let mut b = a.clone();
    b.id = EntityId::new(4);
    b.kind = BodyKind::Fixed;
    d.bodies.extend([a, b]);
    d.next_body_id = 5;
    let scene = PhysicsScene::from_document(d).expect("valid");
    let mut e = Episode::start(&scene).expect("playable");
    e.apply(Action::Wait).expect("tick");
    assert!(e.overlaps().contains(&(EntityId::new(3), EntityId::new(4))));
    assert!(e.overlaps().contains(&(EntityId::new(4), EntityId::new(3))));
}
