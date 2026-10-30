//! Dense cell index over a scene, used by episodes and analysis for O(1) lookups.

use std::collections::BTreeMap;

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
    portal_partner: Vec<Option<Position>>,
}

impl Board {
    /// Indexes every entity of `scene`. Relies on the scene invariant of at most one entity
    /// per cell, all inside the grid. Portal pairs are indexed when a pair id has exactly two
    /// members; an unpaired or over-paired id has no partner (`None`).
    pub fn from_scene(scene: &Scene) -> Self {
        let size = scene.size();
        let mut cells = vec![None; size.cell_count()];
        let mut portals: BTreeMap<u8, Vec<Position>> = BTreeMap::new();
        for entity in scene.entities() {
            if let Some(index) = size.index(entity.position) {
                cells[index] = Some(Occupant {
                    id: entity.id,
                    kind: entity.kind,
                });
            }
            if let EntityKind::Portal { pair } = entity.kind {
                portals.entry(pair).or_default().push(entity.position);
            }
        }
        let mut portal_partner = vec![None; size.cell_count()];
        for positions in portals.values() {
            if let [a, b] = positions.as_slice()
                && let (Some(a_index), Some(b_index)) = (size.index(*a), size.index(*b))
            {
                portal_partner[a_index] = Some(*b);
                portal_partner[b_index] = Some(*a);
            }
        }
        Self {
            size,
            cells,
            portal_partner,
        }
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

    /// The other portal with the same pair id, or `None` if `position` is not a paired
    /// portal.
    pub fn portal_partner(&self, position: Position) -> Option<Position> {
        self.size
            .index(position)
            .and_then(|index| self.portal_partner[index])
    }
}
