//! Canonical state hashing.
//!
//! Hashes are SHA-256 digests of a hand-specified byte encoding, never of debug strings,
//! serializer output or unordered iteration. The encoding (version
//! [`STATE_HASH_VERSION`]) is documented in `docs/ARCHITECTURE.md` under "Canonical
//! hashing" and pinned by golden tests; changing it requires bumping the version, which
//! makes older replays explicitly incompatible instead of silently different.
//!
//! Primitive encodings: integers are fixed-width little-endian; a string is its UTF-8
//! byte length as `u32` followed by the bytes. Entity labels and all session metadata
//! (revisions, episode/snapshot ids, file paths) are excluded: they never influence
//! simulation outcomes.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

use crate::entity::EntityKind;
use crate::episode::{Action, EpisodeState, FailureReason, Outcome};
use crate::geometry::Direction;
use crate::rules::WinCondition;
use crate::scene::Scene;

/// Version of the canonical encoding. Recorded in replays; mismatches are rejected.
pub const STATE_HASH_VERSION: u32 = 1;

const SCENE_DOMAIN: &str = "agentforge.scene.v1";
const STATE_DOMAIN: &str = "agentforge.state.v1";
const RECORDING_DOMAIN: &str = "agentforge.recording.v1";

/// A SHA-256 digest of a canonical encoding, written as `sha256:<64 lowercase hex>`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StateHash([u8; 32]);

impl StateHash {
    /// Prefix of the textual form.
    pub const PREFIX: &'static str = "sha256:";

    /// Wraps raw digest bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex of the digest, without the prefix.
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        out
    }
}

impl fmt::Display for StateHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", Self::PREFIX, self.to_hex())
    }
}

impl fmt::Debug for StateHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A string that is not a valid `sha256:<hex>` hash.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("expected \"sha256:\" followed by 64 lowercase hex characters")]
pub struct HashParseError;

impl FromStr for StateHash {
    type Err = HashParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let hex = text.strip_prefix(Self::PREFIX).ok_or(HashParseError)?;
        if hex.len() != 64 {
            return Err(HashParseError);
        }
        let mut bytes = [0u8; 32];
        let (pairs, _) = hex.as_bytes().as_chunks::<2>();
        for (byte, pair) in bytes.iter_mut().zip(pairs) {
            let high = hex_value(pair[0]).ok_or(HashParseError)?;
            let low = hex_value(pair[1]).ok_or(HashParseError)?;
            *byte = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

impl Serialize for StateHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for StateHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for StateHash {
    fn schema_name() -> Cow<'static, str> {
        "StateHash".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^sha256:[0-9a-f]{64}$",
            "description": "SHA-256 of the canonical state encoding (see docs/ARCHITECTURE.md)."
        })
    }
}

/// Streams canonical primitives into SHA-256: fixed-width little-endian integers, IEEE-754
/// bit patterns for floats, and length-prefixed UTF-8 strings. Shared by every world kind so
/// all state hashes follow the same primitive rules.
pub struct CanonicalEncoder(Sha256);

impl CanonicalEncoder {
    /// Starts an encoding with a domain-separation string.
    pub fn new(domain: &str) -> Self {
        let mut encoder = Self(Sha256::new());
        encoder.str(domain);
        encoder
    }

    /// Appends one byte.
    pub fn u8(&mut self, value: u8) {
        self.0.update([value]);
    }

    /// Appends a little-endian `u32`.
    pub fn u32(&mut self, value: u32) {
        self.0.update(value.to_le_bytes());
    }

    /// Appends a little-endian `i32`.
    pub fn i32(&mut self, value: i32) {
        self.0.update(value.to_le_bytes());
    }

    /// Appends the IEEE-754 bit pattern of `value` as a little-endian `u64`.
    pub fn f64(&mut self, value: f64) {
        self.0.update(value.to_bits().to_le_bytes());
    }

    /// Appends a `u32` byte length followed by the UTF-8 bytes.
    pub fn str(&mut self, value: &str) {
        // Strings hashed here are short domain constants, far below u32::MAX bytes.
        self.u32(value.len() as u32);
        self.0.update(value.as_bytes());
    }

    /// Appends raw bytes (for nested digests).
    pub fn bytes(&mut self, value: &[u8]) {
        self.0.update(value);
    }

    /// Completes the digest.
    pub fn finish(self) -> StateHash {
        StateHash(self.0.finalize().into())
    }
}

/// Code of a win condition in the canonical encoding.
const fn win_condition_code(win: WinCondition) -> u8 {
    match win {
        WinCondition::ReachExit => 1,
        WinCondition::CollectAllKeysThenExit => 2,
    }
}

/// Code of an entity kind in the canonical encoding.
const fn kind_code(kind: EntityKind) -> u8 {
    match kind {
        EntityKind::Player => 1,
        EntityKind::Wall => 2,
        EntityKind::Key { .. } => 3,
        EntityKind::Door { .. } => 4,
        EntityKind::Exit => 5,
        EntityKind::Ice => 6,
        EntityKind::OneWay { .. } => 7,
        EntityKind::Hazard => 8,
        EntityKind::Portal { .. } => 9,
    }
}

/// Kind-specific attribute: color, one-way direction or portal pair.
///
/// For keys and doors this is `1 + color index`, exactly the old color byte, so existing
/// scene digests stay unchanged.
const fn attribute_code(kind: EntityKind) -> u8 {
    match kind {
        EntityKind::Key { color } | EntityKind::Door { color } => color.index() + 1,
        EntityKind::OneWay { direction } => direction.index() + 1,
        EntityKind::Portal { pair } => pair + 1,
        _ => 0,
    }
}

/// Code of an outcome in the canonical encoding, with its tick (0 while in progress).
const fn outcome_code(outcome: Outcome) -> (u8, u32) {
    match outcome {
        Outcome::InProgress => (0, 0),
        Outcome::Won { tick } => (1, tick),
        Outcome::Failed {
            tick,
            reason: FailureReason::MaxTicksReached,
        } => (2, tick),
        Outcome::Failed {
            tick,
            reason: FailureReason::Forfeited,
        } => (3, tick),
        Outcome::Failed {
            tick,
            reason: FailureReason::Hazard,
        } => (4, tick),
    }
}

/// Digest of a scene's simulation-relevant content (labels excluded).
///
/// Layout: `str("agentforge.scene.v1")`, `u32 width`, `u32 height`, `u32 max_ticks`,
/// `u8 win_condition`, `u32 next_entity_id`, `u32 entity_count`, then per entity in
/// ascending id order: `u32 id`, `u8 kind` (1 player, 2 wall, 3 key, 4 door, 5 exit,
/// 6 ice, 7 one-way, 8 hazard, 9 portal), `u8 attribute` (0 none; key/door 1 + color index;
/// one-way 1 + direction index; portal 1 + pair), `i32 x`, `i32 y`.
pub fn scene_digest(scene: &Scene) -> StateHash {
    let mut encoder = CanonicalEncoder::new(SCENE_DOMAIN);
    let size = scene.size();
    let rules = scene.rules();
    encoder.u32(size.width);
    encoder.u32(size.height);
    encoder.u32(rules.max_ticks);
    encoder.u8(win_condition_code(rules.win_condition));
    encoder.u32(scene.next_entity_id());
    // Entity count is bounded by the cell count (at most 64 * 64).
    encoder.u32(scene.entity_count() as u32);
    for entity in scene.entities() {
        encoder.u32(entity.id.get());
        encoder.u8(kind_code(entity.kind));
        encoder.u8(attribute_code(entity.kind));
        encoder.i32(entity.position.x);
        encoder.i32(entity.position.y);
    }
    encoder.finish()
}

/// Hash of the full simulation state of an episode at one tick.
///
/// Layout: `str("agentforge.state.v1")`, the 32 raw bytes of the scene digest, `u32 tick`,
/// `i32 player_x`, `i32 player_y`, `u8 inventory` (bit i = `KeyColor::ALL[i]`),
/// `u8 outcome` (0 in progress, 1 won, 2 failed: max ticks, 3 failed: forfeited),
/// `u32 outcome_tick` (0 while in progress).
///
/// Because each key color appears at most once per scene, the inventory also determines
/// which key entities have been collected, so no separate collected-set is encoded.
pub fn state_hash(scene_digest: &StateHash, state: &EpisodeState) -> StateHash {
    let mut encoder = CanonicalEncoder::new(STATE_DOMAIN);
    encoder.bytes(scene_digest.as_bytes());
    encoder.u32(state.tick);
    encoder.i32(state.player.x);
    encoder.i32(state.player.y);
    encoder.u8(state.inventory.bits());
    let (code, tick) = outcome_code(state.outcome);
    encoder.u8(code);
    encoder.u32(tick);
    encoder.finish()
}

/// Identifies a recording: two replays have the same fingerprint exactly when they record
/// the same actions, with the same per-tick hashes, from the same scene.
///
/// Layout: `str("agentforge.recording.v1")`, the scene digest and initial hash (32 raw bytes
/// each), `u32 step_count`, then per step `u32 tick`, `u8 action` (1-4 move
/// north/east/south/west, 5 wait, 6 forfeit) and the 32 hash bytes, then the final hash.
pub fn recording_fingerprint(
    scene_digest: &StateHash,
    initial_hash: &StateHash,
    steps: &[(u32, Action, StateHash)],
    final_hash: &StateHash,
) -> StateHash {
    let mut encoder = CanonicalEncoder::new(RECORDING_DOMAIN);
    encoder.bytes(scene_digest.as_bytes());
    encoder.bytes(initial_hash.as_bytes());
    // Steps are bounded by max_ticks (at most 10_000).
    encoder.u32(steps.len() as u32);
    for (tick, action, hash) in steps {
        encoder.u32(*tick);
        encoder.u8(match action {
            Action::Move {
                direction: Direction::North,
            } => 1,
            Action::Move {
                direction: Direction::East,
            } => 2,
            Action::Move {
                direction: Direction::South,
            } => 3,
            Action::Move {
                direction: Direction::West,
            } => 4,
            Action::Wait => 5,
            Action::Forfeit => 6,
        });
        encoder.bytes(hash.as_bytes());
    }
    encoder.bytes(final_hash.as_bytes());
    encoder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{KeyColor, KeySet};
    use crate::geometry::{GridSize, Position};
    use crate::rules::Rules;
    use crate::scene::SceneEdit;

    fn tiny_scene() -> Scene {
        let edits = [
            SceneEdit::CreateEntity {
                kind: EntityKind::Player,
                position: Position::new(0, 0),
                label: Some("labels are not hashed".to_owned()),
            },
            SceneEdit::CreateEntity {
                kind: EntityKind::Key {
                    color: KeyColor::Blue,
                },
                position: Position::new(1, 0),
                label: None,
            },
            SceneEdit::CreateEntity {
                kind: EntityKind::Exit,
                position: Position::new(2, 0),
                label: None,
            },
        ];
        let rules = Rules {
            max_ticks: 9,
            win_condition: WinCondition::CollectAllKeysThenExit,
        };
        Scene::new(GridSize::new(3, 1), rules)
            .and_then(|scene| scene.apply_edits(&edits).map_err(|e| e.error))
            .map(|(scene, _)| scene)
            .expect("valid scene")
    }

    /// Golden values pin the documented encoding. They were cross-checked against an
    /// independent Python implementation of the layout described above; if this test fails,
    /// the encoding changed and STATE_HASH_VERSION must be bumped.
    #[test]
    fn golden_hashes_pin_the_encoding() {
        let scene = tiny_scene();
        let digest = scene_digest(&scene);
        assert_eq!(
            digest.to_string(),
            "sha256:5a76b358dcaab685c8476bd19d592b0448b90ea549b03a71f8a596556415ffdb",
            "scene digest changed"
        );
        let state = EpisodeState {
            tick: 2,
            player: Position::new(2, 0),
            inventory: [KeyColor::Blue].into_iter().collect::<KeySet>(),
            outcome: Outcome::Won { tick: 2 },
        };
        assert_eq!(
            state_hash(&digest, &state).to_string(),
            "sha256:0f318b596859364eb14c014f849b6de038a46144b7452c3fce4c5b945b8e1c3e",
            "state hash changed"
        );
    }

    #[test]
    fn labels_do_not_affect_the_digest_but_positions_do() {
        let scene = tiny_scene();
        let relabeled = scene
            .apply_edits(&[SceneEdit::UpdateEntity {
                id: crate::ids::EntityId::new(1),
                kind: None,
                position: None,
                label: Some("different".to_owned()),
            }])
            .map(|(scene, _)| scene)
            .expect("valid");
        assert_eq!(scene_digest(&scene), scene_digest(&relabeled));

        let moved = scene
            .apply_edits(&[SceneEdit::UpdateEntity {
                id: crate::ids::EntityId::new(3),
                kind: Some(EntityKind::Wall),
                position: None,
                label: None,
            }])
            .map(|(scene, _)| scene)
            .expect("valid");
        assert_ne!(scene_digest(&scene), scene_digest(&moved));
    }

    #[test]
    fn textual_form_round_trips_and_rejects_garbage() {
        let digest = scene_digest(&tiny_scene());
        let parsed: StateHash = digest.to_string().parse().expect("round trip");
        assert_eq!(parsed, digest);
        assert!("sha256:xyz".parse::<StateHash>().is_err());
        assert!(
            digest.to_hex().parse::<StateHash>().is_err(),
            "prefix required"
        );
        let upper = digest
            .to_string()
            .to_uppercase()
            .replace("SHA256:", "sha256:");
        assert!(upper.parse::<StateHash>().is_err(), "lowercase only");
    }

    fn scene_with_terrain(kind: Option<EntityKind>) -> Scene {
        let mut edits = vec![
            SceneEdit::CreateEntity {
                kind: EntityKind::Player,
                position: Position::new(0, 0),
                label: None,
            },
            SceneEdit::CreateEntity {
                kind: EntityKind::Exit,
                position: Position::new(3, 0),
                label: None,
            },
        ];
        if let Some(kind) = kind {
            edits.push(SceneEdit::CreateEntity {
                kind,
                position: Position::new(1, 0),
                label: None,
            });
        }
        Scene::new(GridSize::new(4, 1), Rules::default())
            .and_then(|scene| scene.apply_edits(&edits).map_err(|e| e.error))
            .map(|(scene, _)| scene)
            .expect("valid scene")
    }

    #[test]
    fn terrain_kinds_and_attributes_change_the_digest() {
        let none = scene_digest(&scene_with_terrain(None));
        let ice = scene_digest(&scene_with_terrain(Some(EntityKind::Ice)));
        let hazard = scene_digest(&scene_with_terrain(Some(EntityKind::Hazard)));
        let north = scene_digest(&scene_with_terrain(Some(EntityKind::OneWay {
            direction: crate::geometry::Direction::North,
        })));
        let east = scene_digest(&scene_with_terrain(Some(EntityKind::OneWay {
            direction: crate::geometry::Direction::East,
        })));
        let portal_zero = scene_digest(&scene_with_terrain(Some(EntityKind::Portal { pair: 0 })));
        let portal_one = scene_digest(&scene_with_terrain(Some(EntityKind::Portal { pair: 1 })));

        for (name, digest) in [
            ("ice", ice),
            ("hazard", hazard),
            ("one-way north", north),
            ("portal 0", portal_zero),
        ] {
            assert_ne!(none, digest, "{name} must change the scene digest");
        }
        assert_ne!(ice, hazard, "kind codes must differ");
        assert_ne!(north, east, "one-way direction must be hashed");
        assert_ne!(portal_zero, portal_one, "portal pair must be hashed");
    }
}
