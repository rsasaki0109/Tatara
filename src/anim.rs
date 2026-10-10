//! Keyframe animation: per-object tracks of transform and material values.
//!
//! A track holds keys sorted by frame. Each key's interpolation decides how
//! the value travels to the next key (like Blender, the left key owns the
//! segment). Objects keep their static transform and material; a property
//! with a track is driven by it, and every other property stays static.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::{EngineError, Material, Object, Transform, check_color};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Property {
    Translation,
    /// Euler XYZ, radians.
    Rotation,
    Scale,
    /// sRGB colour; key it with `#rrggbb`.
    Color,
    Roughness,
    Metalness,
    /// Emitted light colour; key it with `#rrggbb`.
    Emissive,
    EmissiveStrength,
    Opacity,
    /// A bone's pose (Euler XYZ, radians); the track names the bone.
    Bone,
    CameraFov,
    CameraAperture,
    CameraFocus,
    CameraHeight,
    LightColor,
    LightIntensity,
}

impl Property {
    pub fn width(self) -> usize {
        self.range().map_or(3, |_| 1)
    }

    /// The allowed range of a scalar property (`None` for vectors and colours).
    pub fn range(self) -> Option<(f64, f64)> {
        match self {
            Property::Roughness | Property::Metalness | Property::Opacity => Some((0.0, 1.0)),
            Property::EmissiveStrength => Some((0.0, 20.0)),
            Property::CameraFov => Some((1.0, 170.0)),
            Property::CameraAperture => Some((0.0, 1.0)),
            Property::CameraFocus | Property::CameraHeight => Some((0.001, 1e4)),
            Property::LightIntensity => Some((0.0, 100_000.0)),
            _ => None,
        }
    }

    pub fn is_color(self) -> bool {
        matches!(
            self,
            Property::Color | Property::Emissive | Property::LightColor
        )
    }

    pub fn all() -> [Property; 9] {
        use Property::*;
        [
            Translation,
            Rotation,
            Scale,
            Color,
            Roughness,
            Metalness,
            Emissive,
            EmissiveStrength,
            Opacity,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Interpolation {
    /// Smooth ease in and out (the default).
    #[default]
    Ease,
    Linear,
    /// Hold the value until the next key.
    Step,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Key {
    pub frame: f64,
    /// 3 numbers for vectors and colours (sRGB 0..1), 1 for scalars.
    pub value: Vec<f64>,
    #[serde(default)]
    pub interpolation: Interpolation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Track {
    pub property: Property,
    /// The bone a `bone` track poses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bone: Option<String>,
    /// Sorted by frame, at most one key per frame.
    pub keys: Vec<Key>,
}

fn d_fps() -> f64 {
    24.0
}
fn d_start() -> f64 {
    1.0
}
fn d_end() -> f64 {
    96.0
}

/// Playback range and rate for the scene.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Animation {
    #[serde(default = "d_fps")]
    pub fps: f64,
    #[serde(default = "d_start")]
    pub start: f64,
    #[serde(default = "d_end")]
    pub end: f64,
}

impl Default for Animation {
    fn default() -> Self {
        Self {
            fps: d_fps(),
            start: d_start(),
            end: d_end(),
        }
    }
}

impl Animation {
    pub fn validate(&self) -> Result<(), EngineError> {
        let ok = self.fps.is_finite()
            && (1.0..=240.0).contains(&self.fps)
            && check_frame(self.start).is_ok()
            && check_frame(self.end).is_ok()
            && self.end > self.start;
        if ok {
            Ok(())
        } else {
            Err(EngineError::new(
                "animation needs 1-240 fps and start < end within 0-100000",
            ))
        }
    }
}

/// A keyframe value in a command: a number, a vector or a `#rrggbb` colour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum KeyValue {
    Scalar(f64),
    Vector([f64; 3]),
    Color(String),
}

pub fn check_frame(frame: f64) -> Result<f64, EngineError> {
    if frame.is_finite() && (0.0..=100_000.0).contains(&frame) {
        Ok(frame)
    } else {
        Err(EngineError::new("frame must be between 0 and 100000"))
    }
}

pub fn hex_to_rgb(hex: &str) -> [f64; 3] {
    let ch = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    [ch(1), ch(3), ch(5)]
}

pub fn rgb_to_hex(rgb: [f64; 3]) -> String {
    let b = |c: f64| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(rgb[0]), b(rgb[1]), b(rgb[2]))
}

/// Check a command value against the property and convert it to numbers.
pub fn key_value(property: Property, value: &KeyValue) -> Result<Vec<f64>, EngineError> {
    let bad = |what: &str| {
        Err(EngineError::new(
            format!("{property:?} keys need {what}").to_lowercase(),
        ))
    };
    let v = match (property, value) {
        (Property::LightColor, KeyValue::Vector(v))
            if v.iter().all(|x| x.is_finite() && (0.0..=1.0).contains(x)) =>
        {
            v.to_vec()
        }
        (p, KeyValue::Color(c)) if p.is_color() => hex_to_rgb(&check_color(c)?).to_vec(),
        (p, _) if p.is_color() => return bad("a #rrggbb colour"),
        (p, value) if p.range().is_some() => {
            let (lo, hi) = p.range().unwrap();
            match value {
                KeyValue::Scalar(x) if (lo..=hi).contains(x) => vec![*x],
                KeyValue::Scalar(_) => return bad(&format!("a value between {lo} and {hi}")),
                _ => return bad("a number"),
            }
        }
        (_, KeyValue::Vector(v)) => {
            if v.iter().any(|x| !x.is_finite() || x.abs() > 1e6) {
                return bad("finite numbers");
            }
            if property == Property::Scale && v.iter().any(|x| x.abs() < 1e-6) {
                return bad("non-zero scale");
            }
            v.to_vec()
        }
        _ => return bad("[x, y, z]"),
    };
    Ok(v)
}

/// The static (unanimated) value of a property.
pub fn rest_value(o: &Object, property: Property) -> Vec<f64> {
    match property {
        Property::Translation => o.transform.translation.to_vec(),
        Property::Rotation => o.transform.rotation.to_vec(),
        Property::Scale => o.transform.scale.to_vec(),
        Property::Color => hex_to_rgb(&o.material.color).to_vec(),
        Property::Roughness => vec![o.material.roughness],
        Property::Metalness => vec![o.material.metalness],
        Property::Emissive => hex_to_rgb(&o.material.emissive).to_vec(),
        Property::EmissiveStrength => vec![o.material.emissive_strength],
        Property::Opacity => vec![o.material.opacity],
        Property::Bone => vec![0.0; 3],
        Property::CameraFov => vec![o.camera.as_ref().map_or(36.0, |c| c.fov)],
        Property::CameraAperture => vec![o.camera.as_ref().map_or(0.0, |c| c.aperture)],
        Property::CameraFocus => vec![o.camera.as_ref().map_or(10.0, |c| c.focus)],
        Property::CameraHeight => vec![
            o.camera
                .as_ref()
                .and_then(|c| c.ortho_height)
                .unwrap_or(4.0),
        ],
        Property::LightColor => {
            hex_to_rgb(o.light.as_ref().map_or("#ffffff", |l| l.color.as_str())).to_vec()
        }
        Property::LightIntensity => vec![o.light.as_ref().map_or(10.0, |l| l.intensity)],
    }
}

pub fn sample_track(track: &Track, frame: f64) -> Vec<f64> {
    let keys = &track.keys;
    let (first, last) = (&keys[0], &keys[keys.len() - 1]);
    if frame <= first.frame {
        return first.value.clone();
    }
    if frame >= last.frame {
        return last.value.clone();
    }
    let i = keys.partition_point(|k| k.frame <= frame) - 1;
    let (a, b) = (&keys[i], &keys[i + 1]);
    let t = (frame - a.frame) / (b.frame - a.frame);
    let t = match a.interpolation {
        Interpolation::Step => 0.0,
        Interpolation::Linear => t,
        Interpolation::Ease => t * t * (3.0 - 2.0 * t),
    };
    a.value
        .iter()
        .zip(&b.value)
        .map(|(x, y)| x + (y - x) * t)
        .collect()
}

/// A property's value at `frame`: from its track, or the static value.
pub fn value_at(o: &Object, property: Property, frame: f64) -> Vec<f64> {
    match o
        .tracks
        .iter()
        .find(|t| t.property == property && !t.keys.is_empty())
    {
        Some(track) => sample_track(track, frame),
        None => rest_value(o, property),
    }
}

/// Transform and material of an object at `frame`.
pub fn pose(o: &Object, frame: f64) -> (Transform, Material) {
    if o.tracks.is_empty() {
        return (o.transform.clone(), o.material.clone());
    }
    let v3 = |p| {
        let v = value_at(o, p, frame);
        [v[0], v[1], v[2]]
    };
    let transform = Transform {
        translation: v3(Property::Translation),
        rotation: v3(Property::Rotation),
        scale: v3(Property::Scale),
    };
    let scalar = |p: Property| {
        let (lo, hi) = p.range().unwrap();
        value_at(o, p, frame)[0].clamp(lo, hi)
    };
    let material = Material {
        color: rgb_to_hex(v3(Property::Color)),
        roughness: scalar(Property::Roughness),
        metalness: scalar(Property::Metalness),
        emissive: rgb_to_hex(v3(Property::Emissive)),
        emissive_strength: scalar(Property::EmissiveStrength),
        opacity: scalar(Property::Opacity),
        transmission: o.material.transmission,
        texture: o.material.texture.clone(),
    };
    (transform, material)
}

/// Sample scene optics without changing the unanimated rest settings.
pub fn lens_at(o: &Object, frame: Option<f64>) -> Option<crate::camera::Lens> {
    let mut lens = o.camera.clone()?;
    if let Some(f) = frame {
        lens.fov = value_at(o, Property::CameraFov, f)[0];
        lens.aperture = value_at(o, Property::CameraAperture, f)[0];
        lens.focus = value_at(o, Property::CameraFocus, f)[0];
        if lens.ortho_height.is_some() {
            lens.ortho_height = Some(value_at(o, Property::CameraHeight, f)[0]);
        }
    }
    Some(lens)
}

pub fn lamp_at(o: &Object, frame: Option<f64>) -> Option<crate::light::Lamp> {
    let mut lamp = o.light.clone()?;
    if let Some(f) = frame {
        let v = value_at(o, Property::LightColor, f);
        lamp.color = rgb_to_hex([v[0], v[1], v[2]]);
        lamp.intensity = value_at(o, Property::LightIntensity, f)[0];
    }
    Some(lamp)
}

pub fn validate_property(o: &Object, property: Property) -> Result<(), EngineError> {
    let valid = match property {
        Property::CameraFov
        | Property::CameraAperture
        | Property::CameraFocus
        | Property::CameraHeight => o.camera.is_some(),
        Property::LightColor | Property::LightIntensity => o.light.is_some(),
        _ => true,
    };
    if !valid {
        return Err(EngineError::new(format!(
            "{:?} is not a property of {}",
            property, o.name
        )));
    }
    Ok(())
}

/// Insert or replace the key at `frame` (of `bone` for bone tracks),
/// keeping the track sorted.
pub fn set_key(
    o: &mut Object,
    property: Property,
    bone: Option<&str>,
    frame: f64,
    value: Vec<f64>,
    interpolation: Interpolation,
) -> Result<(), EngineError> {
    validate_property(o, property)?;
    let track = match o
        .tracks
        .iter_mut()
        .position(|t| t.property == property && t.bone.as_deref() == bone)
    {
        Some(i) => &mut o.tracks[i],
        None => {
            o.tracks.push(Track {
                property,
                bone: bone.map(str::to_owned),
                keys: Vec::new(),
            });
            o.tracks.last_mut().unwrap()
        }
    };
    if track.keys.len() >= 10_000 {
        return Err(EngineError::new("a track can hold at most 10000 keys"));
    }
    let key = Key {
        frame,
        value,
        interpolation,
    };
    match track
        .keys
        .iter()
        .position(|k| (k.frame - frame).abs() < 1e-9)
    {
        Some(i) => track.keys[i] = key,
        None => {
            let at = track.keys.partition_point(|k| k.frame < frame);
            track.keys.insert(at, key);
        }
    }
    Ok(())
}

/// Remove keys at `frame` (from one property or all, and for bones from
/// one bone or all); returns how many.
pub fn delete_key(
    o: &mut Object,
    property: Option<Property>,
    bone: Option<&str>,
    frame: f64,
) -> usize {
    let mut removed = 0;
    for t in o.tracks.iter_mut().filter(|t| {
        property.is_none_or(|p| p == t.property)
            && bone.is_none_or(|b| t.bone.as_deref() == Some(b))
    }) {
        let before = t.keys.len();
        t.keys.retain(|k| (k.frame - frame).abs() >= 1e-9);
        removed += before - t.keys.len();
    }
    o.tracks.retain(|t| !t.keys.is_empty());
    removed
}

pub fn validate_tracks(o: &Object) -> Result<(), EngineError> {
    for (i, t) in o.tracks.iter().enumerate() {
        validate_property(o, t.property)?;
        if t.keys.is_empty() {
            return Err(EngineError::new("a track needs at least one key"));
        }
        match (&t.bone, t.property) {
            (Some(b), Property::Bone) if o.bones.iter().any(|x| &x.name == b) => {}
            (None, p) if p != Property::Bone => {}
            _ => {
                return Err(EngineError::new(
                    "a bone track must name a bone of the object (and only bone tracks do)",
                ));
            }
        }
        if o.tracks[..i]
            .iter()
            .any(|x| x.property == t.property && x.bone == t.bone)
        {
            return Err(EngineError::new("two tracks animate the same property"));
        }
        for (i, k) in t.keys.iter().enumerate() {
            check_frame(k.frame)?;
            if k.value.len() != t.property.width() || k.value.iter().any(|v| !v.is_finite()) {
                return Err(EngineError::new(format!(
                    "{:?} key at frame {} has the wrong value",
                    t.property, k.frame
                )));
            }
            if matches!(
                t.property,
                Property::CameraFov
                    | Property::CameraAperture
                    | Property::CameraFocus
                    | Property::CameraHeight
                    | Property::LightColor
                    | Property::LightIntensity
            ) {
                if let Some((lo, hi)) = t.property.range() {
                    if !(lo..=hi).contains(&k.value[0]) {
                        return Err(EngineError::new(
                            "optical key is outside its property range",
                        ));
                    }
                } else if k.value.iter().any(|v| !(0.0..=1.0).contains(v)) {
                    return Err(EngineError::new(
                        "light colour keys need sRGB components in 0-1",
                    ));
                }
            }
            if i > 0 && k.frame <= t.keys[i - 1].frame {
                return Err(EngineError::new("track keys must be sorted by frame"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(interp: Interpolation) -> Track {
        Track {
            property: Property::Translation,
            bone: None,
            keys: vec![
                Key {
                    frame: 0.0,
                    value: vec![0.0, 0.0, 0.0],
                    interpolation: interp,
                },
                Key {
                    frame: 10.0,
                    value: vec![10.0, 0.0, -10.0],
                    interpolation: interp,
                },
            ],
        }
    }

    #[test]
    fn interpolation_modes() {
        assert_eq!(
            sample_track(&track(Interpolation::Linear), 2.5),
            vec![2.5, 0.0, -2.5]
        );
        assert_eq!(
            sample_track(&track(Interpolation::Step), 9.9),
            vec![0.0, 0.0, 0.0]
        );
        let e = sample_track(&track(Interpolation::Ease), 2.5)[0];
        assert!(e > 0.0 && e < 2.5, "ease starts slow: {e}");
        assert_eq!(sample_track(&track(Interpolation::Ease), 5.0)[0], 5.0);
        // Clamped outside the keyed range.
        assert_eq!(sample_track(&track(Interpolation::Linear), -3.0)[0], 0.0);
        assert_eq!(sample_track(&track(Interpolation::Linear), 99.0)[0], 10.0);
    }

    #[test]
    fn values_are_checked_per_property() {
        assert!(key_value(Property::Color, &KeyValue::Color("#ff0000".into())).is_ok());
        assert!(key_value(Property::Color, &KeyValue::Scalar(1.0)).is_err());
        assert!(key_value(Property::Roughness, &KeyValue::Scalar(1.5)).is_err());
        assert!(key_value(Property::Scale, &KeyValue::Vector([1.0, 0.0, 1.0])).is_err());
        assert!(key_value(Property::Emissive, &KeyValue::Color("#00ffcc".into())).is_ok());
        assert!(key_value(Property::EmissiveStrength, &KeyValue::Scalar(8.0)).is_ok());
        assert!(key_value(Property::EmissiveStrength, &KeyValue::Scalar(21.0)).is_err());
        assert!(key_value(Property::Opacity, &KeyValue::Vector([1.0; 3])).is_err());
        assert_eq!(rgb_to_hex(hex_to_rgb("#8fb9a0")), "#8fb9a0");
    }
    fn optical_apply(
        ed: &mut crate::engine::Editor,
        commands: serde_json::Value,
    ) -> Result<crate::engine::ApplyResult, EngineError> {
        ed.apply(&serde_json::from_value(serde_json::json!({"commands":commands})).unwrap())
    }

    #[test]
    fn camera_optics_sample_linear_eased_and_stepped_tracks_without_changing_rest() {
        use serde_json::json;
        let mut ed = crate::engine::Editor::new();
        optical_apply(&mut ed,json!([
            {"op":"add_camera","name":"Shot"},
            {"op":"set_keyframe","id":"Shot","property":"camera_fov","frame":1,"value":20,"interpolation":"linear"},
            {"op":"set_keyframe","id":"Shot","property":"camera_fov","frame":5,"value":60},
            {"op":"set_keyframe","id":"Shot","property":"camera_aperture","frame":1,"value":0,"interpolation":"ease"},
            {"op":"set_keyframe","id":"Shot","property":"camera_aperture","frame":5,"value":0.4},
            {"op":"set_keyframe","id":"Shot","property":"camera_focus","frame":1,"value":2,"interpolation":"step"},
            {"op":"set_keyframe","id":"Shot","property":"camera_focus","frame":5,"value":8}
        ])).unwrap();
        let o = &ed.scene().objects[0];
        let lens = lens_at(o, Some(2.)).unwrap();
        assert_eq!(lens.fov, 30.);
        assert!((lens.aperture - 0.0625).abs() < 1e-9);
        assert_eq!(lens.focus, 2.);
        assert_eq!(lens_at(o, None).unwrap(), crate::camera::Lens::default());
        let c = crate::camera::resolve(
            &ed,
            &crate::engine::ObjRef::Name("Shot".into()),
            Some(2.),
            16,
            16,
        )
        .unwrap();
        assert_eq!(
            (c.fov, c.aperture, c.focus),
            (lens.fov, lens.aperture, lens.focus)
        );
        assert_eq!(lens_at(o, Some(100.)).unwrap().focus, 8.);
        let ctx = crate::engine::context_at(&ed, Some(2.));
        assert_eq!(ctx["objects"][0]["camera"]["fov"], 30.);
    }

    #[test]
    fn light_optics_sample_color_and_power_in_srgb_and_keep_static_kind() {
        use serde_json::json;
        let mut ed = crate::engine::Editor::new();
        optical_apply(&mut ed,json!([
            {"op":"add_light","name":"Key","lamp":{"kind":"sun","intensity":16}},
            {"op":"set_keyframe","id":"Key","property":"light_color","frame":1,"value":"#ff0000","interpolation":"linear"},
            {"op":"set_keyframe","id":"Key","property":"light_color","frame":3,"value":"#0000ff"},
            {"op":"set_keyframe","id":"Key","property":"light_intensity","frame":1,"value":0,"interpolation":"linear"},
            {"op":"set_keyframe","id":"Key","property":"light_intensity","frame":3,"value":20}
        ])).unwrap();
        let o = &ed.scene().objects[0];
        let lamp = lamp_at(o, Some(2.)).unwrap();
        assert_eq!(lamp.color, "#800080");
        assert_eq!(lamp.intensity, 10.);
        assert_eq!(lamp.kind, crate::light::Kind::Sun);
        assert_eq!(lamp_at(o, None).unwrap().intensity, 16.);
        assert!(crate::light::emitters(&ed, Some(1.)).is_empty());
        let ctx = crate::engine::context_at(&ed, Some(2.));
        assert_eq!(ctx["objects"][0]["light"]["intensity"], 10.);
    }

    #[test]
    fn optical_keys_reject_wrong_object_and_invalid_values_atomically() {
        use serde_json::json;
        let mut ed = crate::engine::Editor::new();
        optical_apply(&mut ed,json!([{"op":"add","name":"Cube","primitive":{"kind":"cube"}},{"op":"add_camera","name":"Shot"},{"op":"add_light","name":"Key"}])).unwrap();
        let before = ed.scene().clone();
        for (id, property, value) in [
            ("Cube", "camera_fov", json!(40)),
            ("Shot", "light_intensity", json!(20)),
            ("Key", "camera_focus", json!(4)),
            ("Shot", "camera_fov", json!(171)),
            ("Shot", "camera_focus", json!(0)),
            ("Key", "light_color", json!([2, 0, 0])),
            ("Key", "light_intensity", json!(-1)),
        ] {
            assert!(optical_apply(&mut ed,json!([{"op":"move","id":"Cube","offset":[1,0,0]},{"op":"set_keyframe","id":id,"property":property,"frame":2,"value":value}])).is_err());
            assert_eq!(*ed.scene(), before);
        }
    }

    #[test]
    fn loaded_optical_tracks_reject_bad_ranges_and_wrong_targets() {
        use serde_json::json;
        let mut ed = crate::engine::Editor::new();
        optical_apply(&mut ed, json!([{"op":"add_camera","name":"Shot"}])).unwrap();
        let mut o = ed.scene().objects[0].clone();
        o.tracks = vec![Track {
            property: Property::CameraFov,
            bone: None,
            keys: vec![Key {
                frame: 1.,
                value: vec![171.],
                interpolation: Interpolation::Linear,
            }],
        }];
        assert!(validate_tracks(&o).is_err());
        o.tracks[0].keys[0].value = vec![60.];
        assert!(validate_tracks(&o).is_ok());
        o.tracks[0].property = Property::LightIntensity;
        assert!(validate_tracks(&o).is_err());
    }

    #[test]
    fn optical_keys_share_proposal_review_history_replay_and_single_undo() {
        use serde_json::json;
        let mut ed = crate::engine::Editor::new();
        optical_apply(&mut ed, json!([{"op":"add_camera","name":"Shot"}])).unwrap();
        let keys = |a, b| {
            json!([
                {"op":"set_keyframe","id":"Shot","property":"camera_fov","frame":1,"value":a,"interpolation":"linear"},
                {"op":"set_keyframe","id":"Shot","property":"camera_fov","frame":3,"value":b}
            ])
        };
        optical_apply(&mut ed, keys(20, 60)).unwrap();
        let ids=ed.propose(&serde_json::from_value(json!({"title":"Widen the ending","commands":[{"op":"set_keyframe","id":"Shot","property":"camera_fov","frame":3,"value":100}]})).unwrap()).unwrap();
        assert_eq!(
            lens_at(&ed.preview(ids[0]).unwrap().scene().objects[0], Some(2.))
                .unwrap()
                .fov,
            60.
        );
        assert_eq!(lens_at(&ed.scene().objects[0], Some(2.)).unwrap().fov, 40.);
        ed.accept(ids[0]).unwrap();
        assert_eq!(lens_at(&ed.scene().objects[0], Some(2.)).unwrap().fov, 60.);
        ed.undo().unwrap();
        assert_eq!(lens_at(&ed.scene().objects[0], Some(2.)).unwrap().fov, 40.);
        let commands: Vec<crate::engine::Command> = serde_json::from_value(keys(30, 90)).unwrap();
        assert_eq!(
            lens_at(
                &ed.revise_preview(2, commands.clone())
                    .unwrap()
                    .scene()
                    .objects[0],
                Some(2.)
            )
            .unwrap()
            .fov,
            60.
        );
        ed.revise(2, commands).unwrap();
        assert_eq!(lens_at(&ed.scene().objects[0], Some(2.)).unwrap().fov, 60.);
        ed.undo().unwrap();
        assert_eq!(lens_at(&ed.scene().objects[0], Some(2.)).unwrap().fov, 40.);
    }
}
