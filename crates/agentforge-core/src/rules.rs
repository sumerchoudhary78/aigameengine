//! Gameplay rule parameters. Rules are scene data: editing them never requires recompiling.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a player must achieve to win an episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WinCondition {
    /// Step onto any exit.
    ReachExit,
    /// Collect every key in the scene, then step onto an exit.
    CollectAllKeysThenExit,
}

impl WinCondition {
    /// Wire name of the condition.
    pub const fn as_str(self) -> &'static str {
        match self {
            WinCondition::ReachExit => "reach_exit",
            WinCondition::CollectAllKeysThenExit => "collect_all_keys_then_exit",
        }
    }
}

impl fmt::Display for WinCondition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Rule parameters of a scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    /// The episode fails with `max_ticks_reached` once this many ticks elapse without a win.
    /// A win on the final tick still counts. Range: `1..=10000`.
    pub max_ticks: u32,
    /// What the player must achieve to win.
    pub win_condition: WinCondition,
}

impl Rules {
    /// Largest accepted `max_ticks`.
    pub const MAX_TICKS_LIMIT: u32 = 10_000;

    /// Checks parameter ranges, naming the offending field on failure.
    pub fn check(&self) -> Result<(), (&'static str, String)> {
        if self.max_ticks == 0 || self.max_ticks > Self::MAX_TICKS_LIMIT {
            return Err((
                "max_ticks",
                format!(
                    "must be between 1 and {}, got {}",
                    Self::MAX_TICKS_LIMIT,
                    self.max_ticks
                ),
            ));
        }
        Ok(())
    }
}

impl Default for Rules {
    fn default() -> Self {
        Self {
            max_ticks: 100,
            win_condition: WinCondition::ReachExit,
        }
    }
}

/// Partial update of [`Rules`]; absent fields keep their current values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RulesPatch {
    /// New `max_ticks`, if changing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ticks: Option<u32>,
    /// New win condition, if changing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub win_condition: Option<WinCondition>,
}

impl RulesPatch {
    /// The rules that result from applying this patch to `rules`.
    pub fn apply_to(self, rules: Rules) -> Rules {
        Rules {
            max_ticks: self.max_ticks.unwrap_or(rules.max_ticks),
            win_condition: self.win_condition.unwrap_or(rules.win_condition),
        }
    }

    /// Whether the patch changes nothing.
    pub const fn is_empty(self) -> bool {
        self.max_ticks.is_none() && self.win_condition.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_ticks_range_is_enforced() {
        let mut rules = Rules::default();
        assert!(rules.check().is_ok());
        rules.max_ticks = 0;
        assert_eq!(rules.check().map_err(|(field, _)| field), Err("max_ticks"));
        rules.max_ticks = Rules::MAX_TICKS_LIMIT + 1;
        assert!(rules.check().is_err());
    }

    #[test]
    fn patch_only_touches_present_fields() {
        let rules = Rules::default();
        let patched = RulesPatch {
            max_ticks: Some(7),
            win_condition: None,
        }
        .apply_to(rules);
        assert_eq!(patched.max_ticks, 7);
        assert_eq!(patched.win_condition, rules.win_condition);
    }
}
