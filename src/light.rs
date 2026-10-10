//! Editable analytic lights, sampled consistently by the shared path tracer.
#[cfg(test)]
use crate::engine::Editor;
use crate::engine::{EngineError, Scene};
use glam::{DQuat, DVec3, EulerRot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Point,
    Sun,
    Spot,
}

/// Point/spot intensity is candela; sun intensity is lux. Scale does not affect power.
/// Spot half-angles are radians; rays point along local -Z. Range is in metres.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Lamp {
    pub kind: Kind,
    pub color: String,
    pub intensity: f64,
    pub inner_cone: f64,
    pub outer_cone: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<f64>,
}
impl Default for Lamp {
    fn default() -> Self {
        Self {
            kind: Kind::Point,
            color: "#ffffff".into(),
            intensity: 10.,
            inner_cone: 0.,
            outer_cone: std::f64::consts::FRAC_PI_4,
            range: None,
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
        if !self.inner_cone.is_finite()
            || !self.outer_cone.is_finite()
            || self.inner_cone < 0.
            || self.inner_cone >= self.outer_cone
            || self.outer_cone > std::f64::consts::FRAC_PI_2
        {
            return Err(EngineError::new(
                "light cones must satisfy 0 <= inner_cone < outer_cone <= pi/2",
            ));
        }
        if self
            .range
            .is_some_and(|r| !r.is_finite() || !(0.001..=10000.).contains(&r))
        {
            return Err(EngineError::new(
                "light range must be finite and between 0.001 and 10000",
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
    inner_cone: f64,
    outer_cone: f64,
    range: Option<f64>,
}
impl Emitter {
    /// Incident irradiance, direction toward the emitter and shadow-ray distance.
    pub(crate) fn sample(&self, p: DVec3) -> Option<(DVec3, DVec3, f64)> {
        match self.kind {
            Kind::Sun => Some((self.toward, self.power, f64::INFINITY)),
            Kind::Point | Kind::Spot => {
                let d = self.position - p;
                let dist = d.length();
                if dist <= 1e-6 {
                    return None;
                }
                let direction = d / dist;
                let angular = if self.kind == Kind::Spot {
                    let outer = self.outer_cone.cos();
                    ((direction.dot(self.toward) - outer)
                        / (self.inner_cone.cos() - outer).max(0.001))
                    .clamp(0., 1.)
                    .powi(2)
                } else {
                    1.
                };
                let edge = self
                    .range
                    .map_or(1., |r| (1. - (dist / r).powi(4)).max(0.).powi(2));
                (angular * edge > 0.).then(|| {
                    (
                        direction,
                        self.power * (angular * edge / (dist * dist)),
                        dist * (1. - 1e-4),
                    )
                })
            }
        }
    }
}
#[cfg(test)]
pub(crate) fn emitters(ed: &Editor, frame: Option<f64>) -> Vec<Emitter> {
    scene_emitters(ed.scene(), frame)
}

pub(crate) fn scene_emitters(scene: &Scene, frame: Option<f64>) -> Vec<Emitter> {
    scene
        .objects
        .iter()
        .filter_map(|o| {
            let lamp = crate::anim::lamp_at(o, frame)?;
            if lamp.intensity == 0. {
                return None;
            }
            let t = frame.map_or_else(|| o.transform.clone(), |f| crate::anim::pose(o, f).0);
            let [x, y, z] = t.rotation;
            let q = DQuat::from_euler(EulerRot::XYZ, x, y, z);
            Some(Emitter {
                kind: lamp.kind,
                inner_cone: lamp.inner_cone,
                outer_cone: lamp.outer_cone,
                range: lamp.range,
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
    #[test]
    fn animated_light_power_and_colour_drive_native_irradiance() {
        let mut ed = Editor::new();
        apply(&mut ed,json!([
            {"op":"add_light","name":"Key","translation":[0,2,0]},
            {"op":"set_keyframe","id":"Key","property":"light_intensity","frame":1,"value":0,"interpolation":"linear"},
            {"op":"set_keyframe","id":"Key","property":"light_intensity","frame":3,"value":32},
            {"op":"set_keyframe","id":"Key","property":"light_color","frame":1,"value":"#ff0000","interpolation":"step"},
            {"op":"set_keyframe","id":"Key","property":"light_color","frame":3,"value":"#0000ff"}
        ])).unwrap();
        assert!(emitters(&ed, Some(1.)).is_empty());
        let mid = emitters(&ed, Some(2.))[0].sample(DVec3::ZERO).unwrap();
        assert!(mid.1.abs_diff_eq(DVec3::new(4., 0., 0.), 1e-9));
        let end = emitters(&ed, Some(3.))[0].sample(DVec3::ZERO).unwrap();
        assert!(end.1.abs_diff_eq(DVec3::new(0., 0., 8.), 1e-9));
        assert!(
            emitters(&ed, None)[0]
                .sample(DVec3::ZERO)
                .unwrap()
                .1
                .abs_diff_eq(DVec3::splat(2.5), 1e-9)
        );
    }
    #[test]
    fn spot_cones_follow_squared_gltf_falloff_and_local_negative_z() {
        let mut ed = Editor::new();
        apply(&mut ed,json!([{"op":"add_light","name":"Spot","lamp":{"kind":"spot","intensity":16,"inner_cone":0.2,"outer_cone":0.8}}])).unwrap();
        let emitter = &emitters(&ed, None)[0];
        let center = emitter.sample(DVec3::new(0., 0., -2.)).unwrap();
        assert!((center.1.x - 4.).abs() < 1e-9);
        assert!(emitter.sample(DVec3::new(0., 0., 2.)).is_none());
        assert!(emitter.sample(DVec3::new(2., 0., 0.)).is_none());
        let cosine = (0.2_f64.cos() + 0.8_f64.cos()) / 2.;
        let point = DVec3::new((1. - cosine * cosine).sqrt() * 2., 0., -cosine * 2.);
        assert!((emitter.sample(point).unwrap().1.x - 1.).abs() < 1e-9);
        let twice = emitter.sample(point * 2.).unwrap();
        assert!((twice.1.x - 0.25).abs() < 1e-9);
        assert!((twice.2 - 4. * (1. - 1e-4)).abs() < 1e-9);
        apply(&mut ed,json!([{"op":"light_settings","id":"Spot","lamp":{"kind":"spot","intensity":16,"inner_cone":0,"outer_cone":0.04}}])).unwrap();
        let narrow = emitters(&ed, None)[0]
            .sample(DVec3::new(0., 0., -2.))
            .unwrap()
            .1
            .x;
        let expected = 4. * ((1. - 0.04_f64.cos()) / 0.001).powi(2);
        assert!((narrow - expected).abs() < 1e-9);
    }
    #[test]
    fn punctual_ranges_fade_then_stop_but_sun_range_is_inactive() {
        for kind in ["point", "spot"] {
            let mut ed = Editor::new();
            apply(
                &mut ed,
                json!([{"op":"add_light","lamp":{"kind":kind,"intensity":16,"range":4}}]),
            )
            .unwrap();
            let emitter = &emitters(&ed, None)[0];
            assert!(
                (emitter.sample(DVec3::new(0., 0., -2.)).unwrap().1.x
                    - 4. * (1. - 0.0625_f64).powi(2))
                .abs()
                    < 1e-9
            );
            assert!(emitter.sample(DVec3::new(0., 0., -4.)).is_none());
            assert!(emitter.sample(DVec3::new(0., 0., -8.)).is_none());
        }
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([{"op":"add_light","lamp":{"kind":"sun","intensity":16,"range":4}}]),
        )
        .unwrap();
        assert_eq!(
            emitters(&ed, None)[0]
                .sample(DVec3::new(0., 0., -8.))
                .unwrap()
                .1
                .x,
            16.
        );
    }
    #[test]
    fn invalid_spot_settings_reject_batches_and_loaded_scenes_atomically() {
        let mut ed = Editor::new();
        apply(&mut ed, json!([{"op":"add_light","name":"Key"}])).unwrap();
        let before = ed.scene().clone();
        for lamp in [
            json!({"kind":"spot","inner_cone":0.4,"outer_cone":0.4}),
            json!({"kind":"spot","inner_cone":-0.1}),
            json!({"kind":"spot","outer_cone":1.6}),
            json!({"range":0}),
            json!({"range":10001}),
        ] {
            assert!(apply(&mut ed,json!([{"op":"move","id":"Key","offset":[1,0,0]},{"op":"light_settings","id":"Key","lamp":lamp}])).is_err());
            assert_eq!(*ed.scene(), before);
        }
        let mut invalid = before.clone();
        invalid.objects[0].light.as_mut().unwrap().outer_cone = f64::NAN;
        assert!(ed.load(invalid).is_err());
        assert_eq!(*ed.scene(), before);
        let legacy: Lamp =
            serde_json::from_value(json!({"color":"#ffffff","intensity":10})).unwrap();
        assert_eq!(legacy, Lamp::default());
    }
    #[test]
    fn spot_animated_transform_colour_and_power_use_the_same_emitter_path() {
        let mut ed = Editor::new();
        apply(&mut ed,json!([{"op":"add_light","name":"Spot","lamp":{"kind":"spot","intensity":16}},
            {"op":"set_keyframe","id":"Spot","property":"rotation","frame":1,"value":[0,0,0]},
            {"op":"set_keyframe","id":"Spot","property":"rotation","frame":2,"value":[0,std::f64::consts::FRAC_PI_2,0]},
            {"op":"set_keyframe","id":"Spot","property":"light_intensity","frame":1,"value":16},
            {"op":"set_keyframe","id":"Spot","property":"light_intensity","frame":2,"value":32},
            {"op":"set_keyframe","id":"Spot","property":"light_color","frame":2,"value":"#ff0000"}])).unwrap();
        assert!(
            emitters(&ed, Some(1.))[0]
                .sample(DVec3::new(-2., 0., 0.))
                .is_none()
        );
        let power = emitters(&ed, Some(2.))[0]
            .sample(DVec3::new(-2., 0., 0.))
            .unwrap()
            .1;
        assert!((power - DVec3::new(8., 0., 0.)).length() < 1e-9);
        assert!(
            emitters(&ed, Some(2.))[0]
                .sample(DVec3::new(0., 0., -2.))
                .is_none()
        );
    }
    #[test]
    fn spot_settings_are_reviewable_and_replayed_by_history_with_one_undo() {
        let mut ed = Editor::new();
        apply(&mut ed, json!([{"op":"add_light","name":"Key"}])).unwrap();
        apply(&mut ed, json!([{"op":"move","id":"Key","offset":[0,2,0]}])).unwrap();
        let id=ed.propose(&serde_json::from_value(json!({"title":"Focused light","commands":[{"op":"light_settings","id":"Key","lamp":{"kind":"spot","inner_cone":0.2,"outer_cone":0.6,"range":8}}]})).unwrap()).unwrap()[0];
        assert_eq!(
            ed.preview(id).unwrap().scene().objects[0]
                .light
                .as_ref()
                .unwrap()
                .kind,
            Kind::Spot
        );
        assert_eq!(
            ed.scene().objects[0].light.as_ref().unwrap().kind,
            Kind::Point
        );
        ed.accept(id).unwrap();
        ed.undo().unwrap();
        assert_eq!(
            ed.scene().objects[0].light.as_ref().unwrap().kind,
            Kind::Point
        );
        ed.revise(
            1,
            serde_json::from_value(
                json!([{"op":"add_light","name":"Key","lamp":{"kind":"spot","outer_cone":0.6}}]),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            ed.scene().objects[0].light.as_ref().unwrap().kind,
            Kind::Spot
        );
        assert_eq!(ed.scene().objects[0].transform.translation[1], 2.);
        ed.undo().unwrap();
        assert_eq!(
            ed.scene().objects[0].light.as_ref().unwrap().kind,
            Kind::Point
        );
    }
    #[test]
    fn rendered_spot_illumination_respects_geometry_shadows_and_aim() {
        let mut ed = Editor::new();
        apply(&mut ed,json!([{"op":"world","strength":0},{"op":"add","name":"Subject","primitive":{"kind":"cube"},"color":"#ffffff"},
            {"op":"add_light","name":"Spot","translation":[2,2,3],"rotation":[-0.5880026035475675,0.5064446434135005,0],"lamp":{"kind":"spot","inner_cone":0.3,"outer_cone":0.7,"color":"#ff0000","intensity":30}}])).unwrap();
        let camera =
            crate::pathtrace::Camera::new(DVec3::new(0., 0., 4.), DVec3::ZERO, 36., 24, 24);
        let image = crate::pathtrace::traced_uncached(&ed, None)
            .unwrap()
            .still(&camera, 16, true);
        let index = (12 * 24 + 12) * 4;
        assert!(image[index] > 40 && image[index] > image[index + 1] + 30);
        apply(&mut ed,json!([{"op":"add","name":"Occluder","primitive":{"kind":"cube"},"translation":[1,1,1.75],"color":"#000000"}])).unwrap();
        let shadow = crate::pathtrace::traced_uncached(&ed, None)
            .unwrap()
            .still(&camera, 16, true);
        assert!(
            shadow[index] < image[index] / 2,
            "{} vs {}",
            shadow[index],
            image[index]
        );
        ed.undo().unwrap();
        apply(
            &mut ed,
            json!([{"op":"transform","id":"Spot","rotation":[0,std::f64::consts::PI,0]}]),
        )
        .unwrap();
        let away = crate::pathtrace::traced_uncached(&ed, None)
            .unwrap()
            .still(&camera, 16, true);
        assert!(away[index] < image[index] / 2);
    }
}
