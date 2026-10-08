//! Assemblies and layout for agents. Language models describe scenes in
//! relations ("a table with four chairs around it, a lamp on the table"),
//! not coordinates, so these commands take relations: `build` makes a
//! parametric piece of furniture from primitives, `place` puts something on
//! or beside something else, and `arrange` lays several things out in a row,
//! a grid or a circle. Every move ends by settling onto whatever is below.

use glam::{DQuat, DVec3, EulerRot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::{EngineError, Material, ObjRef, Primitive, Scene, Vec3};
use crate::inspect;

fn err<T>(message: impl Into<String>) -> Result<T, EngineError> {
    Err(EngineError::new(message))
}

/// A parametric assembly, built at real-world size (metres). Its front faces +Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Template {
    /// 1.2 x 0.75 m dining table, 0.75 m high.
    Table,
    /// Dining chair with a backrest, seat at 0.45 m.
    Chair,
    /// Table lamp with a glowing shade.
    Lamp,
    /// Ceramic mug with a handle.
    Mug,
    /// Potted plant.
    Plant,
    /// Open bookshelf, 1.2 m high, with three shelves.
    Shelf,
}

impl Template {
    pub fn label(self) -> &'static str {
        match self {
            Template::Table => "Table",
            Template::Chair => "Chair",
            Template::Lamp => "Lamp",
            Template::Mug => "Mug",
            Template::Plant => "Plant",
            Template::Shelf => "Shelf",
        }
    }
}

/// Which side of the reference object `place` puts things on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// -X
    Left,
    /// +X
    Right,
    /// +Z
    Front,
    /// -Z
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// Side by side along X.
    Row,
    /// Rows and columns on the XZ plane.
    Grid,
    /// Evenly around a centre, each turned to face it.
    Circle,
}

/// One primitive of an assembly, in the assembly's own space.
pub struct Part {
    pub name: &'static str,
    pub primitive: Primitive,
    pub at: Vec3,
    pub rotation: Vec3,
    pub scale: Vec3,
    pub material: Material,
    pub smooth: bool,
    /// The main surface: recoloured by `build`'s `color`.
    pub body: bool,
}

fn mat(color: &str, roughness: f64) -> Material {
    Material {
        color: color.into(),
        roughness,
        ..Material::default()
    }
}

fn block(name: &'static str, size: Vec3, at: Vec3, material: Material, body: bool) -> Part {
    Part {
        name,
        primitive: Primitive::Cube { size: 1.0 },
        at,
        rotation: [0.0; 3],
        scale: size,
        material,
        smooth: false,
        body,
    }
}

fn post(name: &'static str, radius: f64, height: f64, at: Vec3, material: Material) -> Part {
    Part {
        name,
        primitive: Primitive::Cylinder {
            radius,
            radius_top: None,
            height,
            segments: 16,
        },
        at,
        rotation: [0.0; 3],
        scale: [1.0; 3],
        material,
        smooth: false,
        body: false,
    }
}

/// The parts of a template, resting on the floor around the origin.
pub fn parts(t: Template) -> Vec<Part> {
    let wood = mat("#8b5a3c", 0.6);
    let dark = mat("#3b2a22", 0.55);
    match t {
        Template::Table => {
            let mut p = vec![block(
                "top",
                [1.2, 0.05, 0.75],
                [0.0, 0.725, 0.0],
                wood.clone(),
                true,
            )];
            for (k, (x, z)) in [(-0.54, -0.31), (0.54, -0.31), (-0.54, 0.31), (0.54, 0.31)]
                .into_iter()
                .enumerate()
            {
                let name = ["leg 1", "leg 2", "leg 3", "leg 4"][k];
                p.push(post(name, 0.03, 0.7, [x, 0.35, z], dark.clone()));
            }
            p
        }
        Template::Chair => {
            let mut p = vec![
                block(
                    "seat",
                    [0.45, 0.05, 0.45],
                    [0.0, 0.45, 0.0],
                    wood.clone(),
                    true,
                ),
                block(
                    "back",
                    [0.45, 0.42, 0.04],
                    [0.0, 0.685, -0.205],
                    wood.clone(),
                    true,
                ),
            ];
            for (k, (x, z)) in [(-0.19, -0.19), (0.19, -0.19), (-0.19, 0.19), (0.19, 0.19)]
                .into_iter()
                .enumerate()
            {
                let name = ["leg 1", "leg 2", "leg 3", "leg 4"][k];
                p.push(post(name, 0.02, 0.425, [x, 0.2125, z], dark.clone()));
            }
            p
        }
        Template::Lamp => {
            let brass = Material {
                metalness: 1.0,
                ..mat("#c9a25a", 0.3)
            };
            let shade = Material {
                emissive: "#ffcf7a".into(),
                emissive_strength: 1.6,
                ..mat("#f3e3c3", 0.8)
            };
            vec![
                Part {
                    primitive: Primitive::Cylinder {
                        radius: 0.09,
                        radius_top: None,
                        height: 0.03,
                        segments: 32,
                    },
                    ..post("base", 0.0, 0.0, [0.0, 0.015, 0.0], brass.clone())
                },
                post("stem", 0.012, 0.36, [0.0, 0.21, 0.0], brass),
                Part {
                    primitive: Primitive::Cylinder {
                        radius: 0.13,
                        radius_top: Some(0.08),
                        height: 0.16,
                        segments: 32,
                    },
                    body: true,
                    ..post("shade", 0.0, 0.0, [0.0, 0.47, 0.0], shade)
                },
            ]
        }
        Template::Mug => {
            let glaze = mat("#ead9c6", 0.25);
            vec![
                Part {
                    name: "cup",
                    primitive: Primitive::Vessel {
                        profile: vec![[0.04, 0.0], [0.044, 0.008], [0.045, 0.09], [0.046, 0.1]],
                        thickness: 0.006,
                        segments: 40,
                        smooth: true,
                    },
                    at: [0.0; 3],
                    rotation: [0.0; 3],
                    scale: [1.0; 3],
                    material: glaze.clone(),
                    smooth: true,
                    body: true,
                },
                Part {
                    name: "handle",
                    primitive: Primitive::Torus {
                        major_radius: 0.028,
                        minor_radius: 0.007,
                        major_segments: 24,
                        minor_segments: 10,
                    },
                    at: [0.062, 0.052, 0.0],
                    rotation: [std::f64::consts::FRAC_PI_2, 0.0, 0.0],
                    scale: [1.0; 3],
                    material: glaze,
                    smooth: true,
                    body: true,
                },
            ]
        }
        Template::Plant => vec![
            Part {
                primitive: Primitive::Cylinder {
                    radius: 0.11,
                    radius_top: Some(0.15),
                    height: 0.26,
                    segments: 32,
                },
                body: true,
                ..post("pot", 0.0, 0.0, [0.0, 0.13, 0.0], mat("#b5643c", 0.8))
            },
            Part {
                name: "leaves",
                primitive: Primitive::Quadsphere {
                    radius: 0.22,
                    level: 3,
                },
                at: [0.0, 0.43, 0.0],
                rotation: [0.0; 3],
                scale: [1.0, 0.85, 1.0],
                material: mat("#4f8a3c", 0.7),
                smooth: true,
                body: false,
            },
        ],
        Template::Shelf => {
            let mut p = vec![
                block(
                    "side 1",
                    [0.03, 1.2, 0.3],
                    [-0.4, 0.6, 0.0],
                    wood.clone(),
                    true,
                ),
                block(
                    "side 2",
                    [0.03, 1.2, 0.3],
                    [0.4, 0.6, 0.0],
                    wood.clone(),
                    true,
                ),
            ];
            for (k, y) in [0.03, 0.42, 0.8, 1.185].into_iter().enumerate() {
                let name = ["board 1", "board 2", "board 3", "board 4"][k];
                p.push(block(
                    name,
                    [0.77, 0.03, 0.3],
                    [0.0, y, 0.0],
                    wood.clone(),
                    true,
                ));
            }
            p
        }
    }
}

/// Indices of the objects an `id` names: one object, or every part of the
/// group of that name.
pub fn targets(scene: &Scene, r: &ObjRef) -> Result<Vec<usize>, EngineError> {
    let found: Vec<usize> = match r {
        ObjRef::Id(id) => scene
            .objects
            .iter()
            .position(|o| o.id == *id)
            .into_iter()
            .collect(),
        ObjRef::Name(name) => match scene.objects.iter().position(|o| &o.name == name) {
            Some(i) => vec![i],
            None => scene
                .objects
                .iter()
                .enumerate()
                .filter(|(_, o)| o.group.as_deref() == Some(name.as_str()))
                .map(|(i, _)| i)
                .collect(),
        },
    };
    if found.is_empty() {
        return match r {
            ObjRef::Id(id) => err(format!("no object with id {id}")),
            ObjRef::Name(name) => err(format!("no object or group named {name:?}")),
        };
    }
    Ok(found)
}

fn ids(scene: &Scene, idx: &[usize]) -> Vec<u64> {
    idx.iter().map(|&i| scene.objects[i].id).collect()
}

/// Translate objects rigidly.
pub fn translate(scene: &mut Scene, idx: &[usize], d: DVec3) {
    for &i in idx {
        let t = &mut scene.objects[i].transform.translation;
        *t = (DVec3::from(*t) + d).to_array();
    }
}

/// Turn objects rigidly about a vertical axis through `pivot`.
pub fn turn(scene: &mut Scene, idx: &[usize], pivot: DVec3, angle: f64) {
    let q = DQuat::from_rotation_y(angle);
    for &i in idx {
        let tf = &mut scene.objects[i].transform;
        let p = DVec3::from(tf.translation);
        tf.translation = (pivot + q * (p - pivot)).to_array();
        let [rx, ry, rz] = tf.rotation;
        let r = q * DQuat::from_euler(EulerRot::XYZ, rx, ry, rz);
        let (x, y, z) = r.to_euler(EulerRot::XYZ);
        tf.rotation = [x, y, z];
    }
}

fn bounds_of(scene: &Scene, idx: &[usize]) -> Result<(DVec3, DVec3), EngineError> {
    let solids = inspect::scene_solids(scene)?;
    inspect::bounds(&solids, &ids(scene, idx))
        .ok_or_else(|| EngineError::new("nothing with faces to place"))
}

/// Lift objects above everything else, then let them fall onto what is below.
pub fn settle_from_above(scene: &mut Scene, idx: &[usize]) -> Result<(), EngineError> {
    let mine = ids(scene, idx);
    let solids = inspect::scene_solids(scene)?;
    let top = solids
        .iter()
        .filter(|s| !mine.contains(&s.id))
        .map(|s| s.max.y)
        .fold(0.0, f64::max);
    let (lo, _) = inspect::bounds(&solids, &mine)
        .ok_or_else(|| EngineError::new("nothing with faces to place"))?;
    translate(scene, idx, DVec3::Y * (top + 0.5 - lo.y));
    let solids = inspect::scene_solids(scene)?;
    let dy = inspect::settle(&solids, &mine)?;
    translate(scene, idx, DVec3::Y * dy);
    Ok(())
}

fn check_spacing(v: f64, what: &str) -> Result<f64, EngineError> {
    if v.is_finite() && (0.0..=100.0).contains(&v) {
        Ok(v)
    } else {
        err(format!("{what} must be between 0 and 100"))
    }
}

/// `place`: move `what` onto `on` (at `at`, fractions of its top) or beside
/// `beside` on `side` with `gap`, then settle it.
pub fn place(
    scene: &mut Scene,
    what: &ObjRef,
    on: Option<&ObjRef>,
    at: Option<[f64; 2]>,
    beside: Option<&ObjRef>,
    side: Option<Side>,
    gap: f64,
) -> Result<(), EngineError> {
    let gap = check_spacing(gap, "gap")?;
    let idx = targets(scene, what)?;
    let (lo, hi) = bounds_of(scene, &idx)?;
    let half = (hi - lo) * 0.5;
    let centre = (lo + hi) * 0.5;
    let target = match (on, beside) {
        (Some(on), None) => {
            let support = targets(scene, on)?;
            if support.iter().any(|i| idx.contains(i)) {
                return err("cannot place an object on itself");
            }
            let (slo, shi) = bounds_of(scene, &support)?;
            let [u, v] = at.unwrap_or([0.5, 0.5]);
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                return err("at must be two fractions between 0 and 1");
            }
            // Keep the footprint on the support where it fits.
            let fit = |a: f64, b: f64, f: f64, h: f64| {
                if b - a > 2.0 * h {
                    (a + (b - a) * f).clamp(a + h, b - h)
                } else {
                    (a + b) * 0.5
                }
            };
            DVec3::new(
                fit(slo.x, shi.x, u, half.x),
                0.0,
                fit(slo.z, shi.z, v, half.z),
            )
        }
        (None, Some(beside)) => {
            let other = targets(scene, beside)?;
            if other.iter().any(|i| idx.contains(i)) {
                return err("cannot place an object beside itself");
            }
            let (blo, bhi) = bounds_of(scene, &other)?;
            let mid = (blo + bhi) * 0.5;
            match side.unwrap_or(Side::Right) {
                Side::Right => DVec3::new(bhi.x + gap + half.x, 0.0, mid.z),
                Side::Left => DVec3::new(blo.x - gap - half.x, 0.0, mid.z),
                Side::Front => DVec3::new(mid.x, 0.0, bhi.z + gap + half.z),
                Side::Back => DVec3::new(mid.x, 0.0, blo.z - gap - half.z),
            }
        }
        _ => return err("place needs exactly one of `on` or `beside`"),
    };
    translate(
        scene,
        &idx,
        DVec3::new(target.x - centre.x, 0.0, target.z - centre.z),
    );
    settle_from_above(scene, &idx)
}

/// `arrange`: lay `items` out and settle each one.
pub fn arrange(
    scene: &mut Scene,
    items: &[ObjRef],
    layout: Layout,
    around: Option<&ObjRef>,
    centre: Option<[f64; 2]>,
    spacing: f64,
    radius: Option<f64>,
) -> Result<(), EngineError> {
    if items.is_empty() || items.len() > 200 {
        return err("arrange needs 1 to 200 items");
    }
    let spacing = check_spacing(spacing, "spacing")?;
    let groups: Vec<Vec<usize>> = items
        .iter()
        .map(|r| targets(scene, r))
        .collect::<Result<_, _>>()?;
    let boxes: Vec<(DVec3, DVec3)> = groups
        .iter()
        .map(|g| bounds_of(scene, g))
        .collect::<Result<_, _>>()?;
    let around_box = around.map(|r| targets(scene, r)).transpose()?;
    let around_box = around_box.map(|g| bounds_of(scene, &g)).transpose()?;
    let c = match (centre, around_box) {
        (Some([x, z]), _) => DVec3::new(x, 0.0, z),
        (None, Some((lo, hi))) => (lo + hi) * 0.5,
        (None, None) => {
            boxes
                .iter()
                .map(|(lo, hi)| (*lo + *hi) * 0.5)
                .sum::<DVec3>()
                / boxes.len() as f64
        }
    };
    let c = DVec3::new(c.x, 0.0, c.z);
    let size = |(lo, hi): &(DVec3, DVec3)| *hi - *lo;
    let mut spots: Vec<(DVec3, Option<f64>)> = Vec::new();
    match layout {
        Layout::Row => {
            let total: f64 =
                boxes.iter().map(|b| size(b).x).sum::<f64>() + spacing * (boxes.len() - 1) as f64;
            let mut x = c.x - total / 2.0;
            for b in &boxes {
                spots.push((DVec3::new(x + size(b).x / 2.0, 0.0, c.z), None));
                x += size(b).x + spacing;
            }
        }
        Layout::Grid => {
            let cols = (boxes.len() as f64).sqrt().ceil() as usize;
            let rows = boxes.len().div_ceil(cols);
            let cell = boxes.iter().map(size).fold(DVec3::ZERO, DVec3::max) + DVec3::splat(spacing);
            for (k, _) in boxes.iter().enumerate() {
                let (r, col) = (k / cols, k % cols);
                let x = c.x + (col as f64 - (cols - 1) as f64 / 2.0) * cell.x;
                let z = c.z + (r as f64 - (rows - 1) as f64 / 2.0) * cell.z;
                spots.push((DVec3::new(x, 0.0, z), None));
            }
        }
        Layout::Circle => {
            let reach = boxes
                .iter()
                .map(|b| size(b).x.max(size(b).z) / 2.0)
                .fold(0.0, f64::max);
            let r = match (radius, around_box) {
                (Some(r), _) => check_spacing(r, "radius")?,
                (None, Some((lo, hi))) => {
                    let h = (hi - lo) * 0.5;
                    // Clear the edge of what we circle on its longer side.
                    h.x.max(h.z) + reach + spacing
                }
                (None, None) => {
                    (reach * 2.0 + spacing) * boxes.len() as f64 / std::f64::consts::TAU + reach
                }
            };
            for k in 0..boxes.len() {
                let a = std::f64::consts::TAU * k as f64 / boxes.len() as f64;
                let p = c + DVec3::new(a.sin(), 0.0, a.cos()) * r;
                // Turn the +Z front toward the centre.
                spots.push((p, Some(a + std::f64::consts::PI)));
            }
        }
    }
    for (g, (spot, facing)) in groups.iter().zip(spots) {
        if let Some(angle) = facing {
            let (lo, hi) = bounds_of(scene, g)?;
            turn(scene, g, (lo + hi) * 0.5, angle);
        }
        let (lo, hi) = bounds_of(scene, g)?;
        let mid = (lo + hi) * 0.5;
        translate(scene, g, DVec3::new(spot.x - mid.x, 0.0, spot.z - mid.z));
        settle_from_above(scene, g)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::engine::{CommandBatch, Editor};

    fn editor(commands: serde_json::Value) -> Editor {
        let mut ed = Editor::new();
        ed.apply(&serde_json::from_value::<CommandBatch>(json!({ "commands": commands })).unwrap())
            .unwrap();
        ed
    }

    #[test]
    fn furnishes_a_dining_set_without_coordinates() {
        let ed = editor(json!([
            {"op": "build", "template": "table"},
            {"op": "build", "template": "chair"},
            {"op": "build", "template": "chair"},
            {"op": "build", "template": "chair"},
            {"op": "build", "template": "chair"},
            {"op": "arrange", "ids": ["Chair", "Chair 2", "Chair 3", "Chair 4"], "layout": "circle", "around": "Table"},
            {"op": "build", "template": "mug"},
            {"op": "place", "id": "Mug", "on": "Table", "at": [0.3, 0.5]},
            {"op": "build", "template": "plant"},
            {"op": "place", "id": "Plant", "beside": "Table", "side": "right", "gap": 0.8}
        ]));
        let report = crate::inspect::inspect(&ed);
        assert_eq!(report["summary"], "no issues", "{}", report["issues"]);
        let o = |name: &str| ed.scene().objects.iter().find(|o| o.name == name).unwrap();
        // The mug sits on the table top (0.75 m).
        let mug = crate::engine::world_bounds(&o("Mug cup").transform, &o("Mug cup").mesh).unwrap();
        assert!((mug.min[1] - 0.75).abs() < 0.003, "{:?}", mug.min);
        // Chairs keep their parts together and face the table.
        let seat = o("Chair 3 seat").transform.translation;
        let back = o("Chair 3 back").transform.translation;
        assert!((seat[1] - 0.45).abs() < 0.003, "seat height {seat:?}");
        let to_table = (seat[0].powi(2) + seat[2].powi(2)).sqrt();
        let back_dist = (back[0].powi(2) + back[2].powi(2)).sqrt();
        assert!(back_dist > to_table, "the backrest is away from the table");
        assert_eq!(o("Chair 3 leg 1").group.as_deref(), Some("Chair 3"));
    }

    #[test]
    fn groups_move_and_delete_together() {
        let mut ed = editor(json!([
            {"op": "build", "template": "chair", "name": "Seat", "translation": [1, 0, 0]},
            {"op": "move", "id": "Seat", "offset": [0, 0, 2], "rotate_y": 1.5}
        ]));
        let parts: Vec<_> = ed
            .scene()
            .objects
            .iter()
            .filter(|o| o.group.as_deref() == Some("Seat"))
            .collect();
        assert_eq!(parts.len(), 6);
        assert!((parts[0].transform.translation[2] - 2.0).abs() < 0.3);
        ed.apply(
            &serde_json::from_value::<CommandBatch>(
                json!({"commands": [{"op": "delete", "id": "Seat"}]}),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(ed.scene().objects.is_empty());
        let bad = serde_json::from_value::<CommandBatch>(
            json!({"commands": [{"op": "place", "id": "Nope", "on": "Table"}]}),
        )
        .unwrap();
        assert!(ed.apply(&bad).is_err());
    }
}
