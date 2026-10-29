//! Headless SVG previews of the initial, upward-positive physics scene.

use crate::scene::{BodyKind, PhysicsSceneDocument, Shape};

/// Renders initial body poses within the configured scene bounds.
///
/// Physics uses upward-positive Y and counterclockwise radians; SVG uses downward-positive
/// Y and clockwise degrees. Labels stay upright and include stable body IDs. Sensors have
/// translucent green fills and dashed outlines; solid fixed bodies are gray, dynamics blue.
/// The document is expected to satisfy the scene invariants.
pub fn svg(document: &PhysicsSceneDocument) -> String {
    let bounds = document.settings.bounds;
    let width = bounds.max.x - bounds.min.x;
    let height = bounds.max.y - bounds.min.y;
    let left = bounds.min.x;
    let top = -bounds.max.y;
    let unit = width.max(height);
    let stroke = unit / 600.0;
    let font = unit / 80.0;
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{left} {top} {width} {height}\" font-family=\"monospace\">\n<title>Initial physics scene</title>\n<rect class=\"bounds\" x=\"{left}\" y=\"{top}\" width=\"{width}\" height=\"{height}\" fill=\"#f4f1ea\" stroke=\"#777777\" stroke-width=\"{stroke}\"/>\n"
    );
    for body in &document.bodies {
        let x = body.position.x;
        let y = -body.position.y;
        let angle = -body.rotation.to_degrees();
        let (class, fill, outline) = if body.sensor {
            ("sensor", "#a5d6a7", "#2e7d32")
        } else if body.kind == BodyKind::Fixed {
            ("fixed", "#757575", "#424242")
        } else {
            ("dynamic", "#64b5f6", "#1565c0")
        };
        let sensor_style = if body.sensor {
            format!(
                " fill-opacity=\"0.35\" stroke-dasharray=\"{} {}\"",
                stroke * 4.0,
                stroke * 3.0
            )
        } else {
            String::new()
        };
        let caption = match &body.label {
            Some(label) => format!("{}: {}", body.id, label),
            None => body.id.to_string(),
        };
        let caption = escape(&caption);
        out.push_str(&format!(
            "<g id=\"body-{}\" class=\"{class}\"><title>{caption}</title>\n<g transform=\"translate({x} {y}) rotate({angle})\" fill=\"{fill}\" stroke=\"{outline}\" stroke-width=\"{stroke}\"{sensor_style}>\n",
            body.id.get()
        ));
        match body.shape {
            Shape::Ball { radius } => {
                out.push_str(&format!("<circle cx=\"0\" cy=\"0\" r=\"{radius}\"/>\n"))
            }
            Shape::Box {
                half_width,
                half_height,
            } => out.push_str(&format!(
                "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/>\n",
                -half_width,
                -half_height,
                half_width * 2.0,
                half_height * 2.0
            )),
        }
        out.push_str(&format!(
            "</g>\n<text x=\"{x}\" y=\"{y}\" font-size=\"{font}\" text-anchor=\"middle\" fill=\"#212121\" stroke=\"#f4f1ea\" stroke-width=\"{stroke}\" paint-order=\"stroke\">{caption}</text>\n</g>\n"
        ));
    }
    out.push_str("</svg>\n");
    out
}

fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{Bounds, PhysicsScene, Vec2};

    fn launch() -> PhysicsSceneDocument {
        serde_json::from_str(include_str!("../../../fixtures/physics_launch.json"))
            .expect("raw physics document")
    }

    #[test]
    fn renders_bounds_and_initial_ball_with_upward_positive_y() {
        let mut document = launch();
        document.settings.bounds = Bounds {
            min: Vec2::new(-3.0, -2.0),
            max: Vec2::new(7.0, 8.0),
        };
        document.bodies[0].position = Vec2::new(1.0, 4.0);
        document.bodies[0].linear_velocity = Vec2::new(10.0, 20.0);
        let output = svg(&document);
        assert!(output.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(output.contains("viewBox=\"-3 -8 10 10\""));
        assert!(output.contains("class=\"bounds\" x=\"-3\" y=\"-8\" width=\"10\" height=\"10\""));
        assert!(output.contains("translate(1 -4) rotate(-0)"));
        assert!(output.contains("<circle cx=\"0\" cy=\"0\" r=\"0.25\"/>"));
        assert!(output.ends_with("</svg>\n"));
    }

    #[test]
    fn renders_box_half_extents_and_converts_rotation_without_rotating_label() {
        let mut document = launch();
        document.bodies[1].rotation = std::f64::consts::FRAC_PI_2;
        let output = svg(&document);
        assert!(output.contains("translate(4 -0) rotate(-90)"));
        assert!(output.contains("<rect x=\"-0.4\" y=\"-0.8\" width=\"0.8\" height=\"1.6\"/>"));
        assert!(output.contains("</g>\n<text x=\"4\" y=\"-0\""));
    }

    #[test]
    fn distinguishes_sensors_fixed_and_dynamic_bodies_and_escapes_labels() {
        let mut document: PhysicsSceneDocument =
            serde_json::from_str(include_str!("../../../fixtures/physics_drop.json"))
                .expect("raw drop document");
        document.bodies[0].label = Some("<script> & \"ball\" 'α'".into());
        let output = svg(&document);
        assert!(output.contains("id=\"body-1\" class=\"dynamic\""));
        assert!(output.contains("id=\"body-2\" class=\"sensor\""));
        assert!(output.contains("id=\"body-3\" class=\"fixed\""));
        assert_eq!(output.matches("stroke-dasharray=").count(), 1);
        assert!(output.contains("fill-opacity=\"0.35\""));
        assert!(output.contains("#1: &lt;script&gt; &amp; &quot;ball&quot; &apos;α&apos;</text>"));
        assert!(output.contains(">#2: Goal</text>"));
        assert!(!output.contains("<script>"));
        assert_eq!(output, svg(&document));
    }

    #[test]
    fn renders_empty_scene() {
        let mut document = launch();
        document.bodies.clear();
        let output = svg(&document);
        assert!(output.contains("class=\"bounds\""));
        assert!(!output.contains("id=\"body-"));
        assert!(output.ends_with("</svg>\n"));
    }

    #[test]
    fn fixtures_are_valid_playable_raw_documents() {
        for json in [
            include_str!("../../../fixtures/physics_launch.json"),
            include_str!("../../../fixtures/physics_drop.json"),
        ] {
            let document: PhysicsSceneDocument = serde_json::from_str(json).expect("raw document");
            assert_eq!(document.settings.ticks_per_second, 60);
            assert_eq!(document.settings.max_ticks, 180);
            let objective = document.settings.objective.expect("objective");
            let control = document.settings.control.expect("control");
            assert_eq!(objective.body.get(), 1);
            assert_eq!(objective.zone.get(), 2);
            assert_eq!(control.body.get(), 1);
            assert_eq!(control.max_impulse, 5.0);
            assert_eq!(control.max_impulses, 1);
            PhysicsScene::from_document(document).expect("valid scene references and geometry");
        }
    }
}
