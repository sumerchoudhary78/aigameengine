//! The scripted authoring client.
//!
//! It never computes a repair itself. After a failed playtest it asks the engine for a
//! diagnosis and applies the first repair the engine reports as verified, identified only
//! by the entity id in that response.

use std::path::Path;

use serde_json::{Value, json};

use crate::client::{ClientError, EngineSession};

/// Name of the scene the builder authors.
pub const BLOCKED_DUNGEON: &str = "blocked_dungeon";

/// A dungeon whose exit room (right) is sealed off: the double wall between the rooms has
/// one gap, and a single wall plugs it.
pub const BLOCKED_DUNGEON_LAYOUT: [&str; 7] = [
    "#############",
    "#@....##....#",
    "#.##..##.##.#",
    "#...#.#...#.#",
    "#.#...##.#..#",
    "#.....##...E#",
    "#############",
];

/// Rules of the blocked dungeon.
pub fn blocked_dungeon_rules() -> Value {
    json!({"max_ticks": 40, "win_condition": "reach_exit"})
}

/// `project.create` at the session's current revision.
pub fn create_project(session: &mut EngineSession, name: &str) -> Result<Value, ClientError> {
    let revision = session.revision();
    session.call(
        "project.create",
        json!({"name": name, "expected_revision": revision}),
    )
}

/// `scene.create` from an ASCII layout.
pub fn create_scene(
    session: &mut EngineSession,
    name: &str,
    layout: &[&str],
    rules: Value,
) -> Result<Value, ClientError> {
    let revision = session.revision();
    session.call(
        "scene.create",
        json!({"name": name, "expected_revision": revision, "layout": layout, "rules": rules}),
    )
}

/// `project.save` to `path`, guarded by the current revision.
pub fn save(session: &mut EngineSession, path: &Path) -> Result<Value, ClientError> {
    let revision = session.revision();
    session.call(
        "project.save",
        json!({"path": path.display().to_string(), "expected_revision": revision}),
    )
}

/// `scene.render` as SVG into `path`.
pub fn render_svg(
    session: &mut EngineSession,
    scene: &str,
    path: &Path,
) -> Result<Value, ClientError> {
    session.call(
        "scene.render",
        json!({"scene": scene, "format": "svg", "path": path.display().to_string()}),
    )
}

/// `analysis.reachability` towards the scene objective.
pub fn diagnose(session: &mut EngineSession, scene: &str) -> Result<Value, ClientError> {
    session.call("analysis.reachability", json!({"scene": scene}))
}

/// `scene.validate`.
pub fn validate(session: &mut EngineSession, scene: &str) -> Result<Value, ClientError> {
    session.call("scene.validate", json!({"scene": scene}))
}

/// Applies the first verified single-removal repair listed in `diagnosis`, if any.
/// Returns the candidate and the engine's `entity.remove` result.
pub fn apply_verified_repair(
    session: &mut EngineSession,
    scene: &str,
    diagnosis: &Value,
) -> Result<Option<(Value, Value)>, ClientError> {
    let Some(candidate) = diagnosis["repairs"]["single_removals"].get(0).cloned() else {
        return Ok(None);
    };
    let revision = session.revision();
    let removed = session.call(
        "entity.remove",
        json!({"scene": scene, "expected_revision": revision, "id": candidate["remove"]}),
    )?;
    Ok(Some((candidate, removed)))
}
