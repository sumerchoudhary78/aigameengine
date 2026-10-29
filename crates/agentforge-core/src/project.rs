//! Projects: named collections of scenes, and the validated names they use.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::scene::{Scene, SceneDocument, SceneError};

/// Value of [`ProjectDocument::format`].
pub const PROJECT_FORMAT: &str = "agentforge.project";

/// Supported [`ProjectDocument::format_version`].
pub const PROJECT_FORMAT_VERSION: u32 = 1;

/// Longest accepted project or scene name.
pub const MAX_NAME_CHARS: usize = 64;

/// A project or scene name: 1-64 characters of `a-z`, `0-9`, `_` or `-`, starting with a
/// letter or digit. Names appear in file names, so the alphabet is deliberately small.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(String);

/// A string that is not a valid [`Name`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid name {name:?}: use 1-64 characters of a-z, 0-9, '_' or '-', starting with a letter or digit"
)]
pub struct NameError {
    /// The rejected text.
    pub name: String,
}

impl Name {
    /// Validates `text` as a name.
    pub fn new(text: &str) -> Result<Self, NameError> {
        let valid_char =
            |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-';
        let starts_well = text
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if starts_well && text.len() <= MAX_NAME_CHARS && text.chars().all(valid_char) {
            Ok(Self(text.to_owned()))
        } else {
            Err(NameError {
                name: text.to_owned(),
            })
        }
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Name {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A named collection of scenes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    name: Name,
    scenes: BTreeMap<Name, Scene>,
}

/// Serialized form of a [`Project`] (the project file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectDocument {
    /// Always `"agentforge.project"`.
    pub format: String,
    /// Project file format version.
    pub format_version: u32,
    /// Engine version that wrote the file (informational).
    #[serde(default)]
    pub engine_version: String,
    /// Project name.
    pub name: String,
    /// Scenes by name.
    pub scenes: BTreeMap<String, SceneDocument>,
}

/// Why a project document was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectError {
    /// A project or scene name is invalid.
    #[error(transparent)]
    InvalidName(#[from] NameError),
    /// Wrong `format` string.
    #[error("not an AgentForge project: format is {found:?}, expected {PROJECT_FORMAT:?}")]
    UnsupportedFormat {
        /// The format found.
        found: String,
    },
    /// Unsupported `format_version`.
    #[error(
        "project format_version {found} is not supported (supported: {PROJECT_FORMAT_VERSION})"
    )]
    UnsupportedFormatVersion {
        /// The version found.
        found: u32,
    },
    /// A scene violates scene invariants.
    #[error("scene {scene:?} is invalid: {error}")]
    InvalidScene {
        /// Scene name.
        scene: String,
        /// What is wrong.
        error: SceneError,
    },
}

impl Project {
    /// Creates an empty project.
    pub fn new(name: Name) -> Self {
        Self {
            name,
            scenes: BTreeMap::new(),
        }
    }

    /// Project name.
    pub fn name(&self) -> &Name {
        &self.name
    }

    /// The scene called `name`.
    pub fn scene(&self, name: &Name) -> Option<&Scene> {
        self.scenes.get(name)
    }

    /// Scene names in ascending order.
    pub fn scene_names(&self) -> impl Iterator<Item = &Name> {
        self.scenes.keys()
    }

    /// Inserts or replaces the scene called `name`.
    pub fn set_scene(&mut self, name: Name, scene: Scene) {
        self.scenes.insert(name, scene);
    }

    /// The serializable form of this project.
    pub fn to_document(&self) -> ProjectDocument {
        ProjectDocument {
            format: PROJECT_FORMAT.to_owned(),
            format_version: PROJECT_FORMAT_VERSION,
            engine_version: crate::ENGINE_VERSION.to_owned(),
            name: self.name.to_string(),
            scenes: self
                .scenes
                .iter()
                .map(|(name, scene)| (name.to_string(), scene.to_document()))
                .collect(),
        }
    }

    /// Rebuilds a project, validating format, names and every scene.
    pub fn from_document(document: ProjectDocument) -> Result<Self, ProjectError> {
        if document.format != PROJECT_FORMAT {
            return Err(ProjectError::UnsupportedFormat {
                found: document.format,
            });
        }
        if document.format_version != PROJECT_FORMAT_VERSION {
            return Err(ProjectError::UnsupportedFormatVersion {
                found: document.format_version,
            });
        }
        let mut project = Project::new(Name::new(&document.name)?);
        for (scene_name, scene) in document.scenes {
            let name = Name::new(&scene_name)?;
            let scene =
                Scene::from_document(scene).map_err(|error| ProjectError::InvalidScene {
                    scene: scene_name,
                    error,
                })?;
            project.scenes.insert(name, scene);
        }
        Ok(project)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::GridSize;
    use crate::rules::Rules;

    #[test]
    fn names_are_restricted_to_a_file_safe_alphabet() {
        assert!(Name::new("blocked_dungeon-2").is_ok());
        for bad in [
            "",
            "Upper",
            "_lead",
            "has space",
            "../escape",
            &"a".repeat(65),
        ] {
            assert!(Name::new(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn documents_round_trip_and_reject_foreign_formats() {
        let mut project = Project::new(Name::new("demo").expect("valid"));
        let scene = Scene::new(GridSize::new(3, 3), Rules::default()).expect("valid");
        project.set_scene(Name::new("level-1").expect("valid"), scene);
        let document = project.to_document();
        assert_eq!(Project::from_document(document.clone()), Ok(project));

        let mut foreign = document.clone();
        foreign.format = "other".to_owned();
        assert!(matches!(
            Project::from_document(foreign),
            Err(ProjectError::UnsupportedFormat { .. })
        ));
        let mut future = document;
        future.format_version = 2;
        assert!(matches!(
            Project::from_document(future),
            Err(ProjectError::UnsupportedFormatVersion { found: 2 })
        ));
    }
}
