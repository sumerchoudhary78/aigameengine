//! Entity kinds, key colors and key sets.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::geometry::{Direction, Position};
use crate::ids::EntityId;

/// Color shared by a key and the doors it opens. A scene holds at most one key per color.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum KeyColor {
    /// ASCII glyphs: key `r`, door `R`.
    Red,
    /// ASCII glyphs: key `g`, door `G`.
    Green,
    /// ASCII glyphs: key `b`, door `B`.
    Blue,
    /// ASCII glyphs: key `y`, door `Y`.
    Yellow,
    /// ASCII glyphs: key `p`, door `P`.
    Purple,
    /// ASCII glyphs: key `o`, door `O`.
    Orange,
    /// ASCII glyphs: key `c`, door `C`.
    Cyan,
    /// ASCII glyphs: key `w`, door `W`.
    White,
}

impl KeyColor {
    /// All colors in canonical order. A color's position in this array is its bit index in
    /// a [`KeySet`] and (plus one) its code in the canonical hash encoding.
    pub const ALL: [KeyColor; 8] = [
        KeyColor::Red,
        KeyColor::Green,
        KeyColor::Blue,
        KeyColor::Yellow,
        KeyColor::Purple,
        KeyColor::Orange,
        KeyColor::Cyan,
        KeyColor::White,
    ];

    /// Position of this color in [`KeyColor::ALL`].
    pub const fn index(self) -> u8 {
        match self {
            KeyColor::Red => 0,
            KeyColor::Green => 1,
            KeyColor::Blue => 2,
            KeyColor::Yellow => 3,
            KeyColor::Purple => 4,
            KeyColor::Orange => 5,
            KeyColor::Cyan => 6,
            KeyColor::White => 7,
        }
    }

    /// Wire name of the color.
    pub const fn as_str(self) -> &'static str {
        match self {
            KeyColor::Red => "red",
            KeyColor::Green => "green",
            KeyColor::Blue => "blue",
            KeyColor::Yellow => "yellow",
            KeyColor::Purple => "purple",
            KeyColor::Orange => "orange",
            KeyColor::Cyan => "cyan",
            KeyColor::White => "white",
        }
    }

    /// Lowercase ASCII glyph of a key of this color.
    pub const fn key_glyph(self) -> char {
        match self {
            KeyColor::Red => 'r',
            KeyColor::Green => 'g',
            KeyColor::Blue => 'b',
            KeyColor::Yellow => 'y',
            KeyColor::Purple => 'p',
            KeyColor::Orange => 'o',
            KeyColor::Cyan => 'c',
            KeyColor::White => 'w',
        }
    }

    /// Uppercase ASCII glyph of a door of this color.
    pub const fn door_glyph(self) -> char {
        self.key_glyph().to_ascii_uppercase()
    }

    /// The color whose key glyph is `glyph`.
    pub fn from_key_glyph(glyph: char) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|color| color.key_glyph() == glyph)
    }

    /// The color whose door glyph is `glyph`.
    pub fn from_door_glyph(glyph: char) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|color| color.door_glyph() == glyph)
    }

    /// Fill color used by the SVG renderer.
    pub const fn rgb(self) -> &'static str {
        match self {
            KeyColor::Red => "#d32f2f",
            KeyColor::Green => "#388e3c",
            KeyColor::Blue => "#1976d2",
            KeyColor::Yellow => "#fbc02d",
            KeyColor::Purple => "#7b1fa2",
            KeyColor::Orange => "#f57c00",
            KeyColor::Cyan => "#0097a7",
            KeyColor::White => "#fafafa",
        }
    }
}

impl fmt::Display for KeyColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A set of key colors, stored as a bitmask indexed by [`KeyColor::index`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct KeySet(u8);

impl KeySet {
    /// The empty set.
    pub const EMPTY: KeySet = KeySet(0);

    /// Creates a set from its raw bitmask.
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// The raw bitmask (bit `i` set means `KeyColor::ALL[i]` is present).
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether `color` is in the set.
    pub const fn contains(self, color: KeyColor) -> bool {
        self.0 & (1 << color.index()) != 0
    }

    /// Adds `color` to the set.
    pub fn insert(&mut self, color: KeyColor) {
        self.0 |= 1 << color.index();
    }

    /// Whether every color of `other` is also in `self`.
    pub const fn is_superset(self, other: KeySet) -> bool {
        self.0 & other.0 == other.0
    }

    /// Colors of `self` that are not in `other`.
    pub const fn difference(self, other: KeySet) -> KeySet {
        KeySet(self.0 & !other.0)
    }

    /// Whether the set is empty.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Members in canonical color order.
    pub fn colors(self) -> Vec<KeyColor> {
        KeyColor::ALL
            .into_iter()
            .filter(|color| self.contains(*color))
            .collect()
    }
}

impl FromIterator<KeyColor> for KeySet {
    fn from_iter<I: IntoIterator<Item = KeyColor>>(iter: I) -> Self {
        let mut set = KeySet::EMPTY;
        for color in iter {
            set.insert(color);
        }
        set
    }
}

/// The set of gate groups whose gates are currently **open**, stored as a bitmask indexed
/// by group id. A new episode starts with no open groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct GateSet(u8);

impl GateSet {
    /// No open groups: every gate closed.
    pub const EMPTY: GateSet = GateSet(0);

    /// Creates a set from its raw bitmask.
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// The raw bitmask (bit `i` set means group `i` is open).
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether `group` is open.
    pub const fn contains(self, group: u8) -> bool {
        self.0 & (1 << group) != 0
    }

    /// Flips `group`: open becomes closed and closed becomes open.
    pub const fn toggled(self, group: u8) -> GateSet {
        GateSet(self.0 ^ (1 << group))
    }

    /// Groups of `self` in ascending id order.
    pub fn groups(self) -> Vec<u8> {
        (0..MAX_GATE_GROUPS)
            .filter(|group| self.contains(*group))
            .collect()
    }
}

/// Number of distinct portal pair ids: valid ids are `0..MAX_PORTAL_PAIRS`.
pub const MAX_PORTAL_PAIRS: u8 = 10;

/// Number of distinct gate groups: valid ids are `0..MAX_GATE_GROUPS`.
pub const MAX_GATE_GROUPS: u8 = 4;

/// What an entity is. Serialized with a `type` tag, e.g. `{"type": "door", "color": "red"}`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EntityKind {
    /// The player's spawn point. A scene has at most one.
    Player,
    /// Impassable.
    Wall,
    /// Collected by stepping onto it; never consumed.
    Key {
        /// Doors of this color open for a player holding this key.
        color: KeyColor,
    },
    /// Passable only while the player holds the key of the same color.
    Door {
        /// The key color that opens this door.
        color: KeyColor,
    },
    /// Winning target, subject to the scene's win condition.
    Exit,
    /// Slippery floor. Entering it starts a slide in the direction of travel; the slide
    /// continues until a wall, locked door, wrong-way one-way tile or the grid edge stops it,
    /// or until the player enters an object.
    Ice,
    /// Passable only while moving in `direction`. Leaving it is always allowed.
    OneWay {
        /// The entry direction that is allowed.
        direction: Direction,
    },
    /// Entering it fails the episode immediately.
    Hazard,
    /// Teleports the player to the other portal with the same `pair` id.
    Portal {
        /// Link id; exactly two portals share an id in a playable scene.
        #[schemars(range(min = 0, max = 9))]
        pair: u8,
    },
    /// Pressure plate. Entering it toggles its group between "all gates closed" and "all
    /// gates open"; the state persists until the next toggle.
    Switch {
        /// The gate group this switch toggles.
        #[schemars(range(min = 0, max = 3))]
        group: u8,
    },
    /// Barrier that is passable while its group is open and impassable while it is closed.
    Gate {
        /// The gate group this gate belongs to.
        #[schemars(range(min = 0, max = 3))]
        group: u8,
    },
}

impl EntityKind {
    /// Wire name of the kind (the `type` tag).
    pub const fn name(self) -> &'static str {
        match self {
            EntityKind::Player => "player",
            EntityKind::Wall => "wall",
            EntityKind::Key { .. } => "key",
            EntityKind::Door { .. } => "door",
            EntityKind::Exit => "exit",
            EntityKind::Ice => "ice",
            EntityKind::OneWay { .. } => "one_way",
            EntityKind::Hazard => "hazard",
            EntityKind::Portal { .. } => "portal",
            EntityKind::Switch { .. } => "switch",
            EntityKind::Gate { .. } => "gate",
        }
    }

    /// The key color carried by keys and doors.
    pub const fn color(self) -> Option<KeyColor> {
        match self {
            EntityKind::Key { color } | EntityKind::Door { color } => Some(color),
            _ => None,
        }
    }

    /// The gate group carried by switches and gates.
    pub const fn group(self) -> Option<u8> {
        match self {
            EntityKind::Switch { group } | EntityKind::Gate { group } => Some(group),
            _ => None,
        }
    }

    /// ASCII glyph used by layouts and renders.
    pub const fn glyph(self) -> char {
        match self {
            EntityKind::Player => crate::layout::PLAYER,
            EntityKind::Wall => crate::layout::WALL,
            EntityKind::Exit => crate::layout::EXIT,
            EntityKind::Key { color } => color.key_glyph(),
            EntityKind::Door { color } => color.door_glyph(),
            EntityKind::Ice => crate::layout::ICE,
            EntityKind::OneWay { direction } => crate::layout::one_way_glyph(direction),
            EntityKind::Hazard => crate::layout::HAZARD,
            EntityKind::Portal { pair } => crate::layout::portal_glyph(pair),
            EntityKind::Switch { group } => crate::layout::switch_glyph(group),
            EntityKind::Gate { group } => crate::layout::gate_glyph(group),
        }
    }
}

impl fmt::Display for EntityKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.group() {
            Some(group) if matches!(self, EntityKind::Switch { .. }) => {
                write!(f, "switch group {group}")
            }
            Some(group) if matches!(self, EntityKind::Gate { .. }) => {
                write!(f, "gate group {group}")
            }
            _ => match self.color() {
                Some(color) => write!(f, "{color} {}", self.name()),
                None => f.write_str(self.name()),
            },
        }
    }
}

/// An entity placed on the grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Entity {
    /// Stable identifier, unique within the scene's history.
    pub id: EntityId,
    /// What the entity is.
    pub kind: EntityKind,
    /// The cell it occupies.
    pub position: Position,
    /// Optional authoring note. Never shown to players and never part of state hashes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_round_trip_for_every_color() {
        for color in KeyColor::ALL {
            assert_eq!(KeyColor::from_key_glyph(color.key_glyph()), Some(color));
            assert_eq!(KeyColor::from_door_glyph(color.door_glyph()), Some(color));
        }
        assert_eq!(KeyColor::from_key_glyph('E'), None);
    }

    #[test]
    fn key_set_operations() {
        let held: KeySet = [KeyColor::Red, KeyColor::Blue].into_iter().collect();
        let needed: KeySet = [KeyColor::Red].into_iter().collect();
        assert!(held.is_superset(needed));
        assert!(!needed.is_superset(held));
        assert_eq!(held.difference(needed).colors(), vec![KeyColor::Blue]);
        assert_eq!(held.colors(), vec![KeyColor::Red, KeyColor::Blue]);
    }

    #[test]
    fn kinds_use_a_type_tag_and_reject_unknown_fields() {
        let door: EntityKind =
            serde_json::from_str(r#"{"type":"door","color":"red"}"#).expect("valid door");
        assert_eq!(
            door,
            EntityKind::Door {
                color: KeyColor::Red
            }
        );
        let typo = serde_json::from_str::<EntityKind>(r#"{"type":"door","colour":"red"}"#);
        assert!(typo.is_err(), "misspelled field must be rejected");
        assert_eq!(
            serde_json::to_string(&EntityKind::Wall).expect("serializable"),
            r#"{"type":"wall"}"#
        );
    }
}
