//! Headless SVG rendering for human inspection.
//!
//! Secondary to structured observations: no GUI, GPU or font files are needed, and the
//! output is a plain string. Every entity is labelled with its id so diagnostics such as
//! "remove wall #42" can be located at a glance.

use crate::entity::EntityKind;
use crate::geometry::Position;
use crate::scene::Scene;

const CELL: i32 = 40;
const CAPTION_HEIGHT: i32 = 28;
const FLOOR_FILL: &str = "#f4f1ea";
const GRID_STROKE: &str = "#d9d4c7";
const WALL_FILL: &str = "#3d3d3d";
const EXIT_FILL: &str = "#2e7d32";
const PLAYER_FILL: &str = "#1565c0";

/// Renders `scene` as an SVG document captioned with `title`.
pub fn scene_svg(scene: &Scene, title: &str) -> String {
    let size = scene.size();
    // Grid dimensions are at most 64, so these products cannot overflow.
    let width = size.width as i32 * CELL;
    let height = size.height as i32 * CELL;
    let rules = scene.rules();
    let mut svg = String::new();
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{total}\" viewBox=\"0 0 {width} {total}\" font-family=\"monospace\">\n",
        total = height + CAPTION_HEIGHT
    ));
    svg.push_str(&format!("<title>{}</title>\n", escape(title)));
    svg.push_str(&format!(
        "<rect x=\"0\" y=\"0\" width=\"{width}\" height=\"{height}\" fill=\"{FLOOR_FILL}\"/>\n"
    ));
    for x in 0..=size.width as i32 {
        svg.push_str(&format!(
            "<line x1=\"{0}\" y1=\"0\" x2=\"{0}\" y2=\"{height}\" stroke=\"{GRID_STROKE}\"/>\n",
            x * CELL
        ));
    }
    for y in 0..=size.height as i32 {
        svg.push_str(&format!(
            "<line x1=\"0\" y1=\"{0}\" x2=\"{width}\" y2=\"{0}\" stroke=\"{GRID_STROKE}\"/>\n",
            y * CELL
        ));
    }
    for entity in scene.entities() {
        svg.push_str(&entity_svg(entity.kind, entity.position));
        let (x, y) = origin(entity.position);
        let id_fill = if entity.kind == EntityKind::Wall {
            "#bdbdbd"
        } else {
            "#212121"
        };
        svg.push_str(&format!(
            "<text x=\"{}\" y=\"{}\" font-size=\"9\" fill=\"{id_fill}\">{}</text>\n",
            x + 2,
            y + 9,
            entity.id
        ));
    }
    svg.push_str(&format!(
        "<text x=\"4\" y=\"{}\" font-size=\"12\" fill=\"#212121\">{} | {} | max_ticks {} | win: {}</text>\n",
        height + 18,
        escape(title),
        size,
        rules.max_ticks,
        rules.win_condition
    ));
    svg.push_str("</svg>\n");
    svg
}

fn origin(position: Position) -> (i32, i32) {
    (position.x * CELL, position.y * CELL)
}

fn entity_svg(kind: EntityKind, position: Position) -> String {
    let (x, y) = origin(position);
    let (cx, cy) = (x + CELL / 2, y + CELL / 2);
    match kind {
        EntityKind::Wall => format!(
            "<rect x=\"{x}\" y=\"{y}\" width=\"{CELL}\" height=\"{CELL}\" fill=\"{WALL_FILL}\"/>\n"
        ),
        EntityKind::Exit => format!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{EXIT_FILL}\"/>\n<text x=\"{cx}\" y=\"{}\" font-size=\"10\" fill=\"#ffffff\" text-anchor=\"middle\">EXIT</text>\n",
            x + 3,
            y + 3,
            CELL - 6,
            CELL - 6,
            cy + 4
        ),
        EntityKind::Player => format!(
            "<circle cx=\"{cx}\" cy=\"{cy}\" r=\"13\" fill=\"{PLAYER_FILL}\"/>\n<text x=\"{cx}\" y=\"{}\" font-size=\"13\" fill=\"#ffffff\" text-anchor=\"middle\">@</text>\n",
            cy + 5
        ),
        EntityKind::Key { color } => format!(
            "<circle cx=\"{cx}\" cy=\"{cy}\" r=\"9\" fill=\"{}\" stroke=\"#212121\" stroke-width=\"2\"/>\n<text x=\"{cx}\" y=\"{}\" font-size=\"10\" fill=\"#212121\" text-anchor=\"middle\">{}</text>\n",
            color.rgb(),
            cy + 4,
            color.key_glyph()
        ),
        EntityKind::Door { color } => format!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\" stroke=\"#212121\" stroke-width=\"3\"/>\n<text x=\"{cx}\" y=\"{}\" font-size=\"12\" fill=\"#212121\" text-anchor=\"middle\">{}</text>\n",
            x + 4,
            y + 4,
            CELL - 8,
            CELL - 8,
            color.rgb(),
            cy + 4,
            color.door_glyph()
        ),
    }
}

/// Escapes text for XML content and attribute values.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::GridSize;
    use crate::rules::Rules;
    use crate::scene::SceneEdit;

    #[test]
    fn renders_every_entity_with_its_id_and_escapes_titles() {
        let edits = [EntityKind::Player, EntityKind::Wall, EntityKind::Exit]
            .into_iter()
            .zip(0..)
            .map(|(kind, x)| SceneEdit::CreateEntity {
                kind,
                position: Position::new(x, 0),
                label: None,
            })
            .collect::<Vec<_>>();
        let scene = Scene::new(GridSize::new(3, 1), Rules::default())
            .expect("valid")
            .apply_edits(&edits)
            .expect("valid")
            .0;
        let svg = scene_svg(&scene, "a <b> & \"c\"");
        assert!(svg.starts_with("<svg "));
        assert!(svg.trim_end().ends_with("</svg>"));
        for id in ["#1", "#2", "#3"] {
            assert!(svg.contains(&format!(">{id}</text>")), "missing {id}");
        }
        assert!(svg.contains("a &lt;b&gt; &amp; &quot;c&quot;"));
        assert!(!svg.contains("<b>"));
    }
}
