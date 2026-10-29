//! Authoring handlers: projects, scenes, entities, analysis, snapshots, replay verification.

use std::path::PathBuf;

use agentforge_core::analysis::{self, ReachabilityReport, ValidationReport};
use agentforge_core::geometry::GridSize;
use agentforge_core::hash::scene_digest;
use agentforge_core::layout;
use agentforge_core::project::{Name, Project};
use agentforge_core::render;
use agentforge_core::replay;
use agentforge_core::scene::{EditEffect, MAX_BATCH_EDITS, MAX_LABEL_CHARS, Scene, SceneEdit};

use super::errors::{
    ApiError, analysis_error, batch_error, invalid_arguments, layout_error, name_error,
    replay_error, scene_error, single_edit_error, storage_error,
};
use super::{ActiveEpisode, Host, Snapshot, Workspace};
use crate::protocol::ErrorCode;
use crate::protocol::catalog::MAX_SNAPSHOTS;
use crate::protocol::ops::*;
use crate::storage;

impl Host {
    pub(super) fn project_create(
        &mut self,
        args: ProjectCreateArgs,
    ) -> Result<ProjectInfo, ApiError> {
        self.check_revision(args.expected_revision)?;
        let name = Name::new(&args.name).map_err(|e| name_error(e, "name"))?;
        let workspace = Workspace {
            project: Project::new(name),
            path: None,
            dirty: true,
        };
        let info = Self::project_info(&workspace);
        self.workspace = Some(workspace);
        self.revision += 1;
        Ok(info)
    }

    pub(super) fn project_load(&mut self, args: ProjectLoadArgs) -> Result<ProjectInfo, ApiError> {
        self.check_revision(args.expected_revision)?;
        let path = PathBuf::from(&args.path);
        let project = storage::load_project(&path).map_err(|e| storage_error(e, "project"))?;
        let workspace = Workspace {
            project,
            path: Some(path),
            dirty: false,
        };
        let info = Self::project_info(&workspace);
        self.workspace = Some(workspace);
        self.revision += 1;
        Ok(info)
    }

    pub(super) fn project_save(&mut self, args: ProjectSaveArgs) -> Result<ProjectSaved, ApiError> {
        if let Some(expected) = args.expected_revision {
            self.check_revision(expected)?;
        }
        let workspace = self.workspace()?;
        let path = match (args.path, &workspace.path) {
            (Some(path), _) => PathBuf::from(path),
            (None, Some(path)) => path.clone(),
            (None, None) => {
                return Err(invalid_arguments(
                    "this project has never been saved; pass `path`",
                ));
            }
        };
        let bytes = storage::save_project(&path, &workspace.project)
            .map_err(|e| storage_error(e, "project"))?;
        let scenes = workspace
            .project
            .scene_names()
            .map(Name::to_string)
            .collect();
        if let Some(workspace) = self.workspace.as_mut() {
            workspace.path = Some(path.clone());
            workspace.dirty = false;
        }
        Ok(ProjectSaved {
            path: path.display().to_string(),
            bytes,
            scenes,
            revision: self.revision,
        })
    }

    pub(super) fn scene_create(&mut self, args: SceneCreateArgs) -> Result<SceneView, ApiError> {
        self.check_revision(args.expected_revision)?;
        let name = Name::new(&args.name).map_err(|e| name_error(e, "name"))?;
        if self.workspace()?.project.scene(&name).is_some() {
            return Err(ApiError::new(
                ErrorCode::SceneAlreadyExists,
                format!("a scene named {:?} already exists", name.as_str()),
            )
            .with("scene", name.as_str())
            .hint("Pick another name, or edit the existing scene."));
        }
        let (size, edits) = match (args.layout, args.width, args.height) {
            (Some(rows), width, height) => {
                let parsed = layout::parse(&rows).map_err(layout_error)?;
                let mismatched = width.is_some_and(|w| w != parsed.size.width)
                    || height.is_some_and(|h| h != parsed.size.height);
                if mismatched {
                    return Err(invalid_arguments(format!(
                        "width/height do not match the {} layout; omit them or make them agree",
                        parsed.size
                    )));
                }
                let edits = parsed
                    .cells
                    .into_iter()
                    .map(|(position, kind)| SceneEdit::CreateEntity {
                        kind,
                        position,
                        label: None,
                    })
                    .collect();
                (parsed.size, edits)
            }
            (None, Some(width), Some(height)) => (GridSize::new(width, height), Vec::new()),
            (None, _, _) => {
                return Err(invalid_arguments(
                    "provide `layout`, or both `width` and `height`",
                ));
            }
        };
        let empty = Scene::new(size, args.rules.unwrap_or_default()).map_err(scene_error)?;
        let (scene, _) = empty.apply_edits(&edits).map_err(|error| {
            let position = match edits.get(error.index) {
                Some(SceneEdit::CreateEntity { position, .. }) => Some(*position),
                _ => None,
            };
            scene_error(error.error).with("layout_position", position)
        })?;
        let view = scene_view(&name, &scene);
        self.commit_scene(name, scene);
        Ok(view)
    }

    pub(super) fn scene_get(&self, args: SceneRef) -> Result<SceneView, ApiError> {
        let (name, scene) = self.scene(&args.scene)?;
        Ok(scene_view(&name, scene))
    }

    pub(super) fn scene_validate(&self, args: SceneRef) -> Result<ValidationReport, ApiError> {
        let (_, scene) = self.scene(&args.scene)?;
        Ok(analysis::validate(scene))
    }

    pub(super) fn scene_apply_batch(
        &mut self,
        args: ApplyBatchArgs,
    ) -> Result<BatchApplied, ApiError> {
        if args.edits.is_empty() {
            return Err(invalid_arguments("`edits` must contain at least one edit"));
        }
        if args.edits.len() > MAX_BATCH_EDITS {
            return Err(ApiError::new(
                ErrorCode::LimitExceeded,
                format!(
                    "a batch holds at most {MAX_BATCH_EDITS} edits, got {}",
                    args.edits.len()
                ),
            ));
        }
        let effects = self
            .edit_scene(&args.scene, args.expected_revision, &args.edits)
            .map_err(|e| e.map_batch(batch_error))?;
        Ok(BatchApplied {
            applied: u32::try_from(effects.len()).unwrap_or(u32::MAX),
            effects,
        })
    }

    pub(super) fn scene_update_rules(
        &mut self,
        args: UpdateRulesArgs,
    ) -> Result<RulesUpdated, ApiError> {
        let edit = SceneEdit::UpdateRules {
            max_ticks: args.rules.max_ticks,
            win_condition: args.rules.win_condition,
        };
        match self.single_edit(&args.scene, args.expected_revision, edit)? {
            EditEffect::RulesUpdated { before, after } => Ok(RulesUpdated { before, after }),
            other => Err(unexpected_effect(&other)),
        }
    }

    pub(super) fn scene_render(&self, args: RenderArgs) -> Result<Rendered, ApiError> {
        let (name, scene) = self.scene(&args.scene)?;
        let content = match args.format {
            RenderFormat::Svg => render::scene_svg(scene, name.as_str()),
            RenderFormat::Ascii => {
                let mut text = scene_ascii(scene).join("\n");
                text.push('\n');
                text
            }
        };
        let bytes = content.len() as u64;
        match args.path {
            Some(path) => {
                let path = PathBuf::from(path);
                storage::write_atomic(&path, content.as_bytes())
                    .map_err(|e| storage_error(e, "render"))?;
                Ok(Rendered {
                    format: args.format,
                    bytes,
                    path: Some(path.display().to_string()),
                    content: None,
                })
            }
            None => Ok(Rendered {
                format: args.format,
                bytes,
                path: None,
                content: Some(content),
            }),
        }
    }

    pub(super) fn entity_create(&mut self, args: EntityCreateArgs) -> Result<EntityView, ApiError> {
        let edit = SceneEdit::CreateEntity {
            kind: args.kind,
            position: args.position,
            label: args.label,
        };
        match self.single_edit(&args.scene, args.expected_revision, edit)? {
            EditEffect::Created { entity } => Ok(EntityView { entity }),
            other => Err(unexpected_effect(&other)),
        }
    }

    pub(super) fn entity_get(&self, args: EntityRef) -> Result<EntityView, ApiError> {
        let (_, scene) = self.scene(&args.scene)?;
        let entity = scene.entity(args.id).cloned().ok_or_else(|| {
            scene_error(agentforge_core::scene::SceneError::EntityNotFound { id: args.id })
        })?;
        Ok(EntityView { entity })
    }

    pub(super) fn entity_update(
        &mut self,
        args: EntityUpdateArgs,
    ) -> Result<EntityUpdated, ApiError> {
        let edit = SceneEdit::UpdateEntity {
            id: args.id,
            kind: args.kind,
            position: args.position,
            label: args.label,
        };
        match self.single_edit(&args.scene, args.expected_revision, edit)? {
            EditEffect::Updated { before, after } => Ok(EntityUpdated { before, after }),
            other => Err(unexpected_effect(&other)),
        }
    }

    pub(super) fn entity_remove(
        &mut self,
        args: EntityRemoveArgs,
    ) -> Result<EntityRemoved, ApiError> {
        let edit = SceneEdit::RemoveEntity { id: args.id };
        match self.single_edit(&args.scene, args.expected_revision, edit)? {
            EditEffect::Removed { entity } => Ok(EntityRemoved { removed: entity }),
            other => Err(unexpected_effect(&other)),
        }
    }

    pub(super) fn analysis_reachability(
        &self,
        args: ReachabilityArgs,
    ) -> Result<ReachabilityReport, ApiError> {
        let (_, scene) = self.scene(&args.scene)?;
        analysis::reachability(scene, args.from, args.to).map_err(analysis_error)
    }

    pub(super) fn snapshot_capture(
        &mut self,
        args: SnapshotCaptureArgs,
    ) -> Result<SnapshotInfo, ApiError> {
        if self.snapshots.len() >= MAX_SNAPSHOTS {
            return Err(ApiError::new(
                ErrorCode::LimitExceeded,
                format!("a session holds at most {MAX_SNAPSHOTS} snapshots"),
            ));
        }
        let label = match args.label.as_deref() {
            None | Some("") => None,
            Some(text)
                if text.chars().count() > MAX_LABEL_CHARS || text.chars().any(char::is_control) =>
            {
                return Err(ApiError::new(
                    ErrorCode::InvalidLabel,
                    format!(
                        "snapshot labels are 1-{MAX_LABEL_CHARS} characters without control characters"
                    ),
                ));
            }
            Some(text) => Some(text.to_owned()),
        };
        let snapshot = Snapshot {
            id: self.next_snapshot_id,
            label,
            captured_at_revision: self.revision,
            project: self.workspace.as_ref().map(|w| w.project.clone()),
            project_path: self.workspace.as_ref().and_then(|w| w.path.clone()),
            episode: self.episode.clone(),
        };
        let info = Self::snapshot_info(&snapshot);
        self.snapshots.insert(snapshot.id, snapshot);
        self.next_snapshot_id += 1;
        Ok(info)
    }

    pub(super) fn snapshot_restore(
        &mut self,
        args: SnapshotRestoreArgs,
    ) -> Result<SnapshotRestored, ApiError> {
        self.check_revision(args.expected_revision)?;
        let snapshot = self.snapshots.get(&args.snapshot_id).ok_or_else(|| {
            ApiError::new(
                ErrorCode::SnapshotNotFound,
                format!("no snapshot with id {} in this session", args.snapshot_id),
            )
            .with("snapshot_id", args.snapshot_id)
            .with("available", self.snapshots.keys().collect::<Vec<_>>())
        })?;
        let info = Self::snapshot_info(snapshot);
        let workspace = snapshot.project.clone().map(|project| Workspace {
            project,
            path: snapshot.project_path.clone(),
            dirty: true,
        });
        // The restored episode continues a new timeline, so it gets a fresh id: a client
        // still holding the old id gets `stale_episode` instead of acting on it by accident.
        let episode = snapshot.episode.clone().map(|captured| ActiveEpisode {
            id: self.next_episode_id,
            ..captured
        });
        let active_episode = episode.as_ref().map(super::episode_brief);
        if episode.is_some() {
            self.next_episode_id += 1;
        }
        self.workspace = workspace;
        self.episode = episode;
        // The revision moves forward, never back to captured_at_revision, so writes based
        // on any pre-restore revision remain stale.
        self.revision += 1;
        Ok(SnapshotRestored {
            snapshot: info,
            revision: self.revision,
            active_episode,
        })
    }

    pub(super) fn replay_verify(&self, args: ReplayVerifyArgs) -> Result<ReplayVerified, ApiError> {
        let replay = match (args.path, args.replay) {
            (Some(path), None) => storage::load_replay(&PathBuf::from(path))
                .map_err(|e| storage_error(e, "replay"))?,
            (None, Some(replay)) => replay,
            _ => {
                return Err(invalid_arguments(
                    "provide exactly one of `path` and `replay`",
                ));
            }
        };
        let verification = replay::verify(&replay).map_err(replay_error)?;
        let scene_matches = match args.scene {
            Some(scene) => {
                let (_, scene) = self.scene(&scene)?;
                Some(scene_digest(scene) == replay.scene_digest)
            }
            None => None,
        };
        Ok(ReplayVerified {
            verification,
            scene_matches,
        })
    }

    // -- helpers -------------------------------------------------------------------------

    /// Applies a batch to a scene under revision control and commits it.
    fn edit_scene(
        &mut self,
        scene: &str,
        expected_revision: u64,
        edits: &[SceneEdit],
    ) -> Result<Vec<EditEffect>, EditFailure> {
        self.check_revision(expected_revision)
            .map_err(EditFailure::Api)?;
        let (name, current) = self.scene(scene).map_err(EditFailure::Api)?;
        let (updated, effects) = current.apply_edits(edits).map_err(EditFailure::Batch)?;
        self.commit_scene(name, updated);
        Ok(effects)
    }

    fn single_edit(
        &mut self,
        scene: &str,
        expected_revision: u64,
        edit: SceneEdit,
    ) -> Result<EditEffect, ApiError> {
        let mut effects = self
            .edit_scene(scene, expected_revision, std::slice::from_ref(&edit))
            .map_err(|e| e.map_batch(single_edit_error))?;
        effects
            .pop()
            .ok_or_else(|| ApiError::new(ErrorCode::InternalError, "edit produced no effect"))
    }

    /// Stores a new version of a scene and advances the revision. Infallible.
    fn commit_scene(&mut self, name: Name, scene: Scene) {
        if let Some(workspace) = self.workspace.as_mut() {
            workspace.project.set_scene(name, scene);
            workspace.dirty = true;
            self.revision += 1;
        }
    }
}

/// Failure of [`Host::edit_scene`], before or during the batch.
enum EditFailure {
    Api(ApiError),
    Batch(agentforge_core::scene::BatchError),
}

impl EditFailure {
    fn map_batch(self, map: fn(agentforge_core::scene::BatchError) -> ApiError) -> ApiError {
        match self {
            EditFailure::Api(error) => error,
            EditFailure::Batch(error) => map(error),
        }
    }
}

fn unexpected_effect(effect: &EditEffect) -> ApiError {
    ApiError::new(
        ErrorCode::InternalError,
        format!("unexpected edit effect {effect:?}"),
    )
}

fn scene_ascii(scene: &Scene) -> Vec<String> {
    layout::render(
        scene.size(),
        scene.entities().map(|e| (e.position, e.kind.glyph())),
    )
}

fn scene_view(name: &Name, scene: &Scene) -> SceneView {
    let document = scene.to_document();
    SceneView {
        scene: name.to_string(),
        size: document.size,
        rules: document.rules,
        next_entity_id: document.next_entity_id,
        entities: document.entities,
        ascii: scene_ascii(scene),
        digest: scene_digest(scene),
    }
}
