//! Stable identifiers and their allocator.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Stable identifier of an entity within one scene.
///
/// Identifiers are handed out in strictly increasing order by the scene's [`IdAllocator`]
/// and are never reused, even after the entity is removed. An ID quoted in a diagnostic,
/// event or replay therefore denotes exactly one entity in that scene's history.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct EntityId(u32);

impl EntityId {
    /// Wraps a raw identifier value.
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw identifier value.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Monotonic [`EntityId`] allocator.
///
/// Its state is simulation state: it is saved with the scene, captured by snapshots and
/// covered by the canonical scene digest, so a restored scene allocates the same IDs as
/// the original would have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdAllocator {
    next: u32,
}

impl IdAllocator {
    /// The first identifier value handed out by a fresh allocator.
    pub const FIRST: u32 = 1;

    /// Creates an allocator whose next identifier is [`IdAllocator::FIRST`].
    pub const fn new() -> Self {
        Self { next: Self::FIRST }
    }

    /// Creates an allocator that will hand out `next` first.
    pub const fn starting_at(next: u32) -> Self {
        Self { next }
    }

    /// The raw value of the identifier the next allocation will return.
    pub const fn next_raw(self) -> u32 {
        self.next
    }

    /// Allocates the next identifier, or `None` once the identifier space is exhausted.
    pub fn allocate(&mut self) -> Option<EntityId> {
        let id = EntityId(self.next);
        self.next = self.next.checked_add(1)?;
        Some(id)
    }
}

impl Default for IdAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_is_monotonic_and_starts_at_one() {
        let mut ids = IdAllocator::new();
        assert_eq!(ids.allocate(), Some(EntityId::new(1)));
        assert_eq!(ids.allocate(), Some(EntityId::new(2)));
        assert_eq!(ids.next_raw(), 3);
    }

    #[test]
    fn exhausted_allocator_refuses_instead_of_wrapping() {
        let mut ids = IdAllocator::starting_at(u32::MAX);
        assert_eq!(ids.allocate(), None);
        assert_eq!(ids.next_raw(), u32::MAX);
    }
}
