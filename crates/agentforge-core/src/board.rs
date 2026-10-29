//! Dense cell index over a scene, used by episodes and analysis for O(1) lookups.

use crate::entity::EntityKind;
use crate::geometry::{GridSize, Position};
use crate::ids::EntityId;
use crate::scene::Scene;

/// The entity occupying a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occupant {
    /// The occupying entity.
    pub id: EntityId,
    /// Its kind.
    pub kind: EntityKind,
}

/// Immutable row-major view of which entity occupies each cell of a scene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    size: GridSize,
    cells: Vec<Option<Occupant>>,
}

impl Board {
    /// Indexes every entity of `scene`. Relies on the scene invariant of at most one entity
    /// per cell, all inside the grid.
    pub fn from_scene(scene: &Scene) -> Self {
        let size = scene.size();
        let mut cells = vec![None; size.cell_count()];
        for entity in scene.entities() {
            if let Some(index) = size.index(entity.position) {
                cells[index] = Some(Occupant {
                    id: entity.id,
                    kind: entity.kind,
                });
            }
        }
        Self { size, cells }
    }

    /// Grid dimensions.
    pub const fn size(&self) -> GridSize {
        self.size
    }

    /// The occupant of `position`, or `None` for empty or outside cells.
    pub fn at(&self, position: Position) -> Option<Occupant> {
        self.size
            .index(position)
            .and_then(|index| self.cells[index])
    }

    /// The occupant of the cell with row-major `index`.
    pub fn at_index(&self, index: usize) -> Option<Occupant> {
        self.cells.get(index).copied().flatten()
    }
}
