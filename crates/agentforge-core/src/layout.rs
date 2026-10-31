//! ASCII layouts: a compact scene description that language models read and write well.
//!
//! | glyph | meaning |
//! |-------|---------|
//! | `.` or space | floor |
//! | `#` | wall |
//! | `@` | player |
//! | `E` | exit |
//! | `~` | ice |
//! | `^ > v <` | one-way tile allowing entry north, east, south or west |
//! | `!` | hazard |
//! | `0`-`9` | portal with pair id 0-9 |
//! | `( [ { -` | switch of gate group 0, 1, 2 or 3 |
//! | `) ] } =` | gate of gate group 0, 1, 2 or 3 |
//! | `r g b y p o c w` | key (red, green, blue, yellow, purple, orange, cyan, white) |
//! | `R G B Y P O C W` | door of the matching color |

use schemars::JsonSchema;
use serde::Serialize;

use crate::entity::{EntityKind, KeyColor, MAX_GATE_GROUPS};
use crate::geometry::{Direction, GridSize, Position};

/// Empty floor.
pub const FLOOR: char = '.';
/// Alternative floor glyph accepted on input.
pub const FLOOR_ALT: char = ' ';
/// Wall.
pub const WALL: char = '#';
/// Player (spawn point in scenes, current position in observations).
pub const PLAYER: char = '@';
/// Exit.
pub const EXIT: char = 'E';
/// Ice.
pub const ICE: char = '~';
/// Hazard.
pub const HAZARD: char = '!';

/// Glyph of a one-way tile allowing entry while moving in `direction`.
pub const fn one_way_glyph(direction: Direction) -> char {
    match direction {
        Direction::North => '^',
        Direction::East => '>',
        Direction::South => 'v',
        Direction::West => '<',
    }
}

/// Glyph of a portal pair. Pair ids outside `0..=9` have no glyph.
pub const fn portal_glyph(pair: u8) -> char {
    match pair {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        _ => '?',
    }
}

/// Portal pair id denoted by a digit glyph.
pub fn portal_pair(glyph: char) -> Option<u8> {
    glyph
        .to_digit(10)
        .and_then(|digit| u8::try_from(digit).ok())
}

/// Glyph of the switch of gate group `group`. Groups outside `0..MAX_GATE_GROUPS` have no
/// glyph.
pub const fn switch_glyph(group: u8) -> char {
    match group {
        0 => '(',
        1 => '[',
        2 => '{',
        3 => '-',
        _ => '?',
    }
}

/// Glyph of the gate of gate group `group`. Groups outside `0..MAX_GATE_GROUPS` have no
/// glyph.
pub const fn gate_glyph(group: u8) -> char {
    match group {
        0 => ')',
        1 => ']',
        2 => '}',
        3 => '=',
        _ => '?',
    }
}

/// Gate group denoted by a switch glyph.
pub const fn switch_group(glyph: char) -> Option<u8> {
    match glyph {
        '(' => Some(0),
        '[' => Some(1),
        '{' => Some(2),
        '-' => Some(3),
        _ => None,
    }
}

/// Gate group denoted by a gate glyph.
pub const fn gate_group(glyph: char) -> Option<u8> {
    match glyph {
        ')' => Some(0),
        ']' => Some(1),
        '}' => Some(2),
        '=' => Some(3),
        _ => None,
    }
}

/// Whether `group` is a supported gate group id.
pub const fn supported_gate_group(group: u8) -> bool {
    group < MAX_GATE_GROUPS
}

/// A parsed layout: grid size plus the entities to create, in row-major order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedLayout {
    /// Grid dimensions derived from the rows.
    pub size: GridSize,
    /// Entities to create, row by row, left to right.
    pub cells: Vec<(Position, EntityKind)>,
}

/// Why a layout could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    /// No rows, or an empty first row.
    #[error("layout must contain at least one non-empty row")]
    Empty,
    /// Rows of different widths.
    #[error("row {row} has {actual} cells but row 0 has {expected}; all rows must be equally wide")]
    RaggedRow {
        /// Zero-based row index.
        row: usize,
        /// Width of row 0.
        expected: usize,
        /// Width of this row.
        actual: usize,
    },
    /// Wider or taller than supported.
    #[error("layout is {width}x{height}; the maximum is {max}x{max}")]
    TooLarge {
        /// Layout width.
        width: usize,
        /// Layout height.
        height: usize,
        /// Largest supported dimension.
        max: u32,
    },
    /// A character outside the legend.
    #[error("unknown glyph {glyph:?} at {position}")]
    UnknownGlyph {
        /// The character.
        glyph: char,
        /// Where it was found.
        position: Position,
    },
}

/// One legend row, as published by `discover` and observations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LegendEntry {
    /// The glyph.
    pub glyph: String,
    /// What it denotes.
    pub meaning: String,
}

/// Maps a glyph to the entity it denotes: `Some(None)` for floor, `None` if unknown.
pub fn glyph_kind(glyph: char) -> Option<Option<EntityKind>> {
    match glyph {
        FLOOR | FLOOR_ALT => Some(None),
        WALL => Some(Some(EntityKind::Wall)),
        PLAYER => Some(Some(EntityKind::Player)),
        EXIT => Some(Some(EntityKind::Exit)),
        ICE => Some(Some(EntityKind::Ice)),
        HAZARD => Some(Some(EntityKind::Hazard)),
        '^' => Some(Some(EntityKind::OneWay {
            direction: Direction::North,
        })),
        '>' => Some(Some(EntityKind::OneWay {
            direction: Direction::East,
        })),
        'v' => Some(Some(EntityKind::OneWay {
            direction: Direction::South,
        })),
        '<' => Some(Some(EntityKind::OneWay {
            direction: Direction::West,
        })),
        other if let Some(pair) = portal_pair(other) => Some(Some(EntityKind::Portal { pair })),
        other if let Some(group) = switch_group(other) => Some(Some(EntityKind::Switch { group })),
        other if let Some(group) = gate_group(other) => Some(Some(EntityKind::Gate { group })),
        other => KeyColor::from_key_glyph(other)
            .map(|color| Some(EntityKind::Key { color }))
            .or_else(|| {
                KeyColor::from_door_glyph(other).map(|color| Some(EntityKind::Door { color }))
            }),
    }
}

/// Parses equally wide rows of glyphs into a grid size and entity list.
pub fn parse(rows: &[String]) -> Result<ParsedLayout, LayoutError> {
    let width = rows.first().map_or(0, |row| row.chars().count());
    if width == 0 {
        return Err(LayoutError::Empty);
    }
    let max = GridSize::MAX_DIMENSION as usize;
    if width > max || rows.len() > max {
        return Err(LayoutError::TooLarge {
            width,
            height: rows.len(),
            max: GridSize::MAX_DIMENSION,
        });
    }
    let mut cells = Vec::new();
    for (y, row) in rows.iter().enumerate() {
        let actual = row.chars().count();
        if actual != width {
            return Err(LayoutError::RaggedRow {
                row: y,
                expected: width,
                actual,
            });
        }
        for (x, glyph) in row.chars().enumerate() {
            // Both coordinates are bounded by MAX_DIMENSION (64), so they fit in i32.
            let position = Position::new(x as i32, y as i32);
            match glyph_kind(glyph) {
                Some(Some(kind)) => cells.push((position, kind)),
                Some(None) => {}
                None => return Err(LayoutError::UnknownGlyph { glyph, position }),
            }
        }
    }
    Ok(ParsedLayout {
        size: GridSize::new(width as u32, rows.len() as u32),
        cells,
    })
}

/// Renders glyphs onto a floor-filled grid. Later glyphs overwrite earlier ones on the same
/// cell, which lets callers draw the player on top of the cell it stands on.
pub fn render(size: GridSize, glyphs: impl IntoIterator<Item = (Position, char)>) -> Vec<String> {
    let mut grid = vec![FLOOR; size.cell_count()];
    for (position, glyph) in glyphs {
        if let Some(index) = size.index(position) {
            grid[index] = glyph;
        }
    }
    grid.chunks(size.width as usize)
        .map(|row| row.iter().collect())
        .collect()
}

/// The glyph legend.
pub fn legend() -> Vec<LegendEntry> {
    let mut entries = vec![
        entry(FLOOR, "floor (a space is also accepted on input)"),
        entry(WALL, "wall"),
        entry(PLAYER, "player"),
        entry(EXIT, "exit"),
        entry(
            ICE,
            "ice (entering it slides the player in the direction of travel)",
        ),
    ];
    for direction in Direction::ALL {
        entries.push(entry(
            one_way_glyph(direction),
            &format!("one-way tile: entry allowed only while moving {direction}"),
        ));
    }
    entries.push(entry(
        HAZARD,
        "hazard (entering it fails the episode immediately)",
    ));
    for pair in 0..=9 {
        entries.push(entry(
            portal_glyph(pair),
            &format!("portal pair {pair} (teleports to the other {pair})"),
        ));
    }
    for group in 0..MAX_GATE_GROUPS {
        entries.push(entry(
            switch_glyph(group),
            &format!(
                "switch of gate group {group} (entry toggles all gates of group {group} open/closed)"
            ),
        ));
    }
    for group in 0..MAX_GATE_GROUPS {
        entries.push(entry(
            gate_glyph(group),
            &format!(
                "gate of gate group {group} (impassable while closed; passable terrain while open; see the observation's `gates`)"
            ),
        ));
    }
    for color in KeyColor::ALL {
        entries.push(entry(color.key_glyph(), &format!("{color} key")));
    }
    for color in KeyColor::ALL {
        entries.push(entry(color.door_glyph(), &format!("{color} door")));
    }
    entries
}

fn entry(glyph: char, meaning: &str) -> LegendEntry {
    LegendEntry {
        glyph: glyph.to_string(),
        meaning: meaning.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn parses_every_glyph_class_in_row_major_order() {
        let parsed = parse(&rows(&["#@r", "R E"])).expect("valid layout");
        assert_eq!(parsed.size, GridSize::new(3, 2));
        let kinds: Vec<char> = parsed.cells.iter().map(|(_, kind)| kind.glyph()).collect();
        assert_eq!(kinds, vec!['#', '@', 'r', 'R', 'E']);
        assert_eq!(parsed.cells[4].0, Position::new(2, 1));
    }

    #[test]
    fn rejects_ragged_unknown_and_empty_layouts() {
        assert_eq!(
            parse(&rows(&["###", "##"])),
            Err(LayoutError::RaggedRow {
                row: 1,
                expected: 3,
                actual: 2
            })
        );
        assert!(matches!(
            parse(&rows(&["#X#"])),
            Err(LayoutError::UnknownGlyph { glyph: 'X', .. })
        ));
        assert_eq!(parse(&[]), Err(LayoutError::Empty));
        assert!(matches!(
            parse(&rows(&[&"#".repeat(65)])),
            Err(LayoutError::TooLarge { .. })
        ));
    }

    #[test]
    fn render_round_trips_parse() {
        let source = rows(&["#####", "#@.E#", "#####"]);
        let parsed = parse(&source).expect("valid");
        let rendered = render(
            parsed.size,
            parsed.cells.iter().map(|(p, kind)| (*p, kind.glyph())),
        );
        assert_eq!(rendered, source);
    }

    #[test]
    fn terrain_glyphs_parse_and_render_round_trip() {
        let source = rows(&["~^>v<!0123456789"]);
        let parsed = parse(&source).expect("valid terrain");
        assert_eq!(parsed.cells[0].1, EntityKind::Ice);
        assert_eq!(
            parsed.cells[1].1,
            EntityKind::OneWay {
                direction: Direction::North
            }
        );
        assert_eq!(
            parsed.cells[2].1,
            EntityKind::OneWay {
                direction: Direction::East
            }
        );
        assert_eq!(
            parsed.cells[3].1,
            EntityKind::OneWay {
                direction: Direction::South
            }
        );
        assert_eq!(
            parsed.cells[4].1,
            EntityKind::OneWay {
                direction: Direction::West
            }
        );
        assert_eq!(parsed.cells[5].1, EntityKind::Hazard);
        assert_eq!(parsed.cells[6].1, EntityKind::Portal { pair: 0 });
        assert_eq!(parsed.cells[15].1, EntityKind::Portal { pair: 9 });
        let rendered = render(
            parsed.size,
            parsed.cells.iter().map(|(p, kind)| (*p, kind.glyph())),
        );
        assert_eq!(rendered, source);
        assert_eq!(portal_pair('7'), Some(7));
        assert_eq!(portal_pair('x'), None);
        assert_eq!(portal_glyph(42), '?');
        assert!(legend().iter().any(|e| e.glyph == "~"));
        assert!(legend().iter().any(|e| e.glyph == "!"));
        assert!(legend().iter().any(|e| e.glyph == "9"));
    }

    #[test]
    fn switch_and_gate_glyphs_parse_render_round_trip_and_land_in_the_legend() {
        let source = rows(&["([{-", ")]}="]);
        let parsed = parse(&source).expect("valid gate layout");
        for group in 0..4u8 {
            let position = Position::new(group as i32, 0);
            assert_eq!(
                switch_group(switch_glyph(group)),
                Some(group),
                "switch glyph group {group}"
            );
            assert_eq!(gate_group(gate_glyph(group)), Some(group));
            assert_eq!(
                parsed
                    .cells
                    .iter()
                    .find(|(p, _)| *p == position)
                    .expect("cell")
                    .1,
                EntityKind::Switch { group },
                "row 0 holds group {group}'s switch"
            );
            assert_eq!(
                parsed
                    .cells
                    .iter()
                    .find(|(p, _)| *p == Position::new(group as i32, 1))
                    .expect("cell")
                    .1,
                EntityKind::Gate { group }
            );
        }
        let rendered = render(
            parsed.size,
            parsed.cells.iter().map(|(p, kind)| (*p, kind.glyph())),
        );
        assert_eq!(rendered, source);
        let legend_entries = legend();
        let legend_glyphs: Vec<&str> = legend_entries.iter().map(|e| e.glyph.as_str()).collect();
        for glyph in ['(', '[', '{', '-', ')', ']', '}', '='] {
            let glyph = glyph.to_string();
            assert!(
                legend_glyphs.contains(&glyph.as_str()),
                "legend is missing {glyph}"
            );
        }
    }
}
