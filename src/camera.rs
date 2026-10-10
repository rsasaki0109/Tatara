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
}
impl Default for Lens {
    fn default() -> Self {
        Self {
            fov: 36.0,
            aperture: 0.0,
            focus: 10.0,
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
        {
            return Err(EngineError::new(
                "camera lens needs fov 1-170 degrees, aperture 0-1 m and focus 0.001-10000 m",
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
}
