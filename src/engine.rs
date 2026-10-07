//! Scene model, procedural geometry, modeling commands and undo history.
//!
//! Every edit goes through [`Editor::apply`]. A batch is applied to a copy of
//! the scene and only committed when every command succeeds, so an invalid
//! batch never leaves a half-edited scene behind. One successful batch is one
//! undo step.

use std::collections::HashMap;

use glam::{DMat4, DQuat, DVec3, EulerRot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub type Vec3 = [f64; 3];

const MAX_OBJECTS: usize = 2_000;
const MAX_FACES: usize = 250_000;
const MAX_NAME: usize = 80;
const HISTORY_LIMIT: usize = 200;

// ---------------------------------------------------------------------------
// Scene data
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Mesh {
    /// Vertex positions in object space.
    pub vertices: Vec<Vec3>,
    /// Polygons as counter-clockwise vertex index loops (outward normals).
    pub faces: Vec<Vec<u32>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Transform {
    pub translation: Vec3,
    /// Euler angles in radians, XYZ order.
    pub rotation: Vec3,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation: [0.0; 3],
            scale: [1.0; 3],
        }
    }
}

impl Transform {
    pub fn matrix(&self) -> DMat4 {
        let [rx, ry, rz] = self.rotation;
        DMat4::from_scale_rotation_translation(
            DVec3::from(self.scale),
            DQuat::from_euler(EulerRot::XYZ, rx, ry, rz),
            DVec3::from(self.translation),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Material {
    /// sRGB colour as `#rrggbb`.
    pub color: String,
    pub roughness: f64,
    pub metalness: f64,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            color: "#c9c3b8".into(),
            roughness: 0.55,
            metalness: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Object {
    pub id: u64,
    pub name: String,
    /// The primitive this object was generated from (informational).
    pub kind: String,
    pub transform: Transform,
    pub material: Material,
    pub mesh: Mesh,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Scene {
    pub objects: Vec<Object>,
    pub next_id: u64,
    pub revision: u64,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            objects: Vec::new(),
            next_id: 1,
            revision: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// An object reference: numeric ID or exact object name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ObjRef {
    Id(u64),
    Name(String),
}

fn d_cube() -> f64 {
    1.0
}
fn d_radius() -> f64 {
    0.5
}
fn d_height() -> f64 {
    1.0
}
fn d_segments() -> u32 {
    32
}
fn d_rings() -> u32 {
    16
}
fn d_major() -> f64 {
    0.5
}
fn d_minor() -> f64 {
    0.18
}
fn d_major_seg() -> u32 {
    48
}
fn d_minor_seg() -> u32 {
    20
}
fn d_thickness() -> f64 {
    0.03
}
fn d_vessel_seg() -> u32 {
    48
}
fn d_levels() -> u32 {
    1
}
fn d_true() -> bool {
    true
}

/// Procedural primitive. Sizes are meters, Y is up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Primitive {
    /// Axis-aligned cube centred on the origin.
    Cube {
        #[serde(default = "d_cube")]
        size: f64,
    },
    /// Single upward-facing square centred on the origin.
    Plane {
        #[serde(default = "d_cube")]
        size: f64,
    },
    /// UV sphere centred on the origin.
    Sphere {
        #[serde(default = "d_radius")]
        radius: f64,
        #[serde(default = "d_segments")]
        segments: u32,
        #[serde(default = "d_rings")]
        rings: u32,
    },
    /// Capped cylinder (or cone/frustum with `radius_top`) centred on the origin.
    Cylinder {
        #[serde(default = "d_radius")]
        radius: f64,
        #[serde(default)]
        radius_top: Option<f64>,
        #[serde(default = "d_height")]
        height: f64,
        #[serde(default = "d_segments")]
        segments: u32,
    },
    /// Torus lying in the XZ plane.
    Torus {
        #[serde(default = "d_major")]
        major_radius: f64,
        #[serde(default = "d_minor")]
        minor_radius: f64,
        #[serde(default = "d_major_seg")]
        major_segments: u32,
        #[serde(default = "d_minor_seg")]
        minor_segments: u32,
    },
    /// Hollow vessel revolved around Y from an outer cross section.
    /// `profile` is a list of `[radius, height]` points from the foot to the rim.
    Vessel {
        profile: Vec<[f64; 2]>,
        #[serde(default = "d_thickness")]
        thickness: f64,
        #[serde(default = "d_vessel_seg")]
        segments: u32,
        /// Pass a smooth spline through the profile points (default true).
        #[serde(default = "d_true")]
        smooth: bool,
    },
}

impl Primitive {
    pub fn kind(&self) -> &'static str {
        match self {
            Primitive::Cube { .. } => "cube",
            Primitive::Plane { .. } => "plane",
            Primitive::Sphere { .. } => "sphere",
            Primitive::Cylinder { .. } => "cylinder",
            Primitive::Torus { .. } => "torus",
            Primitive::Vessel { .. } => "vessel",
        }
    }
}

/// One modeling operation. Objects are referenced by `id` (number or exact name).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    /// Create an object from a primitive. Its ID is the scene's `next_id`.
    Add {
        #[serde(default)]
        name: Option<String>,
        primitive: Primitive,
        #[serde(default)]
        translation: Option<Vec3>,
        #[serde(default)]
        rotation: Option<Vec3>,
        #[serde(default)]
        scale: Option<Vec3>,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        roughness: Option<f64>,
        #[serde(default)]
        metalness: Option<f64>,
    },
    /// Set any of translation, rotation (radians) or scale.
    Transform {
        id: ObjRef,
        #[serde(default)]
        translation: Option<Vec3>,
        #[serde(default)]
        rotation: Option<Vec3>,
        #[serde(default)]
        scale: Option<Vec3>,
    },
    /// Set any of the material properties.
    Material {
        id: ObjRef,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        roughness: Option<f64>,
        #[serde(default)]
        metalness: Option<f64>,
    },
    Rename {
        id: ObjRef,
        name: String,
    },
    /// Copy an object. `offset` is added to the copy's translation.
    Duplicate {
        id: ObjRef,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        offset: Option<Vec3>,
    },
    Delete {
        id: ObjRef,
    },
    /// Extrude one polygon along its normal (object space).
    Extrude {
        id: ObjRef,
        face: usize,
        distance: f64,
    },
    /// Catmull-Clark subdivision, 1 to 4 levels.
    Subdivide {
        id: ObjRef,
        #[serde(default = "d_levels")]
        levels: u32,
    },
    /// Create `count` copies, each offset by `offset` from the previous one.
    Array {
        id: ObjRef,
        count: u32,
        offset: Vec3,
    },
    /// Remove every object.
    Clear {},
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CommandBatch {
    pub commands: Vec<Command>,
    /// Reject the batch unless the scene is at this revision.
    #[serde(default)]
    pub expected_revision: Option<u64>,
}

pub fn command_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(CommandBatch)).expect("schema serializes")
}

// ---------------------------------------------------------------------------
// Errors and results
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EngineError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_index: Option<usize>,
    /// True when the batch was rejected because of `expected_revision`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.command_index {
            Some(i) => write!(f, "command {i}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for EngineError {}

fn err<T>(message: impl Into<String>) -> Result<T, EngineError> {
    Err(EngineError {
        message: message.into(),
        command_index: None,
        stale: false,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub revision: u64,
    pub created: Vec<u64>,
}

// ---------------------------------------------------------------------------
// Editor (scene + history)
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct Editor {
    scene: Scene,
    undo: Vec<Scene>,
    redo: Vec<Scene>,
}

impl Editor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn apply(&mut self, batch: &CommandBatch) -> Result<ApplyResult, EngineError> {
        if let Some(expected) = batch.expected_revision
            && expected != self.scene.revision
        {
            return Err(EngineError {
                message: format!(
                    "stale revision: expected {expected}, scene is at {}",
                    self.scene.revision
                ),
                command_index: None,
                stale: true,
            });
        }
        if batch.commands.is_empty() {
            return err("batch has no commands");
        }
        let mut next = self.scene.clone();
        let mut created = Vec::new();
        for (i, command) in batch.commands.iter().enumerate() {
            apply_command(&mut next, command, &mut created).map_err(|mut e| {
                e.command_index = Some(i);
                e
            })?;
        }
        self.commit(next);
        Ok(ApplyResult {
            revision: self.scene.revision,
            created,
        })
    }

    /// Replace the whole scene (e.g. opening a file). Undoable.
    pub fn load(&mut self, scene: Scene) -> Result<u64, EngineError> {
        validate_scene(&scene)?;
        self.commit(scene);
        Ok(self.scene.revision)
    }

    fn commit(&mut self, mut next: Scene) {
        let previous = std::mem::take(&mut self.scene);
        next.revision = previous.revision + 1;
        self.undo.push(previous);
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.scene = next;
    }

    pub fn undo(&mut self) -> Option<u64> {
        let mut previous = self.undo.pop()?;
        previous.revision = self.scene.revision + 1;
        let current = std::mem::replace(&mut self.scene, previous);
        self.redo.push(current);
        Some(self.scene.revision)
    }

    pub fn redo(&mut self) -> Option<u64> {
        let mut next = self.redo.pop()?;
        next.revision = self.scene.revision + 1;
        let current = std::mem::replace(&mut self.scene, next);
        self.undo.push(current);
        Some(self.scene.revision)
    }

    /// Clear scene and history (used by tests and recording).
    pub fn reset(&mut self) {
        let revision = self.scene.revision + 1;
        *self = Self::default();
        self.scene.revision = revision;
    }
}

fn resolve(scene: &Scene, r: &ObjRef) -> Result<usize, EngineError> {
    let found = match r {
        ObjRef::Id(id) => scene.objects.iter().position(|o| o.id == *id),
        ObjRef::Name(name) => scene.objects.iter().position(|o| &o.name == name),
    };
    match found {
        Some(i) => Ok(i),
        None => match r {
            ObjRef::Id(id) => err(format!("no object with id {id}")),
            ObjRef::Name(name) => err(format!("no object named {name:?}")),
        },
    }
}

fn apply_command(
    scene: &mut Scene,
    command: &Command,
    created: &mut Vec<u64>,
) -> Result<(), EngineError> {
    match command {
        Command::Add {
            name,
            primitive,
            translation,
            rotation,
            scale,
            color,
            roughness,
            metalness,
        } => {
            if scene.objects.len() >= MAX_OBJECTS {
                return err(format!("scene is limited to {MAX_OBJECTS} objects"));
            }
            let mesh = build_primitive(primitive)?;
            let kind = primitive.kind();
            let id = scene.next_id;
            let name = match name {
                Some(n) => check_name(n)?,
                None => unique_name(scene, &capitalize(kind)),
            };
            let mut transform = Transform::default();
            set_transform(&mut transform, translation, rotation, scale)?;
            let mut material = Material::default();
            set_material(&mut material, color, roughness, metalness)?;
            scene.objects.push(Object {
                id,
                name,
                kind: kind.into(),
                transform,
                material,
                mesh,
            });
            scene.next_id += 1;
            created.push(id);
        }
        Command::Transform {
            id,
            translation,
            rotation,
            scale,
        } => {
            let i = resolve(scene, id)?;
            set_transform(
                &mut scene.objects[i].transform,
                translation,
                rotation,
                scale,
            )?;
        }
        Command::Material {
            id,
            color,
            roughness,
            metalness,
        } => {
            let i = resolve(scene, id)?;
            set_material(&mut scene.objects[i].material, color, roughness, metalness)?;
        }
        Command::Rename { id, name } => {
            let i = resolve(scene, id)?;
            scene.objects[i].name = check_name(name)?;
        }
        Command::Duplicate { id, name, offset } => {
            let i = resolve(scene, id)?;
            let offset = offset.unwrap_or([0.0; 3]);
            check_vec(&offset, "offset")?;
            let name = name.as_deref().map(check_name).transpose()?;
            let new_id = duplicate(scene, i, name, offset)?;
            created.push(new_id);
        }
        Command::Delete { id } => {
            let i = resolve(scene, id)?;
            scene.objects.remove(i);
        }
        Command::Extrude { id, face, distance } => {
            let i = resolve(scene, id)?;
            if !distance.is_finite() || *distance == 0.0 {
                return err("distance must be a non-zero finite number");
            }
            extrude(&mut scene.objects[i].mesh, *face, *distance)?;
        }
        Command::Subdivide { id, levels } => {
            let i = resolve(scene, id)?;
            if !(1..=4).contains(levels) {
                return err("levels must be between 1 and 4");
            }
            let mut mesh = scene.objects[i].mesh.clone();
            for _ in 0..*levels {
                let corners: usize = mesh.faces.iter().map(Vec::len).sum();
                if corners > MAX_FACES {
                    return err(format!("subdivision would exceed {MAX_FACES} faces"));
                }
                mesh = catmull_clark(&mesh);
            }
            scene.objects[i].mesh = mesh;
        }
        Command::Array { id, count, offset } => {
            let i = resolve(scene, id)?;
            check_vec(offset, "offset")?;
            if !(1..=100).contains(count) {
                return err("count must be between 1 and 100");
            }
            let mut source = i;
            for _ in 0..*count {
                let new_id = duplicate(scene, source, None, *offset)?;
                created.push(new_id);
                source = scene.objects.len() - 1;
            }
        }
        Command::Clear {} => scene.objects.clear(),
    }
    Ok(())
}

fn duplicate(
    scene: &mut Scene,
    i: usize,
    name: Option<String>,
    offset: Vec3,
) -> Result<u64, EngineError> {
    if scene.objects.len() >= MAX_OBJECTS {
        return err(format!("scene is limited to {MAX_OBJECTS} objects"));
    }
    let mut copy = scene.objects[i].clone();
    copy.id = scene.next_id;
    copy.name = match name {
        Some(n) => n,
        None => unique_name(scene, &base_name(&copy.name)),
    };
    for (t, o) in copy.transform.translation.iter_mut().zip(offset) {
        *t += o;
    }
    scene.next_id += 1;
    let id = copy.id;
    scene.objects.push(copy);
    Ok(id)
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// "Vase.002" -> "Vase"
fn base_name(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((base, n))
            if !base.is_empty() && n.len() == 3 && n.chars().all(|c| c.is_ascii_digit()) =>
        {
            base.into()
        }
        _ => name.into(),
    }
}

fn unique_name(scene: &Scene, base: &str) -> String {
    let taken = |n: &str| scene.objects.iter().any(|o| o.name == n);
    if !taken(base) {
        return base.into();
    }
    (1..)
        .map(|i| format!("{base}.{i:03}"))
        .find(|n| !taken(n))
        .expect("an unused name exists")
}

fn check_name(name: &str) -> Result<String, EngineError> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.chars().count() > MAX_NAME
        || trimmed.chars().any(char::is_control)
    {
        return err(format!("name must be 1-{MAX_NAME} printable characters"));
    }
    Ok(trimmed.into())
}

fn check_vec(v: &Vec3, what: &str) -> Result<(), EngineError> {
    if v.iter().all(|x| x.is_finite() && x.abs() < 1e6) {
        Ok(())
    } else {
        err(format!("{what} must contain finite numbers"))
    }
}

fn set_transform(
    t: &mut Transform,
    tr: &Option<Vec3>,
    rot: &Option<Vec3>,
    sc: &Option<Vec3>,
) -> Result<(), EngineError> {
    if let Some(v) = tr {
        check_vec(v, "translation")?;
        t.translation = *v;
    }
    if let Some(v) = rot {
        check_vec(v, "rotation")?;
        t.rotation = *v;
    }
    if let Some(v) = sc {
        check_vec(v, "scale")?;
        if v.iter().any(|x| x.abs() < 1e-6) {
            return err("scale components must be non-zero");
        }
        t.scale = *v;
    }
    Ok(())
}

fn set_material(
    m: &mut Material,
    color: &Option<String>,
    rough: &Option<f64>,
    metal: &Option<f64>,
) -> Result<(), EngineError> {
    if let Some(c) = color {
        m.color = check_color(c)?;
    }
    for (value, slot, what) in [
        (rough, &mut m.roughness, "roughness"),
        (metal, &mut m.metalness, "metalness"),
    ] {
        if let Some(v) = value {
            if !(0.0..=1.0).contains(v) {
                return err(format!("{what} must be between 0 and 1"));
            }
            *slot = *v;
        }
    }
    Ok(())
}

pub fn check_color(c: &str) -> Result<String, EngineError> {
    let ok = c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|ch| ch.is_ascii_hexdigit());
    if ok {
        Ok(c.to_ascii_lowercase())
    } else {
        err(format!("color {c:?} must be #rrggbb"))
    }
}

fn validate_scene(scene: &Scene) -> Result<(), EngineError> {
    if scene.objects.len() > MAX_OBJECTS {
        return err(format!("scene is limited to {MAX_OBJECTS} objects"));
    }
    let mut ids = std::collections::HashSet::new();
    for o in &scene.objects {
        let ctx = |e: EngineError| EngineError {
            message: format!("object {}: {}", o.id, e.message),
            ..e
        };
        if !ids.insert(o.id) {
            return err(format!("duplicate object id {}", o.id));
        }
        if o.id >= scene.next_id {
            return err(format!("object id {} is not below next_id", o.id));
        }
        check_name(&o.name).map_err(ctx)?;
        let t = &o.transform;
        set_transform(
            &mut Transform::default(),
            &Some(t.translation),
            &Some(t.rotation),
            &Some(t.scale),
        )
        .map_err(ctx)?;
        let m = &o.material;
        set_material(
            &mut Material::default(),
            &Some(m.color.clone()),
            &Some(m.roughness),
            &Some(m.metalness),
        )
        .map_err(ctx)?;
        validate_mesh(&o.mesh).map_err(ctx)?;
    }
    Ok(())
}

fn validate_mesh(mesh: &Mesh) -> Result<(), EngineError> {
    if mesh.faces.len() > MAX_FACES {
        return err(format!("mesh exceeds {MAX_FACES} faces"));
    }
    for v in &mesh.vertices {
        check_vec(v, "vertex")?;
    }
    let n = mesh.vertices.len() as u32;
    for f in &mesh.faces {
        if f.len() < 3 || f.iter().any(|&i| i >= n) {
            return err("faces need at least 3 valid vertex indices");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Scene queries
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Bounds {
    pub min: Vec3,
    pub max: Vec3,
}

pub fn world_bounds(o: &Object) -> Option<Bounds> {
    let m = o.transform.matrix();
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for v in &o.mesh.vertices {
        let p = m.transform_point3(DVec3::from(*v)).to_array();
        for k in 0..3 {
            min[k] = min[k].min(p[k]);
            max[k] = max[k].max(p[k]);
        }
    }
    min[0].is_finite().then_some(Bounds { min, max })
}

/// Compact scene description without mesh data, for agents and the chat prompt.
pub fn context(scene: &Scene) -> serde_json::Value {
    let mut scene_min = [f64::INFINITY; 3];
    let mut scene_max = [f64::NEG_INFINITY; 3];
    let objects: Vec<_> = scene
        .objects
        .iter()
        .map(|o| {
            let bounds = world_bounds(o);
            if let Some(b) = &bounds {
                for k in 0..3 {
                    scene_min[k] = scene_min[k].min(b.min[k]);
                    scene_max[k] = scene_max[k].max(b.max[k]);
                }
            }
            serde_json::json!({
                "id": o.id,
                "name": o.name,
                "kind": o.kind,
                "transform": o.transform,
                "material": o.material,
                "vertex_count": o.mesh.vertices.len(),
                "face_count": o.mesh.faces.len(),
                "bounds": bounds,
            })
        })
        .collect();
    let bounds = scene_min[0].is_finite().then_some(Bounds {
        min: scene_min,
        max: scene_max,
    });
    serde_json::json!({
        "revision": scene.revision,
        "next_id": scene.next_id,
        "units": "meters, Y up, rotations in radians (XYZ euler)",
        "bounds": bounds,
        "objects": objects,
    })
}

pub fn export_obj(scene: &Scene) -> String {
    let mut out = String::from("# Exported from Tatara\n");
    let mut base = 1usize;
    for o in &scene.objects {
        let m = o.transform.matrix();
        let name: String = o
            .name
            .chars()
            .map(|c| if c.is_whitespace() { '_' } else { c })
            .collect();
        out.push_str(&format!("o {name}\n"));
        for v in &o.mesh.vertices {
            let p = m.transform_point3(DVec3::from(*v));
            out.push_str(&format!("v {:.6} {:.6} {:.6}\n", p.x, p.y, p.z));
        }
        // A mirrored transform flips winding; keep normals outward.
        let flip = m.determinant() < 0.0;
        for f in &o.mesh.faces {
            out.push('f');
            let mut idx: Vec<u32> = f.clone();
            if flip {
                idx.reverse();
            }
            for i in idx {
                out.push_str(&format!(" {}", base + i as usize));
            }
            out.push('\n');
        }
        base += o.mesh.vertices.len();
    }
    out
}

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

fn positive(v: f64, what: &str) -> Result<f64, EngineError> {
    if v.is_finite() && v > 0.0 && v < 1e4 {
        Ok(v)
    } else {
        err(format!("{what} must be positive"))
    }
}

fn seg(v: u32, lo: u32, hi: u32, what: &str) -> Result<u32, EngineError> {
    if (lo..=hi).contains(&v) {
        Ok(v)
    } else {
        err(format!("{what} must be between {lo} and {hi}"))
    }
}

pub fn build_primitive(p: &Primitive) -> Result<Mesh, EngineError> {
    Ok(match *p {
        Primitive::Cube { size } => cube(positive(size, "size")?),
        Primitive::Plane { size } => {
            let h = positive(size, "size")? / 2.0;
            Mesh {
                vertices: vec![[-h, 0.0, h], [h, 0.0, h], [h, 0.0, -h], [-h, 0.0, -h]],
                faces: vec![vec![0, 1, 2, 3]],
            }
        }
        Primitive::Sphere {
            radius,
            segments,
            rings,
        } => {
            let r = positive(radius, "radius")?;
            let segments = seg(segments, 3, 256, "segments")?;
            let rings = seg(rings, 2, 128, "rings")?;
            let profile: Vec<[f64; 2]> = (0..=rings)
                .map(|i| {
                    let a = std::f64::consts::PI * (i as f64 / rings as f64);
                    let radius = if i == 0 || i == rings {
                        0.0
                    } else {
                        r * a.sin()
                    };
                    [radius, -r * a.cos()]
                })
                .collect();
            lathe(&profile, segments)
        }
        Primitive::Cylinder {
            radius,
            radius_top,
            height,
            segments,
        } => {
            let r0 = positive(radius, "radius")?;
            let r1 = match radius_top {
                Some(0.0) => 0.0,
                Some(r) => positive(r, "radius_top")?,
                None => r0,
            };
            let h = positive(height, "height")? / 2.0;
            let segments = seg(segments, 3, 256, "segments")?;
            let mut profile = vec![[0.0, -h], [r0, -h], [r1, h]];
            if r1 > 0.0 {
                profile.push([0.0, h]);
            }
            lathe(&profile, segments)
        }
        Primitive::Torus {
            major_radius,
            minor_radius,
            major_segments,
            minor_segments,
        } => {
            let big = positive(major_radius, "major_radius")?;
            let small = positive(minor_radius, "minor_radius")?;
            if small >= big {
                return err("minor_radius must be smaller than major_radius");
            }
            torus(
                big,
                small,
                seg(major_segments, 3, 256, "major_segments")?,
                seg(minor_segments, 3, 128, "minor_segments")?,
            )
        }
        Primitive::Vessel {
            ref profile,
            thickness,
            segments,
            smooth,
        } => vessel(
            profile,
            positive(thickness, "thickness")?,
            seg(segments, 3, 256, "segments")?,
            smooth,
        )?,
    })
}

fn cube(size: f64) -> Mesh {
    let h = size / 2.0;
    let vertices = vec![
        [-h, -h, -h],
        [h, -h, -h],
        [h, h, -h],
        [-h, h, -h],
        [-h, -h, h],
        [h, -h, h],
        [h, h, h],
        [-h, h, h],
    ];
    let faces = vec![
        vec![4, 5, 6, 7], // +Z
        vec![1, 0, 3, 2], // -Z
        vec![5, 1, 2, 6], // +X
        vec![0, 4, 7, 3], // -X
        vec![7, 6, 2, 3], // +Y
        vec![0, 1, 5, 4], // -Y
    ];
    Mesh { vertices, faces }
}

/// Revolve a `[radius, height]` polyline around Y. Points with zero radius
/// become poles. Walking the profile with the surface's outside on the
/// right-hand side (e.g. upwards on an outer wall) yields outward normals.
pub fn lathe(profile: &[[f64; 2]], segments: u32) -> Mesh {
    let n = segments as usize;
    let mut vertices = Vec::new();
    let mut rings: Vec<Vec<u32>> = Vec::new();
    for &[r, y] in profile {
        let start = vertices.len() as u32;
        if r <= 1e-9 {
            vertices.push([0.0, y, 0.0]);
            rings.push(vec![start]);
        } else {
            for j in 0..n {
                let a = std::f64::consts::TAU * j as f64 / n as f64;
                vertices.push([r * a.cos(), y, r * a.sin()]);
            }
            rings.push((start..start + n as u32).collect());
        }
    }
    let mut faces = Vec::new();
    for w in rings.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        match (a.len(), b.len()) {
            (1, 1) => {}
            (1, _) => (0..n).for_each(|j| faces.push(vec![a[0], b[j], b[(j + 1) % n]])),
            (_, 1) => (0..n).for_each(|j| faces.push(vec![a[j], b[0], a[(j + 1) % n]])),
            _ => (0..n).for_each(|j| {
                let k = (j + 1) % n;
                faces.push(vec![a[j], b[j], b[k], a[k]]);
            }),
        }
    }
    Mesh { vertices, faces }
}

fn torus(big: f64, small: f64, nu: u32, nv: u32) -> Mesh {
    let (nu, nv) = (nu as usize, nv as usize);
    let mut vertices = Vec::with_capacity(nu * nv);
    for i in 0..nu {
        let u = std::f64::consts::TAU * i as f64 / nu as f64;
        for j in 0..nv {
            let v = std::f64::consts::TAU * j as f64 / nv as f64;
            let r = big + small * v.cos();
            vertices.push([r * u.cos(), small * v.sin(), r * u.sin()]);
        }
    }
    let idx = |i: usize, j: usize| ((i % nu) * nv + (j % nv)) as u32;
    let mut faces = Vec::with_capacity(nu * nv);
    for i in 0..nu {
        for j in 0..nv {
            faces.push(vec![
                idx(i, j),
                idx(i, j + 1),
                idx(i + 1, j + 1),
                idx(i + 1, j),
            ]);
        }
    }
    Mesh { vertices, faces }
}

fn vessel(
    profile: &[[f64; 2]],
    thickness: f64,
    segments: u32,
    smooth: bool,
) -> Result<Mesh, EngineError> {
    if profile.len() < 2 || profile.len() > 64 {
        return err("vessel profile needs 2-64 [radius, height] points");
    }
    for (i, &[r, y]) in profile.iter().enumerate() {
        if !r.is_finite() || !y.is_finite() || r <= 0.0 || r > 1e3 || y.abs() > 1e3 {
            return err("vessel profile radii must be positive and finite");
        }
        if i > 0 && y <= profile[i - 1][1] {
            return err("vessel profile heights must increase from foot to rim");
        }
    }
    let smoothed;
    let profile = if smooth && profile.len() > 2 {
        smoothed = smooth_profile(profile, thickness);
        &smoothed[..]
    } else {
        profile
    };
    let foot = profile[0][1];
    let floor = foot + thickness;
    if floor >= profile[profile.len() - 1][1] {
        return err("vessel is too short for its wall thickness");
    }
    let min_r = profile.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
    if thickness >= min_r {
        return err("thickness must be smaller than the narrowest radius");
    }
    // Outside: foot centre -> wall upwards. Inside: rim downwards -> floor centre.
    let mut full = vec![[0.0, foot]];
    full.extend_from_slice(profile);
    let mut inner: Vec<[f64; 2]> = Vec::new();
    for w in profile.windows(2).rev() {
        let ([r0, y0], [r1, y1]) = (w[0], w[1]);
        if inner.is_empty() {
            inner.push([r1 - thickness, y1]);
        }
        if y0 >= floor {
            inner.push([r0 - thickness, y0]);
        } else {
            let t = (floor - y0) / (y1 - y0);
            inner.push([r0 + (r1 - r0) * t - thickness, floor]);
            break;
        }
    }
    if inner.last().is_some_and(|p| p[1] > floor) {
        let r = inner.last().unwrap()[0];
        inner.push([r, floor]);
    }
    full.extend(inner);
    full.push([0.0, floor]);
    Ok(lathe(&full, segments))
}

/// Catmull-Rom spline through the profile points, kept valid for revolving:
/// heights stay increasing and radii never undershoot the thinnest input.
fn smooth_profile(points: &[[f64; 2]], thickness: f64) -> Vec<[f64; 2]> {
    const STEPS: usize = 4;
    let min_r = points.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min);
    let floor_r = min_r.max(thickness * 1.05);
    let at = |i: isize| points[i.clamp(0, points.len() as isize - 1) as usize];
    let mut out = vec![points[0]];
    for i in 0..points.len() as isize - 1 {
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        for s in 1..=STEPS {
            let t = s as f64 / STEPS as f64;
            let (t2, t3) = (t * t, t * t * t);
            let c = |k: usize| {
                0.5 * (2.0 * p1[k]
                    + (p2[k] - p0[k]) * t
                    + (2.0 * p0[k] - 5.0 * p1[k] + 4.0 * p2[k] - p3[k]) * t2
                    + (3.0 * p1[k] - p0[k] - 3.0 * p2[k] + p3[k]) * t3)
            };
            let prev = out[out.len() - 1][1];
            let y = c(1).clamp(p1[1], p2[1]).max(prev + 1e-6);
            out.push([c(0).max(floor_r), y]);
        }
    }
    out
}

fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: Vec3, s: f64) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

/// Newell normal of a polygon (not normalized).
pub fn face_normal(mesh: &Mesh, face: &[u32]) -> Vec3 {
    let mut n = [0.0; 3];
    for (k, &i) in face.iter().enumerate() {
        let a = mesh.vertices[i as usize];
        let b = mesh.vertices[face[(k + 1) % face.len()] as usize];
        n[0] += (a[1] - b[1]) * (a[2] + b[2]);
        n[1] += (a[2] - b[2]) * (a[0] + b[0]);
        n[2] += (a[0] - b[0]) * (a[1] + b[1]);
    }
    n
}

pub fn extrude(mesh: &mut Mesh, face: usize, distance: f64) -> Result<(), EngineError> {
    let Some(poly) = mesh.faces.get(face).cloned() else {
        return err(format!(
            "face {face} does not exist (mesh has {} faces)",
            mesh.faces.len()
        ));
    };
    if mesh.faces.len() + poly.len() > MAX_FACES {
        return err(format!("mesh would exceed {MAX_FACES} faces"));
    }
    let n = face_normal(mesh, &poly);
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len < 1e-12 {
        return err("cannot extrude a degenerate face");
    }
    let offset = scale(n, distance / len);
    let base = mesh.vertices.len() as u32;
    for &i in &poly {
        let v = add(mesh.vertices[i as usize], offset);
        mesh.vertices.push(v);
    }
    let k = poly.len();
    for e in 0..k {
        let (a, b) = (poly[e], poly[(e + 1) % k]);
        let (a2, b2) = (base + e as u32, base + ((e + 1) % k) as u32);
        mesh.faces.push(vec![a, b, b2, a2]);
    }
    mesh.faces[face] = (base..base + k as u32).collect();
    Ok(())
}

/// One level of Catmull-Clark subdivision. Boundaries use the crease rule.
pub fn catmull_clark(mesh: &Mesh) -> Mesh {
    let nv = mesh.vertices.len();
    let v = &mesh.vertices;
    let face_points: Vec<Vec3> = mesh
        .faces
        .iter()
        .map(|f| {
            scale(
                f.iter().fold([0.0; 3], |acc, &i| add(acc, v[i as usize])),
                1.0 / f.len() as f64,
            )
        })
        .collect();

    let mut edge_index: HashMap<(u32, u32), usize> = HashMap::new();
    let mut edges: Vec<(u32, u32)> = Vec::new();
    let mut edge_faces: Vec<Vec<usize>> = Vec::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        for k in 0..f.len() {
            let (a, b) = (f[k], f[(k + 1) % f.len()]);
            let key = (a.min(b), a.max(b));
            let e = *edge_index.entry(key).or_insert_with(|| {
                edges.push(key);
                edge_faces.push(Vec::new());
                edges.len() - 1
            });
            edge_faces[e].push(fi);
        }
    }

    let edge_points: Vec<Vec3> = edges
        .iter()
        .zip(&edge_faces)
        .map(|(&(a, b), fs)| {
            let mid = add(v[a as usize], v[b as usize]);
            if fs.len() == 2 {
                scale(add(add(mid, face_points[fs[0]]), face_points[fs[1]]), 0.25)
            } else {
                scale(mid, 0.5)
            }
        })
        .collect();

    let mut v_faces: Vec<Vec<usize>> = vec![Vec::new(); nv];
    for (fi, f) in mesh.faces.iter().enumerate() {
        for &i in f {
            v_faces[i as usize].push(fi);
        }
    }
    let mut v_edges: Vec<Vec<usize>> = vec![Vec::new(); nv];
    for (e, &(a, b)) in edges.iter().enumerate() {
        v_edges[a as usize].push(e);
        v_edges[b as usize].push(e);
    }

    let mut vertices: Vec<Vec3> = Vec::with_capacity(nv + face_points.len() + edge_points.len());
    for i in 0..nv {
        let p = v[i];
        let fs = &v_faces[i];
        let es = &v_edges[i];
        if fs.is_empty() {
            vertices.push(p);
            continue;
        }
        let boundary: Vec<usize> = es
            .iter()
            .copied()
            .filter(|&e| edge_faces[e].len() != 2)
            .collect();
        if boundary.is_empty() {
            let n = fs.len() as f64;
            let f = scale(
                fs.iter()
                    .fold([0.0; 3], |acc, &fi| add(acc, face_points[fi])),
                1.0 / n,
            );
            let r = scale(
                es.iter().fold([0.0; 3], |acc, &e| {
                    let (a, b) = edges[e];
                    add(acc, scale(add(v[a as usize], v[b as usize]), 0.5))
                }),
                1.0 / es.len() as f64,
            );
            vertices.push(scale(
                add(add(f, scale(r, 2.0)), scale(p, n - 3.0)),
                1.0 / n,
            ));
        } else if boundary.len() == 2 {
            let other = |e: usize| {
                let (a, b) = edges[e];
                v[if a as usize == i { b } else { a } as usize]
            };
            let sum = add(add(scale(p, 6.0), other(boundary[0])), other(boundary[1]));
            vertices.push(scale(sum, 1.0 / 8.0));
        } else {
            vertices.push(p);
        }
    }
    let face_base = vertices.len() as u32;
    vertices.extend_from_slice(&face_points);
    let edge_base = vertices.len() as u32;
    vertices.extend_from_slice(&edge_points);

    let edge_of = |a: u32, b: u32| edge_base + edge_index[&(a.min(b), a.max(b))] as u32;
    let mut faces = Vec::with_capacity(mesh.faces.iter().map(Vec::len).sum());
    for (fi, f) in mesh.faces.iter().enumerate() {
        let k = f.len();
        for c in 0..k {
            let prev = f[(c + k - 1) % k];
            let cur = f[c];
            let next = f[(c + 1) % k];
            faces.push(vec![
                cur,
                edge_of(cur, next),
                face_base + fi as u32,
                edge_of(prev, cur),
            ]);
        }
    }
    Mesh { vertices, faces }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Signed volume; positive for closed meshes with outward normals.
    fn volume(m: &Mesh) -> f64 {
        let mut vol = 0.0;
        for f in &m.faces {
            let a = m.vertices[f[0] as usize];
            for k in 1..f.len() - 1 {
                let b = m.vertices[f[k] as usize];
                let c = m.vertices[f[k + 1] as usize];
                vol += a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                    + a[2] * (b[0] * c[1] - b[1] * c[0]);
            }
        }
        vol / 6.0
    }

    /// Every edge is shared by exactly two faces, in opposite directions.
    fn assert_closed(m: &Mesh) {
        let mut directed = HashMap::new();
        for f in &m.faces {
            for k in 0..f.len() {
                *directed.entry((f[k], f[(k + 1) % f.len()])).or_insert(0) += 1;
            }
        }
        for (&(a, b), &count) in &directed {
            assert_eq!(count, 1, "edge {a}->{b} used {count} times");
            assert!(directed.contains_key(&(b, a)), "edge {a}->{b} has no twin");
        }
    }

    fn batch(json: serde_json::Value) -> CommandBatch {
        serde_json::from_value(json).expect("valid batch")
    }

    #[test]
    fn primitives_are_closed_with_outward_normals() {
        let prims = [
            Primitive::Cube { size: 2.0 },
            Primitive::Sphere {
                radius: 1.0,
                segments: 24,
                rings: 12,
            },
            Primitive::Cylinder {
                radius: 1.0,
                radius_top: None,
                height: 2.0,
                segments: 16,
            },
            Primitive::Cylinder {
                radius: 1.0,
                radius_top: Some(0.0),
                height: 2.0,
                segments: 16,
            },
            Primitive::Torus {
                major_radius: 1.0,
                minor_radius: 0.25,
                major_segments: 32,
                minor_segments: 12,
            },
            Primitive::Vessel {
                profile: vec![[0.3, 0.0], [0.5, 0.4], [0.2, 0.9], [0.25, 1.0]],
                thickness: 0.05,
                segments: 24,
                smooth: false,
            },
            Primitive::Vessel {
                profile: vec![[0.3, 0.0], [0.5, 0.4], [0.2, 0.9], [0.25, 1.0]],
                thickness: 0.05,
                segments: 24,
                smooth: true,
            },
        ];
        for p in prims {
            let m = build_primitive(&p).unwrap();
            assert_closed(&m);
            assert!(volume(&m) > 0.0, "{} has inward normals", p.kind());
        }
        let cube = build_primitive(&Primitive::Cube { size: 2.0 }).unwrap();
        assert!((volume(&cube) - 8.0).abs() < 1e-9);
    }

    #[test]
    fn vessel_is_hollow() {
        let solid_r = 0.5;
        let m = build_primitive(&Primitive::Vessel {
            profile: vec![[solid_r, 0.0], [solid_r, 1.0]],
            thickness: 0.1,
            segments: 64,
            smooth: true,
        })
        .unwrap();
        let shell = std::f64::consts::PI * (solid_r * solid_r * 1.0 - 0.4 * 0.4 * 0.9);
        assert!(
            (volume(&m) - shell).abs() / shell < 0.01,
            "volume {} vs {shell}",
            volume(&m)
        );
    }

    #[test]
    fn extrude_keeps_mesh_closed_and_adds_volume() {
        let mut m = cube(2.0);
        extrude(&mut m, 4, 1.0).unwrap();
        assert_closed(&m);
        assert!((volume(&m) - 12.0).abs() < 1e-9);
        assert_eq!(m.faces.len(), 10);
        extrude(&mut m, 4, -0.5).unwrap();
        assert_closed(&m);
        assert!((volume(&m) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn subdivision_smooths_a_closed_mesh() {
        let mut m = cube(2.0);
        for _ in 0..2 {
            m = catmull_clark(&m);
            assert_closed(&m);
        }
        assert_eq!(m.faces.len(), 96);
        let v = volume(&m);
        assert!(v > 2.0 && v < 8.0, "volume {v}");
        let plane = build_primitive(&Primitive::Plane { size: 1.0 }).unwrap();
        assert_eq!(catmull_clark(&plane).faces.len(), 4);
    }

    #[test]
    fn rotation_order_matches_three_js_xyz() {
        // three.js 'XYZ' Euler builds R = Rx * Ry * Rz.
        let (a, b, c) = (0.3, -0.7, 1.1);
        let t = Transform {
            rotation: [a, b, c],
            ..Default::default()
        };
        let expected =
            DMat4::from_rotation_x(a) * DMat4::from_rotation_y(b) * DMat4::from_rotation_z(c);
        assert!(t.matrix().abs_diff_eq(expected, 1e-12));
    }

    #[test]
    fn batches_are_atomic_and_undoable() {
        let mut ed = Editor::new();
        let r = ed
            .apply(&batch(serde_json::json!({"commands": [
                {"op": "add", "name": "Vase", "primitive": {"kind": "vessel", "profile": [[0.2, 0.0], [0.3, 0.5]]}},
                {"op": "material", "id": "Vase", "color": "#AA3300"},
                {"op": "duplicate", "id": 1, "offset": [1.0, 0.0, 0.0]}
            ]})))
            .unwrap();
        assert_eq!(r.created, vec![1, 2]);
        assert_eq!(ed.scene().objects[1].name, "Vase.001");
        assert_eq!(ed.scene().objects[1].material.color, "#aa3300");
        assert_eq!(ed.scene().revision, 1);

        let before = ed.scene().clone();
        let e = ed
            .apply(&batch(serde_json::json!({"commands": [
                {"op": "delete", "id": 1},
                {"op": "transform", "id": 99, "translation": [0, 0, 0]}
            ]})))
            .unwrap_err();
        assert_eq!(e.command_index, Some(1));
        assert_eq!(ed.scene(), &before);

        let stale = ed
            .apply(&CommandBatch {
                commands: vec![Command::Clear {}],
                expected_revision: Some(0),
            })
            .unwrap_err();
        assert!(stale.stale);

        ed.undo().unwrap();
        assert!(ed.scene().objects.is_empty());
        assert_eq!(ed.scene().revision, 2);
        ed.redo().unwrap();
        assert_eq!(ed.scene().objects.len(), 2);
        assert_eq!(ed.scene().revision, 3);
    }

    #[test]
    fn rejects_unknown_fields_and_bad_values() {
        let unknown = serde_json::from_value::<CommandBatch>(
            serde_json::json!({"commands": [{"op": "delete", "id": 1, "bogus": 1}]}),
        );
        assert!(unknown.is_err());
        let mut ed = Editor::new();
        for bad in [
            serde_json::json!({"op": "add", "primitive": {"kind": "cube"}, "color": "red"}),
            serde_json::json!({"op": "add", "primitive": {"kind": "cube", "size": -1}}),
            serde_json::json!({"op": "add", "primitive": {"kind": "cube"}, "scale": [1, 0, 1]}),
            serde_json::json!({"op": "add", "primitive": {"kind": "vessel", "profile": [[0.2, 0.5], [0.3, 0.1]]}}),
        ] {
            assert!(
                ed.apply(&batch(serde_json::json!({"commands": [bad]})))
                    .is_err()
            );
        }
        assert_eq!(ed.scene().revision, 0);
    }

    #[test]
    fn array_and_export() {
        let mut ed = Editor::new();
        ed.apply(&batch(serde_json::json!({"commands": [
            {"op": "add", "primitive": {"kind": "cube"}},
            {"op": "array", "id": 1, "count": 3, "offset": [2, 0, 0]}
        ]})))
        .unwrap();
        let s = ed.scene();
        assert_eq!(s.objects.len(), 4);
        assert_eq!(s.objects[3].transform.translation, [6.0, 0.0, 0.0]);
        assert_eq!(s.objects[3].name, "Cube.003");
        let obj = export_obj(s);
        assert_eq!(obj.lines().filter(|l| l.starts_with("v ")).count(), 32);
        assert!(obj.contains("f 29 30 31 32"));
        let ctx = context(s);
        assert_eq!(ctx["bounds"]["max"][0], 6.5);
    }

    #[test]
    fn load_validates() {
        let mut ed = Editor::new();
        let mut scene = Scene::default();
        scene.objects.push(Object {
            id: 5,
            name: "x".into(),
            kind: "cube".into(),
            transform: Transform::default(),
            material: Material::default(),
            mesh: cube(1.0),
        });
        assert!(ed.load(scene.clone()).is_err(), "id must be below next_id");
        scene.next_id = 6;
        ed.load(scene).unwrap();
        assert_eq!(ed.scene().objects.len(), 1);
    }

    #[test]
    fn schema_lists_operations() {
        let s = command_schema().to_string();
        for op in ["add", "extrude", "subdivide", "vessel"] {
            assert!(s.contains(op));
        }
    }
}
