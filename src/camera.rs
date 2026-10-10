//! Scene cameras share the object command, proposal and animation paths.
use crate::engine::{Editor, EngineError, ObjRef, Scene};
use glam::{DQuat, DVec3, EulerRot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A perspective lens: vertical FOV in degrees, lens radius and focus in metres.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Lens {
    pub fov: f64,
    pub aperture: f64,
    pub focus: f64,
    /// Vertical orthographic view height in metres; absent means perspective.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ortho_height: Option<f64>,
}
impl Default for Lens {
    fn default() -> Self {
        Self {
            fov: 36.0,
            aperture: 0.0,
            focus: 10.0,
            ortho_height: None,
        }
    }
}
impl Lens {
    pub fn validate(&self) -> Result<(), EngineError> {
        if !self.fov.is_finite()
            || !(1.0..=170.0).contains(&self.fov)
            || !self.aperture.is_finite()
            || !(0.0..=1.0).contains(&self.aperture)
            || !self.focus.is_finite()
            || !(0.001..=1e4).contains(&self.focus)
            || self
                .ortho_height
                .is_some_and(|h| !h.is_finite() || !(0.001..=1e4).contains(&h))
        {
            return Err(EngineError::new(
                "camera lens needs fov 1-170 degrees, aperture 0-1 m and focus 0.001-10000 m and optional ortho_height 0.001-10000 m",
            ));
        }
        Ok(())
    }
}

pub(crate) fn validate_scene(scene: &Scene) -> Result<(), EngineError> {
    for o in &scene.objects {
        if let Some(lens) = &o.camera {
            lens.validate()?;
            if o.kind != "camera"
                || !o.mesh.vertices.is_empty()
                || !o.mesh.faces.is_empty()
                || !o.modifiers.is_empty()
                || !o.bones.is_empty()
                || o.group.is_some()
            {
                return Err(EngineError::new(
                    "camera objects cannot contain geometry, modifiers, bones or assembly membership",
                ));
            }
        } else if o.kind == "camera" {
            return Err(EngineError::new("camera object is missing its lens"));
        }
    }
    Ok(())
}

/// Resolve and sample a scene camera, preserving its roll and animated transform.
pub fn resolve(
    ed: &Editor,
    id: &ObjRef,
    frame: Option<f64>,
    width: usize,
    height: usize,
) -> Result<crate::pathtrace::Camera, EngineError> {
    let o = ed
        .scene()
        .objects
        .iter()
        .find(|o| match id {
            ObjRef::Id(id) => o.id == *id,
            ObjRef::Name(n) => o.name == *n,
        })
        .ok_or_else(|| EngineError::new("scene camera does not exist"))?;
    let lens =
        crate::anim::lens_at(o, frame).ok_or_else(|| EngineError::new("object is not a camera"))?;
    lens.validate()?;
    let t = frame.map_or_else(|| o.transform.clone(), |f| crate::anim::pose(o, f).0);
    let [x, y, z] = t.rotation;
    let q = DQuat::from_euler(EulerRot::XYZ, x, y, z);
    let eye = DVec3::from(t.translation);
    let mut c = crate::pathtrace::Camera::new(
        eye,
        eye + q * DVec3::NEG_Z * lens.focus,
        lens.fov,
        width,
        height,
    );
    c.up = q * DVec3::Y;
    c.aperture = lens.aperture;
    c.focus = lens.focus;
    c.ortho_height = lens.ortho_height;
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CommandBatch;
    use serde_json::{Value, json};
    fn apply(ed: &mut Editor, commands: Value) -> Result<crate::engine::ApplyResult, EngineError> {
        ed.apply(&serde_json::from_value::<CommandBatch>(json!({"commands":commands})).unwrap())
    }
    #[test]
    fn lenses_are_atomic_saved_and_undoable() {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([{"op":"add_camera","name":"Shot","translation":[1,2,3]}]),
        )
        .unwrap();
        let before = ed.scene().clone();
        assert!(apply(&mut ed,json!([{"op":"move","id":"Shot","offset":[1,0,0]},{"op":"camera_settings","id":"Shot","lens":{"fov":180}}])).is_err());
        assert_eq!(*ed.scene(), before);
        apply(&mut ed,json!([{"op":"camera_settings","id":"Shot","lens":{"fov":60,"aperture":0.05,"focus":3}}])).unwrap();
        assert_eq!(ed.scene().objects[0].camera.as_ref().unwrap().fov, 60.);
        ed.undo().unwrap();
        assert_eq!(ed.scene().objects[0].camera, Some(Lens::default()));
        let saved = serde_json::to_vec(ed.scene()).unwrap();
        let scene: Scene = serde_json::from_slice(&saved).unwrap();
        validate_scene(&scene).unwrap();
        assert_eq!(scene.objects[0].camera, Some(Lens::default()));
    }
    #[test]
    fn scene_cameras_preserve_roll_and_sample_motion() {
        let mut ed = Editor::new();
        apply(&mut ed,json!([{"op":"add_camera","name":"Roll","rotation":[0,0,std::f64::consts::FRAC_PI_2],"lens":{"focus":4}},
            {"op":"set_keyframe","id":"Roll","property":"translation","frame":1,"value":[0,0,0],"interpolation":"linear"},
            {"op":"set_keyframe","id":"Roll","property":"translation","frame":3,"value":[2,0,0],"interpolation":"linear"}])).unwrap();
        let c = resolve(&ed, &ObjRef::Name("Roll".into()), Some(2.), 16, 8).unwrap();
        assert!(c.eye.abs_diff_eq(DVec3::X, 1e-9));
        assert!(c.up.abs_diff_eq(DVec3::NEG_X, 1e-9));
        assert!(c.target.abs_diff_eq(DVec3::new(1., 0., -4.), 1e-9));
        assert_eq!((c.width, c.height), (16, 8));
    }
    #[test]
    fn camera_only_and_mixed_glb_files_round_trip_static_lenses() {
        for mixed in [false, true] {
            let mut ed = Editor::new();
            apply(&mut ed,json!([{"op":"add_camera","name":"Shot","translation":[1,2,3],"rotation":[0.2,0.3,0.4],"lens":{"fov":50,"aperture":0.1,"focus":6}}])).unwrap();
            if mixed {
                apply(&mut ed, json!([{"op":"add","primitive":{"kind":"cube"}}])).unwrap();
            }
            let commands = crate::gltf::import(&crate::gltf::export_glb(&ed)).unwrap();
            let mut back = Editor::new();
            apply(&mut back, serde_json::to_value(commands).unwrap()).unwrap();
            let c = resolve(&back, &ObjRef::Name("Shot".into()), None, 8, 8).unwrap();
            let original = resolve(&ed, &ObjRef::Name("Shot".into()), None, 8, 8).unwrap();
            assert!(c.eye.abs_diff_eq(original.eye, 1e-8));
            assert!(c.target.abs_diff_eq(original.target, 1e-8));
            assert!(c.up.abs_diff_eq(original.up, 1e-8));
            assert!((c.fov - original.fov).abs() < 1e-9);
            assert_eq!(c.aperture, original.aperture);
            assert_eq!(back.scene().objects.len(), if mixed { 2 } else { 1 });
        }
    }
    #[test]
    fn camera_geometry_and_invalid_loaded_lenses_are_rejected() {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([{"op":"add_camera"},{"op":"add","primitive":{"kind":"cube"}}]),
        )
        .unwrap();
        let before = ed.scene().clone();
        assert!(
            apply(
                &mut ed,
                json!([{"op":"add_modifier","id":1,"modifier":{"type":"subdivision","levels":1}}])
            )
            .is_err()
        );
        assert_eq!(*ed.scene(), before);
        assert!(resolve(&ed, &ObjRef::Id(2), None, 8, 8).is_err());
        assert!(resolve(&ed, &ObjRef::Id(99), None, 8, 8).is_err());
        let mut invalid = before;
        invalid.objects[0].camera.as_mut().unwrap().focus = f64::NAN;
        assert!(validate_scene(&invalid).is_err());
        assert_eq!(crate::inspect::scene_solids(ed.scene()).unwrap().len(), 1);
    }
    #[test]
    fn orthographic_height_samples_keys_and_legacy_cameras_stay_perspective() {
        let mut ed = Editor::new();
        ed.apply(&serde_json::from_value(serde_json::json!({"commands":[
            {"op":"add_camera","name":"Ortho","lens":{"ortho_height":4}},
            {"op":"set_keyframe","id":"Ortho","property":"camera_height","frame":1,"value":2,"interpolation":"linear"},
            {"op":"set_keyframe","id":"Ortho","property":"camera_height","frame":3,"value":6},
            {"op":"add_camera","name":"Legacy"}
        ]})).unwrap()).unwrap();
        assert_eq!(
            resolve(&ed, &ObjRef::Name("Ortho".into()), Some(2.), 16, 8)
                .unwrap()
                .ortho_height,
            Some(4.)
        );
        assert_eq!(
            resolve(&ed, &ObjRef::Name("Ortho".into()), None, 16, 8)
                .unwrap()
                .ortho_height,
            Some(4.)
        );
        assert_eq!(
            resolve(&ed, &ObjRef::Name("Legacy".into()), Some(2.), 16, 8)
                .unwrap()
                .ortho_height,
            None
        );
        let mut scene = ed.scene().clone();
        scene.objects[0].tracks[0].keys[0].value = vec![0.];
        assert!(ed.load(scene).is_err());
    }

    #[test]
    fn invalid_orthographic_lenses_reject_the_entire_batch_and_loaded_scene() {
        let mut ed = Editor::new();
        ed.apply(
            &serde_json::from_value(
                serde_json::json!({"commands":[{"op":"add_camera","name":"Shot"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        let before = ed.scene().clone();
        for h in [0., -1., 10001.] {
            assert!(ed.apply(&serde_json::from_value(serde_json::json!({"commands":[{"op":"move","id":"Shot","offset":[1,0,0]},{"op":"camera_settings","id":"Shot","lens":{"ortho_height":h}}]})).unwrap()).is_err());
            assert_eq!(*ed.scene(), before);
        }
        let mut bad = before;
        bad.objects[0].camera.as_mut().unwrap().ortho_height = Some(f64::NAN);
        assert!(ed.load(bad).is_err());
    }

    #[test]
    fn projection_settings_follow_proposal_review_and_history_revision() {
        use serde_json::json;
        let mut ed = Editor::new();
        let batch = |commands| serde_json::from_value(json!({"commands":commands})).unwrap();
        ed.apply(&batch(
            json!([{"op":"add_camera","name":"Shot","lens":{"ortho_height":4}}]),
        ))
        .unwrap();
        ed.apply(&batch(json!([{"op":"move","id":"Shot","offset":[0,0,1]}])))
            .unwrap();
        let id=ed.propose(&serde_json::from_value(json!({"title":"Tighter drawing","commands":[{"op":"camera_settings","id":"Shot","lens":{"ortho_height":2}}]})).unwrap()).unwrap()[0];
        assert_eq!(
            resolve(
                &ed.preview(id).unwrap(),
                &ObjRef::Name("Shot".into()),
                None,
                8,
                8
            )
            .unwrap()
            .ortho_height,
            Some(2.)
        );
        assert_eq!(
            ed.scene().objects[0].camera.as_ref().unwrap().ortho_height,
            Some(4.)
        );
        ed.accept(id).unwrap();
        ed.undo().unwrap();
        let commands = serde_json::from_value(
            json!([{"op":"add_camera","name":"Shot","lens":{"ortho_height":6}}]),
        )
        .unwrap();
        ed.revise(1, commands).unwrap();
        assert_eq!(
            resolve(&ed, &ObjRef::Name("Shot".into()), None, 8, 8)
                .unwrap()
                .ortho_height,
            Some(6.)
        );
        assert_eq!(ed.scene().objects[0].transform.translation[2], 1.);
        ed.undo().unwrap();
        assert_eq!(
            ed.scene().objects[0].camera.as_ref().unwrap().ortho_height,
            Some(4.)
        );
    }
}
