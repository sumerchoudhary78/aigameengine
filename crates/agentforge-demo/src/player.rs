//! The scripted player client.
//!
//! It reads only its observation, plans a route with its own breadth-first search over
//! (cell, keys held), and submits each move through `episode.act` with `expected_tick`.
//! If it finds no route it walks to the reachable cell closest to an exit and forfeits,
//! leaving the verdict to the engine.

use std::collections::{BTreeMap, VecDeque};

use serde_json::{Value, json};

use crate::client::{ClientError, EngineSession};

const DIRECTIONS: [(&str, i64, i64); 4] = [
    ("north", 0, -1),
    ("east", 1, 0),
    ("south", 0, 1),
    ("west", -1, 0),
];

/// What happened in one playtest.
#[derive(Debug, Clone)]
pub struct PlayReport {
    /// Episode id assigned by the engine.
    pub episode_id: u64,
    /// Whether the player's planner found a winning route.
    pub planned: bool,
    /// Actions submitted, in order.
    pub actions: Vec<String>,
    /// Final outcome reported by the engine.
    pub outcome: Value,
    /// Final tick reported by the engine.
    pub final_tick: u64,
    /// The engine's `replay.export` result.
    pub replay: Value,
}

impl PlayReport {
    /// The engine-reported outcome status (`won`, `failed`, `in_progress`).
    pub fn status(&self) -> &str {
        self.outcome["status"].as_str().unwrap_or("unknown")
    }
}

/// Plays one episode of the session's bound scene and exports its replay.
pub fn play(session: &mut EngineSession) -> Result<PlayReport, ClientError> {
    let started = session.call("episode.start", json!({}))?;
    let episode_id = started["episode_id"].as_u64().unwrap_or_default();
    let mut observation = started["observation"].clone();
    let world = World::from_observation(&observation).map_err(ClientError::BadResponse)?;
    let (route, planned) = match world.plan() {
        Some(route) => (route, true),
        None => (world.approach_exit(), false),
    };
    let mut actions = Vec::new();
    for direction in route {
        if is_finished(&observation) {
            break;
        }
        let tick = observation["tick"].as_u64().unwrap_or_default();
        let result = session.call(
            "episode.act",
            json!({"action": {"type": "move", "direction": direction}, "expected_tick": tick}),
        )?;
        observation = result["observation"].clone();
        actions.push(format!("move {direction}"));
    }
    if !is_finished(&observation) {
        let tick = observation["tick"].as_u64().unwrap_or_default();
        let result = session.call(
            "episode.act",
            json!({"action": {"type": "forfeit"}, "expected_tick": tick}),
        )?;
        observation = result["observation"].clone();
        actions.push("forfeit".to_owned());
    }
    let replay = session.call("replay.export", json!({}))?;
    Ok(PlayReport {
        episode_id,
        planned,
        actions,
        outcome: observation["outcome"].clone(),
        final_tick: observation["tick"].as_u64().unwrap_or_default(),
        replay,
    })
}

fn is_finished(observation: &Value) -> bool {
    observation["outcome"]["status"].as_str() != Some("in_progress")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Cell {
    Wall,
    Door(String),
    Key(String),
    Exit,
}

/// The player's model of the board, built from one observation.
struct World {
    width: i64,
    height: i64,
    player: (i64, i64),
    cells: BTreeMap<(i64, i64), Cell>,
    colors: Vec<String>,
    held_mask: u32,
    required_mask: u32,
}

type State = (i64, i64, u32);

/// Breadth-first parent links: how each state was first reached.
type Parents = BTreeMap<State, Option<(State, &'static str)>>;

impl World {
    fn from_observation(observation: &Value) -> Result<Self, String> {
        let int = |value: &Value, what: &str| {
            value
                .as_i64()
                .ok_or_else(|| format!("observation lacks {what}"))
        };
        let position = |value: &Value| -> Result<(i64, i64), String> {
            Ok((int(&value["x"], "x")?, int(&value["y"], "y")?))
        };
        let mut cells = BTreeMap::new();
        let mut colors: Vec<String> = Vec::new();
        let strings = |value: &Value| -> Vec<String> {
            value
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let held = strings(&observation["inventory"]);
        let required = strings(&observation["objective"]["keys_required"]);
        for entity in observation["entities"]
            .as_array()
            .ok_or("observation lacks entities")?
        {
            let kind = &entity["kind"];
            let color = kind["color"].as_str().map(str::to_owned);
            let cell = match (kind["type"].as_str(), color) {
                (Some("wall"), _) => Cell::Wall,
                (Some("exit"), _) => Cell::Exit,
                (Some("door"), Some(color)) => Cell::Door(color),
                (Some("key"), Some(color)) => {
                    colors.push(color.clone());
                    Cell::Key(color)
                }
                _ => continue,
            };
            cells.insert(position(&entity["position"])?, cell);
        }
        colors.extend(held.iter().cloned());
        colors.extend(required.iter().cloned());
        colors.sort();
        colors.dedup();
        let mask = |names: &[String]| {
            names
                .iter()
                .filter_map(|name| colors.iter().position(|c| c == name))
                .fold(0u32, |mask, bit| mask | (1 << bit))
        };
        Ok(Self {
            width: int(&observation["grid"]["width"], "grid width")?,
            height: int(&observation["grid"]["height"], "grid height")?,
            player: position(&observation["player"])?,
            held_mask: mask(&held),
            required_mask: mask(&required),
            colors,
            cells,
        })
    }

    fn bit(&self, color: &str) -> Option<u32> {
        self.colors
            .iter()
            .position(|c| c == color)
            .and_then(|index| u32::try_from(index).ok())
    }

    /// Successor state after moving from `state` by `(dx, dy)`, if the rules allow it.
    fn step(&self, state: State, dx: i64, dy: i64) -> Option<State> {
        let (x, y, mask) = (state.0 + dx, state.1 + dy, state.2);
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return None;
        }
        match self.cells.get(&(x, y)) {
            Some(Cell::Wall) => None,
            Some(Cell::Door(color)) => {
                let bit = self.bit(color)?;
                (mask & (1 << bit) != 0).then_some((x, y, mask))
            }
            Some(Cell::Key(color)) => {
                Some((x, y, mask | self.bit(color).map_or(0, |bit| 1 << bit)))
            }
            Some(Cell::Exit) | None => Some((x, y, mask)),
        }
    }

    /// Breadth-first search; returns the parent map and visit order.
    fn explore(&self) -> (Parents, Vec<State>) {
        let start = (self.player.0, self.player.1, self.held_mask);
        let mut parents = BTreeMap::from([(start, None)]);
        let mut order = Vec::new();
        let mut queue = VecDeque::from([start]);
        while let Some(state) = queue.pop_front() {
            order.push(state);
            for (name, dx, dy) in DIRECTIONS {
                if let Some(next) = self.step(state, dx, dy)
                    && !parents.contains_key(&next)
                {
                    parents.insert(next, Some((state, name)));
                    queue.push_back(next);
                }
            }
        }
        (parents, order)
    }

    fn route_to(parents: &Parents, goal: State) -> Vec<&'static str> {
        let mut route = Vec::new();
        let mut cursor = goal;
        while let Some(Some((previous, direction))) = parents.get(&cursor) {
            route.push(*direction);
            cursor = *previous;
        }
        route.reverse();
        route
    }

    fn is_goal(&self, state: State) -> bool {
        self.cells.get(&(state.0, state.1)) == Some(&Cell::Exit)
            && state.2 & self.required_mask == self.required_mask
    }

    /// Shortest winning route, if one exists.
    fn plan(&self) -> Option<Vec<&'static str>> {
        let (parents, order) = self.explore();
        let goal = order.into_iter().find(|state| self.is_goal(*state))?;
        Some(Self::route_to(&parents, goal))
    }

    /// Route to the reachable cell closest (Manhattan) to any exit.
    fn approach_exit(&self) -> Vec<&'static str> {
        let exits: Vec<(i64, i64)> = self
            .cells
            .iter()
            .filter(|(_, cell)| **cell == Cell::Exit)
            .map(|(position, _)| *position)
            .collect();
        let distance = |state: &State| {
            exits
                .iter()
                .map(|(x, y)| (state.0 - x).abs() + (state.1 - y).abs())
                .min()
                .unwrap_or(i64::MAX)
        };
        let (parents, order) = self.explore();
        // `min_by_key` keeps the first minimum, i.e. the earliest-discovered closest cell.
        match order.iter().min_by_key(|state| distance(state)) {
            Some(best) => Self::route_to(&parents, *best),
            None => Vec::new(),
        }
    }
}
