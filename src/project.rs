//! Native projects retain the validated scene recipe rather than only its result.
use crate::engine::{Scene, Step};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Format {
    #[serde(rename = "tatara-project")]
    Tatara,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub format: Format,
    pub version: u32,
    /// Older steps may have been folded into this validated native scene.
    pub base: Scene,
    /// Applied batches, oldest first; these remain editable after reopening.
    pub steps: Vec<Step>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CommandBatch, Editor};
    use serde_json::{Value, json};

    fn apply(ed: &mut Editor, commands: Value) {
        let batch: CommandBatch = serde_json::from_value(
            json!({"commands":commands,"source":"UI","actor":{"id":"artist","name":"Artist"}}),
        )
        .unwrap();
        ed.apply(&batch).unwrap();
    }

    #[test]
    fn project_reopens_the_recipe_and_keeps_one_open_undo_step() {
        let mut source = Editor::new();
        apply(
            &mut source,
            json!([{"op":"add","name":"Box","primitive":{"kind":"cube"}}]),
        );
        apply(
            &mut source,
            json!([{"op":"extrude","id":"Box","face":4,"distance":0.5}]),
        );
        let bytes = serde_json::to_vec(&source.project()).unwrap();
        let project: Project = serde_json::from_slice(&bytes).unwrap();
        let mut opened = Editor::new();
        apply(
            &mut opened,
            json!([{"op":"add","name":"Previous","primitive":{"kind":"cube"}}]),
        );
        let previous = opened.scene().objects.clone();
        let previous_recipe = opened.project();
        opened.load_project(project.clone()).unwrap();
        assert_eq!(opened.scene().objects, source.scene().objects);
        assert_eq!(opened.project(), project);
        opened.undo().unwrap();
        assert_eq!(opened.scene().objects, previous);
        assert_eq!(opened.project(), previous_recipe);
        opened.redo().unwrap();
        assert_eq!(opened.scene().objects, source.scene().objects);
        opened
            .revise(
                1,
                serde_json::from_value(
                    json!([{"op":"add","name":"Box","primitive":{"kind":"cube","size":2}}]),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            opened.scene().objects[0].mesh.faces.len(),
            source.scene().objects[0].mesh.faces.len()
        );
        assert_ne!(
            opened.scene().objects[0].mesh.vertices,
            source.scene().objects[0].mesh.vertices
        );
        assert_eq!(opened.history().0[0].actor.as_ref().unwrap().name, "Artist");
    }

    #[test]
    fn invalid_project_replay_preserves_current_scene_and_recipe() {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([{"op":"add","name":"Box","primitive":{"kind":"cube"}}]),
        );
        let before = ed.scene().clone();
        let original = ed.project();
        let mut version = original.clone();
        version.version += 1;
        let mut empty = original.clone();
        empty.steps[0].commands.clear();
        let mut missing = original.clone();
        missing.steps[0].commands =
            serde_json::from_value(json!([{"op":"move","id":"Missing","offset":[1,0,0]}])).unwrap();
        let mut actor = original.clone();
        actor.steps[0].actor.as_mut().unwrap().id.clear();
        let mut too_long = original.clone();
        too_long.steps = vec![original.steps[0].clone(); crate::engine::MAX_STEPS + 1];
        for bad in [version, empty, missing, actor, too_long] {
            assert!(ed.load_project(bad).is_err());
            assert_eq!(*ed.scene(), before);
            assert_eq!(ed.project(), original);
            assert!(ed.can_undo());
            assert!(!ed.can_redo());
        }
    }

    #[test]
    fn projects_preserve_loaded_assets_joint_bounds_and_bezier_keys() {
        let mut source = Editor::new();
        apply(
            &mut source,
            json!([
                {"op":"add_image","name":"Tiny","data":"iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAIAAAD91JpzAAAAEklEQVR4nGP4z8DAAMIM/4EAAB/uBfsL2WiLAAAAAElFTkSuQmCC"},
                {"op":"add","name":"Arm","primitive":{"kind":"cylinder","height":2,"rings":4}},
                {"op":"rig","id":"Arm","chain":2},
                {"op":"set_bone_limits","id":"Arm","bone":"Bone 2","limits":{"min":[0,0,0],"max":[0,0,1]}},
                {"op":"set_keyframe","id":"Arm","property":"bone","bone":"Bone 2","frame":1,"value":[0,0,-1],"interpolation":"bezier"},
                {"op":"set_keyframe","id":"Arm","property":"bone","bone":"Bone 2","frame":11,"value":[0,0,2]},
                {"op":"set_key_handle","id":"Arm","property":"bone","bone":"Bone 2","frame":1,"side":"out","handle":{"time":[0.3,0.3,0.8],"value":[0,0,1]}}
            ]),
        );
        let mut loaded = Editor::new();
        loaded.load(source.scene().clone()).unwrap();
        apply(
            &mut loaded,
            json!([{"op":"move","id":"Arm","offset":[1,0,0]}]),
        );
        let bytes = serde_json::to_vec(&loaded.project()).unwrap();
        let project: Project = serde_json::from_slice(&bytes).unwrap();
        let mut opened = Editor::new();
        opened.load_project(project).unwrap();
        assert!(opened.scene().images.contains_key("Tiny"));
        let arm = &opened.scene().objects[0];
        assert_eq!(
            arm.bones[1].limits,
            source.scene().objects[0].bones[1].limits
        );
        assert_eq!(arm.tracks, source.scene().objects[0].tracks);
        assert_eq!(crate::rig::rotations(arm, Some(11.0))[1][2], 1.0);
        assert_eq!(arm.transform.translation[0], 1.0);
    }

    #[test]
    fn exhausted_project_ids_reject_every_creation_path_atomically() {
        let mut source = Editor::new();
        apply(
            &mut source,
            json!([{ "op":"add", "name":"Box", "primitive":{"kind":"cube"}}]),
        );
        let mut ed = Editor::new();
        let before = ed.scene().clone();
        let recipe = ed.project();
        for command in [
            json!({"op":"add","primitive":{"kind":"cube"}}),
            json!({"op":"add_camera"}),
            json!({"op":"add_light","lamp":{}}),
            json!({"op":"add_mesh","vertices":[[0,0,0],[1,0,0],[0,1,0]],"faces":[[0,1,2]]}),
            json!({"op":"build","template":"chair"}),
            json!({"op":"duplicate","id":"Box"}),
        ] {
            let mut project = source.project();
            project.base = source.scene().clone();
            project.base.next_id = u64::MAX;
            project.steps[0].commands = serde_json::from_value(json!([command])).unwrap();
            let error = ed.load_project(project).unwrap_err();
            assert!(error.message.contains("exhausted"), "{error}");
            assert_eq!(*ed.scene(), before);
            assert_eq!(ed.project(), recipe);
            assert!(!ed.can_undo());
            assert!(!ed.can_redo());
        }
        let mut project = source.project();
        project.base.next_id = u64::MAX - 1;
        project.steps[0]
            .commands
            .push(serde_json::from_value(json!({"op":"add_camera"})).unwrap());
        assert!(
            ed.load_project(project)
                .unwrap_err()
                .message
                .contains("exhausted")
        );
        assert_eq!(*ed.scene(), before);
        assert_eq!(ed.project(), recipe);
    }

    #[test]
    fn recovered_recipe_keeps_sampled_constraints_revisable() {
        let mut source = Editor::new();
        apply(
            &mut source,
            json!([{ "op":"add", "name":"Anchor", "primitive":{"kind":"cube"}}]),
        );
        apply(
            &mut source,
            json!([
                {"op":"add","name":"Follower","primitive":{"kind":"cube"},"translation":[0,3,0]},
                {"op":"constrain","id":"Follower","on":"Anchor"}
            ]),
        );
        apply(
            &mut source,
            json!([
                {"op":"set_keyframe","id":"Anchor","property":"translation","frame":1,"value":[0,0,0]},
                {"op":"set_keyframe","id":"Anchor","property":"translation","frame":11,"value":[0,2,0]}
            ]),
        );
        let project: Project =
            serde_json::from_slice(&serde_json::to_vec(&source.project()).unwrap()).unwrap();
        let mut recovered = Editor::new();
        recovered.load_project(project).unwrap();
        let before = recovered.scene().clone();
        assert_eq!(
            recovered.scene_at(6.0).unwrap().objects,
            source.scene_at(6.0).unwrap().objects
        );
        assert_eq!(*recovered.scene(), before);
        assert_eq!(
            recovered.scene_at(6.0).unwrap().objects[1]
                .transform
                .translation[1],
            2.0
        );
        recovered
            .revise(
                1,
                serde_json::from_value(
                    json!([{"op":"add","name":"Anchor","primitive":{"kind":"cube","size":2}}]),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            recovered.scene_at(6.0).unwrap().objects[1]
                .transform
                .translation[1],
            2.5
        );
        recovered.undo().unwrap();
        assert_eq!(
            recovered.scene_at(6.0).unwrap().objects[1]
                .transform
                .translation[1],
            2.0
        );
        recovered.undo().unwrap();
        assert!(recovered.scene().objects.is_empty());
        recovered.redo().unwrap();
        assert_eq!(
            recovered.scene_at(6.0).unwrap().objects[1]
                .transform
                .translation[1],
            2.0
        );
    }

    #[test]
    fn folded_history_base_reopens_and_remains_revisable() {
        let mut source = Editor::new();
        apply(
            &mut source,
            json!([{"op":"add","name":"Box","primitive":{"kind":"cube"}}]),
        );
        for _ in 0..crate::engine::MAX_STEPS {
            apply(
                &mut source,
                json!([{"op":"move","id":"Box","offset":[1,0,0]}]),
            );
        }
        let project = source.project();
        assert_eq!(project.steps.len(), crate::engine::MAX_STEPS);
        assert_eq!(project.base.objects.len(), 1);
        let mut opened = Editor::new();
        opened.load_project(project).unwrap();
        assert_eq!(opened.scene().objects, source.scene().objects);
        opened
            .revise(
                1,
                serde_json::from_value(json!([{"op":"move","id":"Box","offset":[2,0,0]}])).unwrap(),
            )
            .unwrap();
        assert_eq!(
            opened.scene().objects[0].transform.translation[0],
            crate::engine::MAX_STEPS as f64 + 1.0
        );
    }
}

/// The same typed request guards HTTP and MCP project replacement.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoadRequest {
    pub project: Project,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}
