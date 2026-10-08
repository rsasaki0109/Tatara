//! Scene model, procedural geometry, modeling commands and undo history.
//!
//! Every edit goes through [`Editor::apply`]. A batch is applied to a copy of
//! the scene and only committed when every command succeeds, so an invalid
//! batch never leaves a half-edited scene behind. One successful batch is one
//! undo step.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use glam::{DMat4, DQuat, DVec3, EulerRot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::anim::{self, Animation, Interpolation, KeyValue, Property, Track};
use crate::assembly::{self, Layout, Side, Template};
use crate::edit;
use crate::modifiers::{self, Axis, Modifier};
use crate::sculpt::{self, Brush};

pub type Vec3 = [f64; 3];

pub const MAX_OBJECTS: usize = 2_000;
pub const MAX_FACES: usize = 250_000;
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
    /// Emitted light colour as `#rrggbb` (`#000000` = none).
    #[serde(default = "d_emissive")]
    pub emissive: String,
    /// Multiplier for `emissive`, 0-20. Above 1 the surface glows.
    #[serde(default = "d_one")]
    pub emissive_strength: f64,
    /// Alpha, 0 (invisible) to 1 (opaque).
    #[serde(default = "d_one")]
    pub opacity: f64,
    /// How much light passes through, 0-1 (1 = clear glass).
    #[serde(default)]
    pub transmission: f64,
}

fn d_emissive() -> String {
    "#000000".into()
}
fn d_one() -> f64 {
    1.0
}

impl Default for Material {
    fn default() -> Self {
        Self {
            color: "#c9c3b8".into(),
            roughness: 0.55,
            metalness: 0.0,
            emissive: d_emissive(),
            emissive_strength: 1.0,
            opacity: 1.0,
            transmission: 0.0,
        }
    }
}

/// A starting point for a material; any field given alongside it wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaterialPreset {
    /// Glazed white stoneware.
    Ceramic,
    /// Matte unfired clay.
    Clay,
    /// Satin plastic.
    Plastic,
    /// Soft black rubber.
    Rubber,
    /// Clear glass.
    Glass,
    /// Frosted glass.
    Frosted,
    Gold,
    Copper,
    Chrome,
    /// Brushed steel.
    Steel,
    /// Polished green stone.
    Jade,
    /// Glowing tube light; its glow follows `color` unless `emissive` is set.
    Neon,
}

impl MaterialPreset {
    pub fn material(self) -> Material {
        use MaterialPreset::*;
        let (color, roughness, metalness) = match self {
            Ceramic => ("#ece6da", 0.18, 0.0),
            Clay => ("#b8714f", 0.92, 0.0),
            Plastic => ("#3f7fd8", 0.38, 0.0),
            Rubber => ("#26272b", 0.85, 0.0),
            Glass => ("#f4f8fb", 0.04, 0.0),
            Frosted => ("#eef3f6", 0.42, 0.0),
            Gold => ("#e8b04a", 0.22, 1.0),
            Copper => ("#d9825b", 0.3, 1.0),
            Chrome => ("#e9ecef", 0.04, 1.0),
            Steel => ("#a9adb3", 0.42, 1.0),
            Jade => ("#4f9d7a", 0.16, 0.0),
            Neon => ("#ff4fd8", 0.3, 0.0),
        };
        let mut m = Material {
            color: color.into(),
            roughness,
            metalness,
            ..Material::default()
        };
        match self {
            Glass => m.transmission = 1.0,
            Frosted => m.transmission = 0.85,
            Jade => m.transmission = 0.25,
            Neon => {
                m.emissive = m.color.clone();
                m.emissive_strength = 2.0;
            }
            _ => {}
        }
        m
    }
}

/// Material fields of a command; unset ones keep their current value.
#[derive(Debug, Default, Clone, Copy)]
pub struct MaterialEdit<'a> {
    pub preset: Option<MaterialPreset>,
    pub color: Option<&'a str>,
    pub roughness: Option<f64>,
    pub metalness: Option<f64>,
    pub emissive: Option<&'a str>,
    pub emissive_strength: Option<f64>,
    pub opacity: Option<f64>,
    pub transmission: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Object {
    pub id: u64,
    pub name: String,
    /// The primitive this object was generated from (informational).
    pub kind: String,
    pub transform: Transform,
    pub material: Material,
    /// Editable base mesh. Modifiers are evaluated on top of it.
    pub mesh: Mesh,
    /// Non-destructive modifier stack, evaluated in order.
    #[serde(default)]
    pub modifiers: Vec<Modifier>,
    /// Keyframe tracks; animated properties override the static values.
    #[serde(default)]
    pub tracks: Vec<Track>,
    /// Smooth shading: normals blend across every edge instead of splitting
    /// at sharp creases. On by default for quadspheres (sculpting).
    #[serde(default)]
    pub smooth: bool,
    /// The assembly this object is a part of (from `build`). Commands that
    /// take an `id` also accept a group name and move its parts together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Scene {
    pub objects: Vec<Object>,
    pub next_id: u64,
    pub revision: u64,
    #[serde(default)]
    pub animation: Animation,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            objects: Vec::new(),
            next_id: 1,
            revision: 0,
            animation: Animation::default(),
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
fn d_gap() -> f64 {
    0.1
}
fn d_quad_level() -> u32 {
    4
}
fn d_levels() -> u32 {
    1
}
fn d_half() -> f64 {
    0.5
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
    /// Sphere of even quads (a subdivided cube pushed out to `radius`): the
    /// best start for sculpting. `level` 1-6 gives 24 to 24,576 faces.
    Quadsphere {
        #[serde(default = "d_radius")]
        radius: f64,
        #[serde(default = "d_quad_level")]
        level: u32,
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
            Primitive::Quadsphere { .. } => "quadsphere",
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
        /// `#rrggbb` light the surface emits.
        #[serde(default)]
        emissive: Option<String>,
        #[serde(default)]
        emissive_strength: Option<f64>,
        #[serde(default)]
        opacity: Option<f64>,
        #[serde(default)]
        transmission: Option<f64>,
        /// Start from a preset; other material fields override it.
        #[serde(default)]
        preset: Option<MaterialPreset>,
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
    /// Set any of the material properties, optionally starting from a preset.
    Material {
        id: ObjRef,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        roughness: Option<f64>,
        #[serde(default)]
        metalness: Option<f64>,
        /// `#rrggbb` light the surface emits.
        #[serde(default)]
        emissive: Option<String>,
        #[serde(default)]
        emissive_strength: Option<f64>,
        #[serde(default)]
        opacity: Option<f64>,
        #[serde(default)]
        transmission: Option<f64>,
        /// Start from a preset; other material fields override it.
        #[serde(default)]
        preset: Option<MaterialPreset>,
    },
    Rename {
        id: ObjRef,
        name: String,
    },
    /// Move an object (or group) straight down until it rests on the floor
    /// or on what is beneath it (or up, out of whatever it has sunk into).
    Drop {
        id: ObjRef,
    },
    /// Build a parametric assembly (furniture) from primitives, at real-world
    /// size with its front facing +Z. The parts share a group named `name`
    /// (default: the template, numbered: "Chair", "Chair 2", ...), so `move`,
    /// `place`, `arrange`, `drop` and `delete` act on the whole piece.
    Build {
        template: Template,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        translation: Option<Vec3>,
        /// Turn about the vertical axis, radians.
        #[serde(default)]
        rotation_y: Option<f64>,
        /// Uniform size factor (0.05-20).
        #[serde(default)]
        scale: Option<f64>,
        /// `#rrggbb` for the main surfaces.
        #[serde(default)]
        color: Option<String>,
    },
    /// Put an object or group on another (`on`; `at` is [u, v], fractions of
    /// its top from the -X/-Z corner, default the middle) or beside it
    /// (`beside`, `side`, `gap` metres), then settle it. No coordinates needed.
    Place {
        id: ObjRef,
        #[serde(default)]
        on: Option<ObjRef>,
        #[serde(default)]
        at: Option<[f64; 2]>,
        #[serde(default)]
        beside: Option<ObjRef>,
        #[serde(default)]
        side: Option<Side>,
        #[serde(default = "d_gap")]
        gap: f64,
    },
    /// Lay objects or groups out in a row (along X), a grid, or a circle
    /// around `around` (or `center` [x, z]) with each turned to face the
    /// middle, `spacing` metres apart, then settle each one.
    Arrange {
        ids: Vec<ObjRef>,
        layout: Layout,
        #[serde(default)]
        around: Option<ObjRef>,
        #[serde(default)]
        center: Option<[f64; 2]>,
        #[serde(default = "d_gap")]
        spacing: f64,
        #[serde(default)]
        radius: Option<f64>,
    },
    /// Move an object or group rigidly by `offset` and/or turn it by
    /// `rotate_y` radians about its centre.
    Move {
        id: ObjRef,
        #[serde(default)]
        offset: Option<Vec3>,
        #[serde(default)]
        rotate_y: Option<f64>,
    },
    /// Smooth shading on or off (off splits normals at sharp creases).
    Shade {
        id: ObjRef,
        smooth: bool,
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
    /// Move a polygon's corners toward its centre by `fraction` (0-1),
    /// adding a ring of quads. Combine with `extrude` for panels and bosses.
    Inset {
        id: ObjRef,
        face: usize,
        fraction: f64,
    },
    /// Create an object from explicit geometry (used by glTF import).
    /// `faces` are counter-clockwise vertex loops with outward normals.
    AddMesh {
        #[serde(default)]
        name: Option<String>,
        vertices: Vec<Vec3>,
        faces: Vec<Vec<u32>>,
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
        /// `#rrggbb` light the surface emits.
        #[serde(default)]
        emissive: Option<String>,
        #[serde(default)]
        emissive_strength: Option<f64>,
        #[serde(default)]
        opacity: Option<f64>,
        #[serde(default)]
        transmission: Option<f64>,
        /// Start from a preset; other material fields override it.
        #[serde(default)]
        preset: Option<MaterialPreset>,
    },
    /// Move base-mesh vertices by `offset` (object space).
    MoveVertices {
        id: ObjRef,
        vertices: Vec<u32>,
        offset: Vec3,
    },
    /// Chamfer edges given as `[a, b]` vertex pairs, or every edge when
    /// `edges` is omitted. Each touched edge must be longer than `2 * width`.
    Bevel {
        id: ObjRef,
        #[serde(default)]
        edges: Option<Vec<[u32; 2]>>,
        width: f64,
    },
    /// Cut a new edge loop through the ring of quads crossing `edge`.
    LoopCut {
        id: ObjRef,
        edge: [u32; 2],
        #[serde(default = "d_half")]
        fraction: f64,
    },
    /// Sculpt the base mesh with one brush stroke. `points` is the stroke path
    /// in object space; dabs land every `radius / 5` along it. `strength` is
    /// 0-1 (one draw dab at full strength rises `0.15 * radius`). `invert`
    /// carves instead of raising; `grab` moves the region under the first
    /// point by `offset`; `symmetry` mirrors every dab across an axis.
    Sculpt {
        id: ObjRef,
        brush: Brush,
        points: Vec<Vec3>,
        radius: f64,
        #[serde(default = "d_half")]
        strength: f64,
        #[serde(default)]
        invert: bool,
        #[serde(default)]
        offset: Option<Vec3>,
        #[serde(default)]
        symmetry: Option<Axis>,
    },
    /// Append a modifier, or insert it at `index`.
    AddModifier {
        id: ObjRef,
        modifier: Modifier,
        #[serde(default)]
        index: Option<usize>,
    },
    /// Replace the modifier at `index`.
    SetModifier {
        id: ObjRef,
        index: usize,
        modifier: Modifier,
    },
    RemoveModifier {
        id: ObjRef,
        index: usize,
    },
    /// Bake the modifier stack into the base mesh and clear it.
    ApplyModifiers {
        id: ObjRef,
    },
    /// Key a property at `frame`. Without `value`, keys the property's
    /// current value at that frame. Colours take `#rrggbb`.
    SetKeyframe {
        id: ObjRef,
        property: Property,
        frame: f64,
        #[serde(default)]
        value: Option<KeyValue>,
        #[serde(default)]
        interpolation: Option<Interpolation>,
    },
    /// Remove the keys at `frame` (one property, or all of them).
    DeleteKeyframe {
        id: ObjRef,
        frame: f64,
        #[serde(default)]
        property: Option<Property>,
    },
    /// Remove an object's animation (one property, or all of it).
    ClearAnimation {
        id: ObjRef,
        #[serde(default)]
        property: Option<Property>,
    },
    /// Set the playback range and frame rate.
    SetAnimation {
        #[serde(default)]
        fps: Option<f64>,
        #[serde(default)]
        start: Option<f64>,
        #[serde(default)]
        end: Option<f64>,
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

impl EngineError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            command_index: None,
            stale: false,
        }
    }
}

fn err<T>(message: impl Into<String>) -> Result<T, EngineError> {
    Err(EngineError::new(message))
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub revision: u64,
    pub created: Vec<u64>,
}

// ---------------------------------------------------------------------------
// Editor (scene + history)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Evaluated {
    fingerprint: u64,
    mesh: Arc<Mesh>,
}

#[derive(Debug, Default)]
pub struct Editor {
    scene: Scene,
    undo: Vec<Scene>,
    redo: Vec<Scene>,
    /// Evaluated meshes for objects with modifiers, reused while unchanged.
    evaluated: HashMap<u64, Evaluated>,
}

fn fingerprint(o: &Object) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for v in &o.mesh.vertices {
        v.map(f64::to_bits).hash(&mut h);
    }
    o.mesh.faces.hash(&mut h);
    serde_json::to_string(&o.modifiers)
        .unwrap_or_default()
        .hash(&mut h);
    h.finish()
}

impl Editor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// The mesh an object displays: its base mesh run through its modifiers.
    pub fn evaluated<'a>(&'a self, o: &'a Object) -> &'a Mesh {
        match self.evaluated.get(&o.id) {
            Some(e) if !o.modifiers.is_empty() => &e.mesh,
            _ => &o.mesh,
        }
    }

    /// Evaluate every modifier stack in `scene`, reusing unchanged results.
    fn evaluate_all(&self, scene: &Scene) -> Result<HashMap<u64, Evaluated>, EngineError> {
        let mut out = HashMap::new();
        for o in scene.objects.iter().filter(|o| !o.modifiers.is_empty()) {
            let fp = fingerprint(o);
            let entry =
                match self.evaluated.get(&o.id) {
                    Some(e) if e.fingerprint == fp => e.clone(),
                    _ => Evaluated {
                        fingerprint: fp,
                        mesh: Arc::new(modifiers::evaluate(&o.mesh, &o.modifiers).map_err(
                            |e| EngineError::new(format!("{:?}: {}", o.name, e.message)),
                        )?),
                    },
                };
            out.insert(o.id, entry);
        }
        Ok(out)
    }

    fn refresh_evaluated(&mut self) {
        // Scenes in history were valid when committed, so this cannot fail.
        self.evaluated = self.evaluate_all(&self.scene).unwrap_or_default();
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
        let evaluated = self.evaluate_all(&next)?;
        self.commit(next);
        self.evaluated = evaluated;
        Ok(ApplyResult {
            revision: self.scene.revision,
            created,
        })
    }

    /// Replace the whole scene (e.g. opening a file). Undoable.
    pub fn load(&mut self, scene: Scene) -> Result<u64, EngineError> {
        validate_scene(&scene)?;
        let evaluated = self.evaluate_all(&scene)?;
        self.commit(scene);
        self.evaluated = evaluated;
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
        self.refresh_evaluated();
        Some(self.scene.revision)
    }

    pub fn redo(&mut self) -> Option<u64> {
        let mut next = self.redo.pop()?;
        next.revision = self.scene.revision + 1;
        let current = std::mem::replace(&mut self.scene, next);
        self.undo.push(current);
        self.refresh_evaluated();
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
            emissive,
            emissive_strength,
            opacity,
            transmission,
            preset,
        } => {
            let edit = MaterialEdit {
                preset: *preset,
                color: color.as_deref(),
                roughness: *roughness,
                metalness: *metalness,
                emissive: emissive.as_deref(),
                emissive_strength: *emissive_strength,
                opacity: *opacity,
                transmission: *transmission,
            };
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
            set_material(&mut material, &edit)?;
            scene.objects.push(Object {
                id,
                name,
                kind: kind.into(),
                transform,
                material,
                mesh,
                modifiers: Vec::new(),
                tracks: Vec::new(),
                smooth: kind == "quadsphere",
                group: None,
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
            emissive,
            emissive_strength,
            opacity,
            transmission,
            preset,
        } => {
            let edit = MaterialEdit {
                preset: *preset,
                color: color.as_deref(),
                roughness: *roughness,
                metalness: *metalness,
                emissive: emissive.as_deref(),
                emissive_strength: *emissive_strength,
                opacity: *opacity,
                transmission: *transmission,
            };
            let i = resolve(scene, id)?;
            set_material(&mut scene.objects[i].material, &edit)?;
        }
        Command::Rename { id, name } => {
            let i = resolve(scene, id)?;
            scene.objects[i].name = check_name(name)?;
        }
        Command::Drop { id } => {
            let idx = assembly::targets(scene, id)?;
            if idx.iter().any(|&i| {
                scene.objects[i]
                    .tracks
                    .iter()
                    .any(|t| t.property == Property::Translation)
            }) {
                return err(
                    "drop moves the static position; this object's translation is animated",
                );
            }
            let ids: Vec<u64> = idx.iter().map(|&i| scene.objects[i].id).collect();
            let solids = crate::inspect::scene_solids(scene)?;
            let dy = crate::inspect::settle(&solids, &ids)?;
            assembly::translate(scene, &idx, DVec3::Y * dy);
        }
        Command::Build {
            template,
            name,
            translation,
            rotation_y,
            scale,
            color,
        } => {
            let parts = assembly::parts(*template);
            if scene.objects.len() + parts.len() > MAX_OBJECTS {
                return err(format!("scene is limited to {MAX_OBJECTS} objects"));
            }
            let base = match name {
                Some(n) => check_name(n)?,
                None => template.label().to_string(),
            };
            let taken = |n: &str| {
                scene
                    .objects
                    .iter()
                    .any(|o| o.name == n || o.group.as_deref() == Some(n))
            };
            let group = if !taken(&base) {
                base.clone()
            } else if name.is_some() {
                return err(format!("{base:?} is already used"));
            } else {
                (2..)
                    .map(|k| format!("{base} {k}"))
                    .find(|n| !taken(n))
                    .unwrap()
            };
            let size = scale.unwrap_or(1.0);
            if !(0.05..=20.0).contains(&size) {
                return err("scale must be between 0.05 and 20");
            }
            let origin = translation.unwrap_or([0.0; 3]);
            check_vec(&origin, "translation")?;
            let turn = DQuat::from_rotation_y(rotation_y.unwrap_or(0.0));
            let tint = color.as_deref().map(check_color).transpose()?;
            for p in parts {
                let mesh = build_primitive(&p.primitive)?;
                let at = DVec3::from(origin) + turn * (DVec3::from(p.at) * size);
                let [rx, ry, rz] = p.rotation;
                let (x, y, z) =
                    (turn * DQuat::from_euler(EulerRot::XYZ, rx, ry, rz)).to_euler(EulerRot::XYZ);
                let mut material = p.material;
                if let (Some(c), true) = (&tint, p.body) {
                    material.color = c.clone();
                }
                let id = scene.next_id;
                scene.objects.push(Object {
                    id,
                    name: check_name(&format!("{group} {}", p.name))?,
                    kind: p.primitive.kind().into(),
                    transform: Transform {
                        translation: at.to_array(),
                        rotation: [x, y, z],
                        scale: p.scale.map(|v| v * size),
                    },
                    material,
                    mesh,
                    modifiers: Vec::new(),
                    tracks: Vec::new(),
                    smooth: p.smooth,
                    group: Some(group.clone()),
                });
                scene.next_id += 1;
                created.push(id);
            }
        }
        Command::Place {
            id,
            on,
            at,
            beside,
            side,
            gap,
        } => assembly::place(scene, id, on.as_ref(), *at, beside.as_ref(), *side, *gap)?,
        Command::Arrange {
            ids,
            layout,
            around,
            center,
            spacing,
            radius,
        } => assembly::arrange(
            scene,
            ids,
            *layout,
            around.as_ref(),
            *center,
            *spacing,
            *radius,
        )?,
        Command::Move {
            id,
            offset,
            rotate_y,
        } => {
            let idx = assembly::targets(scene, id)?;
            if let Some(a) = rotate_y {
                if !a.is_finite() {
                    return err("rotate_y must be finite");
                }
                let solids = crate::inspect::scene_solids(scene)?;
                let ids: Vec<u64> = idx.iter().map(|&i| scene.objects[i].id).collect();
                let pivot = crate::inspect::bounds(&solids, &ids)
                    .map_or(DVec3::ZERO, |(lo, hi)| (lo + hi) * 0.5);
                assembly::turn(scene, &idx, pivot, *a);
            }
            if let Some(o) = offset {
                check_vec(o, "offset")?;
                assembly::translate(scene, &idx, DVec3::from(*o));
            }
        }
        Command::Shade { id, smooth } => {
            let i = resolve(scene, id)?;
            scene.objects[i].smooth = *smooth;
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
            let mut idx = assembly::targets(scene, id)?;
            idx.sort_unstable();
            for i in idx.into_iter().rev() {
                scene.objects.remove(i);
            }
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
        Command::Inset { id, face, fraction } => {
            let i = resolve(scene, id)?;
            if !(fraction.is_finite() && *fraction > 0.0 && *fraction < 1.0) {
                return err("fraction must be between 0 and 1 (exclusive)");
            }
            inset(&mut scene.objects[i].mesh, *face, *fraction)?;
        }
        Command::MoveVertices {
            id,
            vertices,
            offset,
        } => {
            let i = resolve(scene, id)?;
            edit::move_vertices(&mut scene.objects[i].mesh, vertices, *offset)?;
        }
        Command::Bevel { id, edges, width } => {
            let i = resolve(scene, id)?;
            edit::bevel(&mut scene.objects[i].mesh, edges.as_deref(), *width)?;
        }
        Command::LoopCut { id, edge, fraction } => {
            let i = resolve(scene, id)?;
            edit::loop_cut(&mut scene.objects[i].mesh, *edge, *fraction)?;
        }
        Command::Sculpt {
            id,
            brush,
            points,
            radius,
            strength,
            invert,
            offset,
            symmetry,
        } => {
            let i = resolve(scene, id)?;
            sculpt::sculpt(
                &mut scene.objects[i].mesh,
                &sculpt::Stroke {
                    brush: *brush,
                    points,
                    radius: *radius,
                    strength: *strength,
                    invert: *invert,
                    offset: *offset,
                    symmetry: *symmetry,
                },
            )?;
        }
        Command::AddMesh {
            name,
            vertices,
            faces,
            translation,
            rotation,
            scale,
            color,
            roughness,
            metalness,
            emissive,
            emissive_strength,
            opacity,
            transmission,
            preset,
        } => {
            let edit = MaterialEdit {
                preset: *preset,
                color: color.as_deref(),
                roughness: *roughness,
                metalness: *metalness,
                emissive: emissive.as_deref(),
                emissive_strength: *emissive_strength,
                opacity: *opacity,
                transmission: *transmission,
            };
            if scene.objects.len() >= MAX_OBJECTS {
                return err(format!("scene is limited to {MAX_OBJECTS} objects"));
            }
            let mesh = Mesh {
                vertices: vertices.clone(),
                faces: faces.clone(),
            };
            if mesh.faces.is_empty() {
                return err("a mesh needs at least one face");
            }
            validate_mesh(&mesh)?;
            let id = scene.next_id;
            let name = match name {
                Some(n) => check_name(n)?,
                None => unique_name(scene, "Mesh"),
            };
            let mut transform = Transform::default();
            set_transform(&mut transform, translation, rotation, scale)?;
            let mut material = Material::default();
            set_material(&mut material, &edit)?;
            scene.objects.push(Object {
                id,
                name,
                kind: "mesh".into(),
                transform,
                material,
                mesh,
                modifiers: Vec::new(),
                tracks: Vec::new(),
                smooth: false,
                group: None,
            });
            scene.next_id += 1;
            created.push(id);
        }
        Command::AddModifier {
            id,
            modifier,
            index,
        } => {
            let i = resolve(scene, id)?;
            modifier.validate()?;
            let stack = &mut scene.objects[i].modifiers;
            if stack.len() >= 16 {
                return err("an object can have at most 16 modifiers");
            }
            let at = index.unwrap_or(stack.len());
            if at > stack.len() {
                return err(format!("modifier index {at} is out of range"));
            }
            stack.insert(at, modifier.clone());
        }
        Command::SetModifier {
            id,
            index,
            modifier,
        } => {
            let i = resolve(scene, id)?;
            modifier.validate()?;
            let Some(slot) = scene.objects[i].modifiers.get_mut(*index) else {
                return err(format!("no modifier at index {index}"));
            };
            *slot = modifier.clone();
        }
        Command::RemoveModifier { id, index } => {
            let i = resolve(scene, id)?;
            if *index >= scene.objects[i].modifiers.len() {
                return err(format!("no modifier at index {index}"));
            }
            scene.objects[i].modifiers.remove(*index);
        }
        Command::ApplyModifiers { id } => {
            let i = resolve(scene, id)?;
            let o = &mut scene.objects[i];
            o.mesh = modifiers::evaluate(&o.mesh, &o.modifiers)?;
            o.modifiers.clear();
        }
        Command::SetKeyframe {
            id,
            property,
            frame,
            value,
            interpolation,
        } => {
            let i = resolve(scene, id)?;
            let frame = anim::check_frame(*frame)?;
            let o = &mut scene.objects[i];
            let value = match value {
                Some(v) => anim::key_value(*property, v)?,
                None => anim::value_at(o, *property, frame),
            };
            anim::set_key(
                o,
                *property,
                frame,
                value,
                interpolation.unwrap_or_default(),
            )?;
        }
        Command::DeleteKeyframe {
            id,
            frame,
            property,
        } => {
            let i = resolve(scene, id)?;
            if anim::delete_key(&mut scene.objects[i], *property, *frame) == 0 {
                return err(format!("no keyframe at frame {frame}"));
            }
        }
        Command::ClearAnimation { id, property } => {
            let i = resolve(scene, id)?;
            let o = &mut scene.objects[i];
            o.tracks
                .retain(|t| property.is_some_and(|p| p != t.property));
        }
        Command::SetAnimation { fps, start, end } => {
            let mut a = scene.animation.clone();
            if let Some(v) = fps {
                a.fps = *v;
            }
            if let Some(v) = start {
                a.start = *v;
            }
            if let Some(v) = end {
                a.end = *v;
            }
            a.validate()?;
            scene.animation = a;
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

pub fn set_material(m: &mut Material, e: &MaterialEdit) -> Result<(), EngineError> {
    if let Some(p) = e.preset {
        *m = p.material();
    }
    if let Some(c) = e.color {
        m.color = check_color(c)?;
    }
    if let Some(c) = e.emissive {
        m.emissive = check_color(c)?;
    } else if e.preset == Some(MaterialPreset::Neon) {
        m.emissive = m.color.clone();
    }
    for (value, slot, what, max) in [
        (e.roughness, &mut m.roughness, "roughness", 1.0),
        (e.metalness, &mut m.metalness, "metalness", 1.0),
        (
            e.emissive_strength,
            &mut m.emissive_strength,
            "emissive_strength",
            20.0,
        ),
        (e.opacity, &mut m.opacity, "opacity", 1.0),
        (e.transmission, &mut m.transmission, "transmission", 1.0),
    ] {
        if let Some(v) = value {
            if !(0.0..=max).contains(&v) {
                return err(format!("{what} must be between 0 and {max}"));
            }
            *slot = v;
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
            &MaterialEdit {
                color: Some(&m.color),
                roughness: Some(m.roughness),
                metalness: Some(m.metalness),
                emissive: Some(&m.emissive),
                emissive_strength: Some(m.emissive_strength),
                opacity: Some(m.opacity),
                transmission: Some(m.transmission),
                preset: None,
            },
        )
        .map_err(ctx)?;
        validate_mesh(&o.mesh).map_err(ctx)?;
        anim::validate_tracks(o).map_err(ctx)?;
    }
    scene.animation.validate()?;
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

pub fn world_bounds(transform: &Transform, mesh: &Mesh) -> Option<Bounds> {
    let m = transform.matrix();
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for v in &mesh.vertices {
        let p = m.transform_point3(DVec3::from(*v)).to_array();
        for k in 0..3 {
            min[k] = min[k].min(p[k]);
            max[k] = max[k].max(p[k]);
        }
    }
    min[0].is_finite().then_some(Bounds { min, max })
}

/// Compact scene description without mesh data, for agents and the chat prompt.
pub fn context(ed: &Editor) -> serde_json::Value {
    context_at(ed, None)
}

/// Scene summary; with `frame`, objects also report their animated pose and
/// bounds are measured at that frame.
pub fn context_at(ed: &Editor, frame: Option<f64>) -> serde_json::Value {
    let scene = ed.scene();
    let mut scene_min = [f64::INFINITY; 3];
    let mut scene_max = [f64::NEG_INFINITY; 3];
    let objects: Vec<_> = scene
        .objects
        .iter()
        .map(|o| {
            let mesh = ed.evaluated(o);
            let pose = frame
                .filter(|_| !o.tracks.is_empty())
                .map(|f| anim::pose(o, f));
            let bounds = world_bounds(pose.as_ref().map_or(&o.transform, |p| &p.0), mesh);
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
                "modifiers": o.modifiers,
                "base_face_count": o.mesh.faces.len(),
                "vertex_count": mesh.vertices.len(),
                "face_count": mesh.faces.len(),
                "bounds": bounds,
                "animation": o.tracks.iter().map(|t| serde_json::json!({
                    "property": t.property,
                    "frames": t.keys.iter().map(|k| k.frame).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "pose": pose.map(|(t, m)| serde_json::json!({ "transform": t, "material": m })),
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
        "animation": scene.animation,
        "frame": frame,
        "bounds": bounds,
        "objects": objects,
    })
}

pub fn export_obj(ed: &Editor) -> String {
    let mut out = String::from("# Exported from Tatara\n");
    let mut base = 1usize;
    for o in &ed.scene().objects {
        let mesh = ed.evaluated(o);
        let m = o.transform.matrix();
        let name: String = o
            .name
            .chars()
            .map(|c| if c.is_whitespace() { '_' } else { c })
            .collect();
        out.push_str(&format!("o {name}\n"));
        for v in &mesh.vertices {
            let p = m.transform_point3(DVec3::from(*v));
            out.push_str(&format!("v {:.6} {:.6} {:.6}\n", p.x, p.y, p.z));
        }
        // A mirrored transform flips winding; keep normals outward.
        let flip = m.determinant() < 0.0;
        for f in &mesh.faces {
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
        base += mesh.vertices.len();
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
        Primitive::Quadsphere { radius, level } => {
            let r = positive(radius, "radius")?;
            if !(1..=6).contains(&level) {
                return err("level must be between 1 and 6");
            }
            let mut mesh = cube(1.0);
            for _ in 0..level {
                mesh = catmull_clark(&mesh);
            }
            for v in &mut mesh.vertices {
                let p = DVec3::from(*v).normalize() * r;
                *v = p.to_array();
            }
            mesh
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
    let poly = face_loop(mesh, face)?;
    let n = face_normal(mesh, &poly);
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len < 1e-12 {
        return err("cannot extrude a degenerate face");
    }
    let offset = scale(n, distance / len);
    ring_face(mesh, face, &poly, |v| add(v, offset));
    Ok(())
}

pub fn inset(mesh: &mut Mesh, face: usize, fraction: f64) -> Result<(), EngineError> {
    let poly = face_loop(mesh, face)?;
    let centre = scale(
        poly.iter()
            .fold([0.0; 3], |acc, &i| add(acc, mesh.vertices[i as usize])),
        1.0 / poly.len() as f64,
    );
    ring_face(mesh, face, &poly, |v| {
        add(v, scale(add(centre, scale(v, -1.0)), fraction))
    });
    Ok(())
}

fn face_loop(mesh: &Mesh, face: usize) -> Result<Vec<u32>, EngineError> {
    let Some(poly) = mesh.faces.get(face).cloned() else {
        return err(format!(
            "face {face} does not exist (mesh has {} faces)",
            mesh.faces.len()
        ));
    };
    if mesh.faces.len() + poly.len() > MAX_FACES {
        return err(format!("mesh would exceed {MAX_FACES} faces"));
    }
    Ok(poly)
}

/// Replace a face with a moved copy of itself joined by a ring of quads.
/// The face keeps its index; the side quads are appended in edge order.
fn ring_face(mesh: &mut Mesh, face: usize, poly: &[u32], place: impl Fn(Vec3) -> Vec3) {
    let base = mesh.vertices.len() as u32;
    for &i in poly {
        let v = place(mesh.vertices[i as usize]);
        mesh.vertices.push(v);
    }
    let k = poly.len();
    for e in 0..k {
        let (a, b) = (poly[e], poly[(e + 1) % k]);
        let (a2, b2) = (base + e as u32, base + ((e + 1) % k) as u32);
        mesh.faces.push(vec![a, b, b2, a2]);
    }
    mesh.faces[face] = (base..base + k as u32).collect();
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
            Primitive::Quadsphere {
                radius: 1.0,
                level: 3,
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
        let obj = export_obj(&ed);
        assert_eq!(obj.lines().filter(|l| l.starts_with("v ")).count(), 32);
        assert!(obj.contains("f 29 30 31 32"));
        let ctx = context(&ed);
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
            modifiers: Vec::new(),
            tracks: Vec::new(),
            smooth: false,
            group: None,
        });
        assert!(ed.load(scene.clone()).is_err(), "id must be below next_id");
        scene.next_id = 6;
        ed.load(scene).unwrap();
        assert_eq!(ed.scene().objects.len(), 1);
    }

    #[test]
    fn inset_keeps_mesh_closed() {
        let mut m = cube(2.0);
        inset(&mut m, 4, 0.5).unwrap();
        assert_closed(&m);
        assert!((volume(&m) - 8.0).abs() < 1e-9);
        assert_eq!(m.faces.len(), 10);
        extrude(&mut m, 4, 1.0).unwrap();
        assert!((volume(&m) - 9.0).abs() < 1e-9, "1x1 boss of height 1");
    }

    #[test]
    fn modifier_commands_are_non_destructive_and_undoable() {
        let mut ed = Editor::new();
        ed.apply(&batch(serde_json::json!({"commands": [
            {"op": "add", "name": "Box", "primitive": {"kind": "cube"}},
            {"op": "add_modifier", "id": "Box", "modifier": {"type": "array", "count": 3, "offset": [0, 1.5, 0]}},
            {"op": "add_modifier", "id": "Box", "modifier": {"type": "subdivision", "levels": 1}}
        ]})))
        .unwrap();
        let o = &ed.scene().objects[0];
        assert_eq!(o.mesh.faces.len(), 6, "base mesh is untouched");
        assert_eq!(ed.evaluated(o).faces.len(), 72);
        assert_eq!(
            context(&ed)["bounds"]["max"][1],
            serde_json::json!(
                ed.evaluated(o)
                    .vertices
                    .iter()
                    .map(|v| v[1])
                    .fold(f64::MIN, f64::max)
            )
        );

        // Editing the base re-evaluates the stack.
        ed.apply(&batch(serde_json::json!({"commands": [
            {"op": "extrude", "id": "Box", "face": 4, "distance": 0.5}
        ]})))
        .unwrap();
        let o = &ed.scene().objects[0];
        assert_eq!(ed.evaluated(o).faces.len(), 3 * 10 * 4);

        // An over-budget stack is rejected atomically.
        let before = ed.scene().clone();
        assert!(ed.apply(&batch(serde_json::json!({"commands": [
            {"op": "set_modifier", "id": "Box", "index": 1, "modifier": {"type": "subdivision", "levels": 4}},
            {"op": "set_modifier", "id": "Box", "index": 0, "modifier": {"type": "array", "count": 100, "offset": [1, 0, 0]}}
        ]}))).is_err());
        assert_eq!(ed.scene(), &before);

        ed.undo().unwrap();
        let o = &ed.scene().objects[0];
        assert_eq!(
            ed.evaluated(o).faces.len(),
            72,
            "undo restores the evaluated mesh"
        );

        ed.apply(&batch(
            serde_json::json!({"commands": [{"op": "apply_modifiers", "id": "Box"}]}),
        ))
        .unwrap();
        let o = &ed.scene().objects[0];
        assert!(o.modifiers.is_empty());
        assert_eq!(o.mesh.faces.len(), 72);
    }

    #[test]
    fn keyframe_commands() {
        let mut ed = Editor::new();
        ed.apply(&batch(serde_json::json!({"commands": [
            {"op": "add", "name": "Ball", "primitive": {"kind": "sphere"}, "translation": [0, 1, 0]},
            {"op": "set_keyframe", "id": "Ball", "property": "translation", "frame": 1},
            {"op": "set_keyframe", "id": "Ball", "property": "translation", "frame": 25, "value": [2, 1, 0], "interpolation": "linear"},
            {"op": "set_keyframe", "id": "Ball", "property": "color", "frame": 25, "value": "#FF0000"},
            {"op": "set_animation", "fps": 30, "end": 48}
        ]})))
        .unwrap();
        let o = &ed.scene().objects[0];
        assert_eq!(o.tracks.len(), 2);
        let (t, m) = crate::anim::pose(o, 13.0);
        assert_eq!(
            t.translation,
            [1.0, 1.0, 0.0],
            "key without a value captures the pose"
        );
        assert_eq!(m.color, "#ff0000");
        assert_eq!(ed.scene().animation.fps, 30.0);
        let ctx = context_at(&ed, Some(25.0));
        assert_eq!(
            ctx["objects"][0]["pose"]["transform"]["translation"][0],
            2.0
        );
        assert_eq!(
            ctx["objects"][0]["animation"][0]["frames"],
            serde_json::json!([1.0, 25.0])
        );

        for bad in [
            serde_json::json!({"op": "set_keyframe", "id": "Ball", "property": "color", "frame": 1, "value": [1, 0, 0]}),
            serde_json::json!({"op": "set_keyframe", "id": "Ball", "property": "scale", "frame": -1, "value": [1, 1, 1]}),
            serde_json::json!({"op": "delete_keyframe", "id": "Ball", "frame": 7}),
            serde_json::json!({"op": "set_animation", "start": 50, "end": 10}),
        ] {
            assert!(
                ed.apply(&batch(serde_json::json!({"commands": [bad]})))
                    .is_err()
            );
        }
        ed.apply(&batch(serde_json::json!({"commands": [
            {"op": "delete_keyframe", "id": "Ball", "frame": 25, "property": "color"},
            {"op": "clear_animation", "id": "Ball", "property": "translation"}
        ]})))
        .unwrap();
        assert!(ed.scene().objects[0].tracks.is_empty());

        // Saved scenes with tracks load back; broken tracks are rejected.
        let mut saved = ed.scene().clone();
        saved.objects[0].tracks.push(crate::anim::Track {
            property: crate::anim::Property::Roughness,
            keys: vec![crate::anim::Key {
                frame: 3.0,
                value: vec![0.2, 0.4],
                interpolation: Default::default(),
            }],
        });
        assert!(Editor::new().load(saved).is_err());
    }

    #[test]
    fn material_presets_and_fields() {
        let mut ed = Editor::new();
        ed.apply(&batch(serde_json::json!({"commands": [
            {"op": "add", "name": "Vase", "primitive": {"kind": "cube"}, "preset": "glass"},
            {"op": "add", "name": "Sign", "primitive": {"kind": "torus"}, "preset": "neon", "color": "#30E0FF"},
            {"op": "add", "name": "Cup", "primitive": {"kind": "cube"}, "preset": "gold", "roughness": 0.5},
            {"op": "material", "id": "Cup", "opacity": 0.4, "emissive": "#ff8800", "emissive_strength": 3}
        ]})))
        .unwrap();
        let m = |i: usize| ed.scene().objects[i].material.clone();
        assert_eq!(m(0).transmission, 1.0);
        assert_eq!(
            (m(1).color.as_str(), m(1).emissive.as_str()),
            ("#30e0ff", "#30e0ff"),
            "neon glows in its colour"
        );
        assert!(m(1).emissive_strength > 1.0);
        assert_eq!((m(2).metalness, m(2).roughness), (1.0, 0.5));
        assert_eq!((m(2).opacity, m(2).emissive_strength), (0.4, 3.0));
        // A preset replaces the whole material, then explicit fields apply.
        ed.apply(&batch(serde_json::json!({"commands": [
            {"op": "material", "id": "Cup", "preset": "clay"}
        ]})))
        .unwrap();
        assert_eq!(
            ed.scene().objects[2].material,
            MaterialPreset::Clay.material()
        );

        for bad in [
            serde_json::json!({"op": "material", "id": "Cup", "opacity": 1.5}),
            serde_json::json!({"op": "material", "id": "Cup", "emissive_strength": 25}),
            serde_json::json!({"op": "material", "id": "Cup", "emissive": "orange"}),
        ] {
            assert!(
                ed.apply(&batch(serde_json::json!({"commands": [bad]})))
                    .is_err()
            );
        }

        // Scenes saved before these fields existed still load.
        let mut old = serde_json::to_value(ed.scene()).unwrap();
        for o in old["objects"].as_array_mut().unwrap() {
            let mat = o["material"].as_object_mut().unwrap();
            for k in ["emissive", "emissive_strength", "opacity", "transmission"] {
                mat.remove(k);
            }
        }
        let old: Scene = serde_json::from_value(old).unwrap();
        assert_eq!(old.objects[0].material.opacity, 1.0);
        assert!(Editor::new().load(old).is_ok());
    }

    #[test]
    fn sculpt_command_is_one_undo_step() {
        let mut ed = Editor::new();
        ed.apply(&batch(serde_json::json!({"commands": [
            {"op": "add", "name": "Head", "primitive": {"kind": "quadsphere", "radius": 0.5}}
        ]})))
        .unwrap();
        let before = ed.scene().objects[0].mesh.clone();
        assert_eq!(before.faces.len(), 6 * 4usize.pow(4));
        assert!(before.vertices.iter().all(|v| {
            let r = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            (r - 0.5).abs() < 1e-9
        }));
        ed.apply(&batch(serde_json::json!({"commands": [
            {"op": "sculpt", "id": "Head", "brush": "draw", "points": [[0.15, 0.1, 0.48], [0.15, -0.1, 0.48]], "radius": 0.15, "strength": 1, "symmetry": "x"}
        ]})))
        .unwrap();
        let after = &ed.scene().objects[0].mesh;
        assert_eq!(after.faces, before.faces);
        let reach = |m: &Mesh, x: f64| {
            m.vertices
                .iter()
                .filter(|v| (v[0] - x).abs() < 0.05 && v[1].abs() < 0.1)
                .map(|v| v[2])
                .fold(f64::MIN, f64::max)
        };
        assert!(reach(after, 0.15) > reach(&before, 0.15) + 0.02);
        assert!(
            reach(after, -0.15) > reach(&before, -0.15) + 0.02,
            "mirrored"
        );
        ed.undo().unwrap();
        assert_eq!(ed.scene().objects[0].mesh, before);
        assert!(ed.scene().objects[0].smooth, "quadspheres start smooth");
        ed.apply(&batch(serde_json::json!({"commands": [
            {"op": "shade", "id": "Head", "smooth": false}
        ]})))
        .unwrap();
        assert!(!ed.scene().objects[0].smooth);
        for bad in [
            serde_json::json!({"op": "sculpt", "id": "Head", "brush": "draw", "points": [], "radius": 0.1}),
            serde_json::json!({"op": "sculpt", "id": "Head", "brush": "draw", "points": [[0, 0, 0]], "radius": 0}),
            serde_json::json!({"op": "sculpt", "id": "Head", "brush": "grab", "points": [[0, 0, 0]], "radius": 0.1}),
            serde_json::json!({"op": "sculpt", "id": "Head", "brush": "draw", "points": [[0, 0, 0], [1000, 0, 0]], "radius": 0.001}),
        ] {
            assert!(
                ed.apply(&batch(serde_json::json!({"commands": [bad]})))
                    .is_err()
            );
        }
    }

    #[test]
    fn schema_lists_operations() {
        let s = command_schema().to_string();
        for op in [
            "add",
            "extrude",
            "subdivide",
            "vessel",
            "inset",
            "add_modifier",
            "twist",
            "emissive_strength",
            "neon",
            "sculpt",
            "quadsphere",
        ] {
            assert!(s.contains(op));
        }
    }
}
