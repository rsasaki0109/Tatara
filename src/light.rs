//! Editable analytic lights, sampled consistently by the shared path tracer.
use crate::engine::{Editor, EngineError, Scene};
use glam::{DQuat, DVec3, EulerRot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Point,
    Sun,
}

/// Point intensity is candela; sun intensity is lux. Scale does not affect power.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Lamp {
    pub kind: Kind,
    pub color: String,
    pub intensity: f64,
}
impl Default for Lamp {
    fn default() -> Self {
        Self {
            kind: Kind::Point,
            color: "#ffffff".into(),
            intensity: 10.,
        }
    }
}
impl Lamp {
    pub fn validate(&self) -> Result<(), EngineError> {
        crate::engine::check_color(&self.color)?;
        if !self.intensity.is_finite() || !(0.0..=100_000.0).contains(&self.intensity) {
            return Err(EngineError::new(
                "light intensity must be finite and between 0 and 100000",
            ));
        }
        Ok(())
    }
}
pub(crate) fn validate_scene(scene: &Scene) -> Result<(), EngineError> {
    let mut count = 0;
    for o in &scene.objects {
        if let Some(lamp) = &o.light {
            count += 1;
            lamp.validate()?;
            if o.kind != "light"
                || o.camera.is_some()
                || !o.mesh.vertices.is_empty()
                || !o.mesh.faces.is_empty()
                || !o.modifiers.is_empty()
                || !o.bones.is_empty()
                || o.group.is_some()
            {
                return Err(EngineError::new(
                    "light objects cannot contain geometry, cameras, modifiers, bones or assembly membership",
                ));
            }
        } else if o.kind == "light" {
            return Err(EngineError::new("light object is missing its lamp"));
        }
    }
    if count > 16 {
        return Err(EngineError::new(
            "a scene supports at most 16 analytic lights",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct Emitter {
    kind: Kind,
    position: DVec3,
    toward: DVec3,
    power: DVec3,
}
impl Emitter {
    /// Incident irradiance, direction toward the emitter and shadow-ray distance.
    pub(crate) fn sample(&self, p: DVec3) -> Option<(DVec3, DVec3, f64)> {
        match self.kind {
            Kind::Sun => Some((self.toward, self.power, f64::INFINITY)),
            Kind::Point => {
                let d = self.position - p;
                let dist = d.length();
                (dist > 1e-6).then(|| (d / dist, self.power / (dist * dist), dist * (1. - 1e-4)))
            }
        }
    }
}
pub(crate) fn emitters(ed: &Editor, frame: Option<f64>) -> Vec<Emitter> {
    ed.scene()
        .objects
        .iter()
        .filter_map(|o| {
            let lamp = o.light.as_ref()?;
            if lamp.intensity == 0. {
                return None;
            }
            let t = frame.map_or_else(|| o.transform.clone(), |f| crate::anim::pose(o, f).0);
            let [x, y, z] = t.rotation;
            let q = DQuat::from_euler(EulerRot::XYZ, x, y, z);
            Some(Emitter {
                kind: lamp.kind,
                position: t.translation.into(),
                toward: q * DVec3::Z,
                power: crate::render::hex(&lamp.color) * lamp.intensity,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CommandBatch, ObjRef};
    use serde_json::{Value, json};
    fn apply(ed: &mut Editor, commands: Value) -> Result<crate::engine::ApplyResult, EngineError> {
        ed.apply(&serde_json::from_value::<CommandBatch>(json!({"commands":commands})).unwrap())
    }
    #[test]
    fn lamp_edits_are_atomic_undoable_and_non_geometry() {
        let mut ed = Editor::new();
        apply(&mut ed, json!([{"op":"add_light","name":"Key"}])).unwrap();
        let before = ed.scene().clone();
        assert!(apply(&mut ed,json!([{"op":"move","id":"Key","offset":[1,0,0]},{"op":"light_settings","id":"Key","lamp":{"intensity":-1}}])).is_err());
        assert_eq!(*ed.scene(), before);
        apply(&mut ed,json!([{"op":"light_settings","id":"Key","lamp":{"kind":"sun","color":"#ff0000","intensity":5}}])).unwrap();
        assert_eq!(
            ed.scene().objects[0].light.as_ref().unwrap().kind,
            Kind::Sun
        );
        ed.undo().unwrap();
        assert_eq!(ed.scene().objects[0].light, Some(Lamp::default()));
        assert!(crate::inspect::scene_solids(ed.scene()).unwrap().is_empty());
        assert!(apply(&mut ed,json!([{"op":"add_modifier","id":"Key","modifier":{"type":"subdivision","levels":1}}])).is_err());
        assert!(crate::camera::resolve(&ed, &ObjRef::Id(1), None, 8, 8).is_err());
        let mut invalid = ed.scene().clone();
        invalid.objects[0].camera = Some(crate::camera::Lens::default());
        assert!(validate_scene(&invalid).is_err());
    }
    #[test]
    fn light_count_and_loaded_power_limits_cannot_be_bypassed() {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            Value::Array((0..16).map(|_| json!({"op":"add_light"})).collect()),
        )
        .unwrap();
        let before = ed.scene().clone();
        assert!(apply(&mut ed, json!([{"op":"add_light"}])).is_err());
        assert_eq!(*ed.scene(), before);
        let mut scene = before;
        scene.objects[0].light.as_mut().unwrap().intensity = f64::INFINITY;
        assert!(validate_scene(&scene).is_err());
        assert!(
            Lamp {
                color: "red".into(),
                ..Lamp::default()
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn point_attenuation_and_animated_positions_match_photometric_units() {
        let mut ed = Editor::new();
        apply(&mut ed,json!([{"op":"add_light","name":"Key","translation":[0,2,0],"lamp":{"intensity":16}},
    {"op":"set_keyframe","id":"Key","property":"translation","frame":1,"value":[0,2,0],"interpolation":"linear"},
    {"op":"set_keyframe","id":"Key","property":"translation","frame":3,"value":[0,4,0],"interpolation":"linear"}])).unwrap();
        let a = emitters(&ed, Some(1.))[0].sample(DVec3::ZERO).unwrap();
        let b = emitters(&ed, Some(3.))[0].sample(DVec3::ZERO).unwrap();
        assert_eq!(a.0, DVec3::Y);
        assert!(a.1.abs_diff_eq(DVec3::splat(4.), 1e-9));
        assert!(b.1.abs_diff_eq(DVec3::ONE, 1e-9));
        assert!(
            emitters(&ed, Some(2.))[0]
                .position
                .abs_diff_eq(DVec3::new(0., 3., 0.), 1e-9)
        );
    }
    #[test]
    fn sun_direction_uses_rotation_but_not_translation_or_scale() {
        let mut ed = Editor::new();
        apply(&mut ed,json!([{"op":"add_light","name":"Sun","rotation":[-std::f64::consts::FRAC_PI_2,0,0],"lamp":{"kind":"sun","intensity":2}}])).unwrap();
        let a = emitters(&ed, None)[0].sample(DVec3::ZERO).unwrap();
        apply(
            &mut ed,
            json!([{"op":"transform","id":"Sun","translation":[20,30,40],"scale":[3,3,3]}]),
        )
        .unwrap();
        let b = emitters(&ed, None)[0]
            .sample(DVec3::new(7., 8., 9.))
            .unwrap();
        assert!(a.0.abs_diff_eq(DVec3::Y, 1e-9));
        assert_eq!(a, b);
        assert_eq!(a.1, DVec3::splat(2.));
    }
    #[test]
    fn editable_red_light_lights_the_mesh_and_respects_occlusion() {
        let mut ed = Editor::new();
        apply(&mut ed,json!([{"op":"world","strength":0},{"op":"add","name":"Subject","primitive":{"kind":"cube"},"color":"#ffffff"},
   {"op":"add_light","name":"Red key","translation":[2,2,3],"lamp":{"color":"#ff0000","intensity":30}}])).unwrap();
        let camera =
            crate::pathtrace::Camera::new(DVec3::new(0., 0., 4.), DVec3::ZERO, 36., 24, 24);
        let image = crate::pathtrace::traced_uncached(&ed, None)
            .unwrap()
            .still(&camera, 16, true);
        let index = (12 * 24 + 12) * 4;
        assert!(image[index] > 40 && image[index] > image[index + 1] + 30);
        apply(&mut ed,json!([{"op":"add","name":"Occluder","primitive":{"kind":"cube"},"translation":[1,1,1.75],"scale":[1,1,1],"color":"#000000"}])).unwrap();
        let shadow = crate::pathtrace::traced_uncached(&ed, None)
            .unwrap()
            .still(&camera, 16, true);
        assert!(
            shadow[index] < image[index] / 2,
            "{} vs {}",
            shadow[index],
            image[index]
        );
    }
    #[test]
    fn punctual_glb_round_trip_preserves_power_color_kind_and_pose() {
        let mut ed = Editor::new();
        apply(&mut ed,json!([{"op":"add_light","name":"Point","translation":[1,2,3],"lamp":{"color":"#769c8b","intensity":23}},
   {"op":"add_light","name":"Sun","rotation":[0.2,0.3,0.4],"lamp":{"kind":"sun","color":"#ffffff","intensity":4}}])).unwrap();
        let commands = crate::gltf::import(&crate::gltf::export_glb(&ed)).unwrap();
        let mut back = Editor::new();
        apply(&mut back, serde_json::to_value(commands).unwrap()).unwrap();
        for (a, b) in ed.scene().objects.iter().zip(&back.scene().objects) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.light, b.light);
            assert_eq!(a.transform.translation, b.transform.translation);
            let d = emitters(&ed, None);
            let e = emitters(&back, None);
            assert!(d[1].toward.abs_diff_eq(e[1].toward, 1e-9));
        }
    }
}
