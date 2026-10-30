//! Integer grid geometry.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A cell coordinate. `x` grows east, `y` grows south, `(0, 0)` is the north-west corner.
///
/// Coordinates are signed so that out-of-range requests deserialize and can be rejected
/// with a precise `out_of_bounds` error instead of an opaque type error.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Position {
    /// Column, growing east.
    pub x: i32,
    /// Row, growing south.
    pub y: i32,
}

impl Position {
    /// Creates a position.
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// The neighbouring cell in `direction`. Saturates instead of overflowing; a saturated
    /// result is outside every supported grid, so bounds checks still reject it.
    pub const fn step(self, direction: Direction) -> Self {
        let (dx, dy) = direction.delta();
        Self {
            x: self.x.saturating_add(dx),
            y: self.y.saturating_add(dy),
        }
    }

    /// Manhattan distance between two cells.
    pub const fn manhattan(self, other: Self) -> u32 {
        self.x.abs_diff(other.x) + self.y.abs_diff(other.y)
    }
}

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

/// One of the four movement directions.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Towards smaller `y`.
    North,
    /// Towards larger `x`.
    East,
    /// Towards larger `y`.
    South,
    /// Towards smaller `x`.
    West,
}

impl Direction {
    /// All directions in the engine's canonical order. Legal-action lists and every search
    /// expand neighbours in this order, which makes reported paths deterministic.
    pub const ALL: [Direction; 4] = [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
    ];

    /// The `(dx, dy)` offset of one step in this direction.
    pub const fn delta(self) -> (i32, i32) {
        match self {
            Direction::North => (0, -1),
            Direction::East => (1, 0),
            Direction::South => (0, 1),
            Direction::West => (-1, 0),
        }
    }

    /// Wire name of the direction.
    pub const fn as_str(self) -> &'static str {
        match self {
            Direction::North => "north",
            Direction::East => "east",
            Direction::South => "south",
            Direction::West => "west",
        }
    }

    /// Position of this direction in [`Direction::ALL`].
    pub const fn index(self) -> u8 {
        match self {
            Direction::North => 0,
            Direction::East => 1,
            Direction::South => 2,
            Direction::West => 3,
        }
    }

    /// The direction pointing the opposite way.
    pub const fn opposite(self) -> Self {
        match self {
            Direction::North => Direction::South,
            Direction::East => Direction::West,
            Direction::South => Direction::North,
            Direction::West => Direction::East,
        }
    }
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Grid dimensions in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GridSize {
    /// Number of columns.
    pub width: u32,
    /// Number of rows.
    pub height: u32,
}

impl GridSize {
    /// Largest supported width and height.
    pub const MAX_DIMENSION: u32 = 64;

    /// Creates a size without validating it; see [`GridSize::is_supported`].
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Whether both dimensions lie in `1..=MAX_DIMENSION`.
    pub const fn is_supported(self) -> bool {
        self.width >= 1
            && self.height >= 1
            && self.width <= Self::MAX_DIMENSION
            && self.height <= Self::MAX_DIMENSION
    }

    /// Whether `position` lies inside the grid.
    pub const fn contains(self, position: Position) -> bool {
        position.x >= 0
            && position.y >= 0
            && (position.x as u32) < self.width
            && (position.y as u32) < self.height
    }

    /// Number of cells.
    pub const fn cell_count(self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Row-major index of `position`, or `None` when it lies outside the grid.
    pub const fn index(self, position: Position) -> Option<usize> {
        if self.contains(position) {
            Some(position.y as usize * self.width as usize + position.x as usize)
        } else {
            None
        }
    }

    /// Inverse of [`GridSize::index`]. `index` must be below [`GridSize::cell_count`].
    pub const fn position(self, index: usize) -> Position {
        let width = self.width as usize;
        Position::new((index % width) as i32, (index / width) as i32)
    }
}

impl fmt::Display for GridSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_round_trips_and_rejects_outside_cells() {
        let size = GridSize::new(5, 3);
        for index in 0..size.cell_count() {
            assert_eq!(size.index(size.position(index)), Some(index));
        }
        assert_eq!(size.index(Position::new(5, 0)), None);
        assert_eq!(size.index(Position::new(0, 3)), None);
        assert_eq!(size.index(Position::new(-1, 0)), None);
    }

    #[test]
    fn stepping_saturates_at_the_integer_limit() {
        let edge = Position::new(i32::MAX, 0);
        assert_eq!(edge.step(Direction::East), edge);
        assert!(!GridSize::new(64, 64).contains(edge.step(Direction::East)));
    }

    #[test]
    fn supported_sizes() {
        assert!(GridSize::new(1, 1).is_supported());
        assert!(GridSize::new(64, 64).is_supported());
        assert!(!GridSize::new(0, 4).is_supported());
        assert!(!GridSize::new(65, 4).is_supported());
    }

    #[test]
    fn direction_indices_and_opposites_are_consistent() {
        for (index, direction) in Direction::ALL.into_iter().enumerate() {
            assert_eq!(usize::from(direction.index()), index);
            assert_eq!(direction.opposite().opposite(), direction);
            assert_eq!(direction.opposite().index(), (direction.index() + 2) % 4);
        }
    }
}
