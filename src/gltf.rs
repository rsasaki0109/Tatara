//! glTF 2.0 interchange. Export writes a binary `.glb` of the evaluated
//! scene (one node, mesh and PBR material per object). Import reads `.glb`
//! or `.gltf` with embedded buffers and turns every triangle primitive into an
//! `add_mesh` command, welding split vertices and restoring planar quads so
//! the result is editable.

use std::collections::{BTreeSet, HashMap};

use base64::Engine as _;
use glam::{DMat4, DQuat, DVec3, EulerRot};
use serde_json::{Value, json};

use crate::anim::{Property, sample_track};
use crate::engine::{
    Command, Editor, EngineError, MAX_FACES, MAX_OBJECTS, Material, Mesh, Vec3, check_color,
};
use crate::image::{ImageAsset, MAX_IMAGES, Pixels, decode_base64};
use crate::nodes;
use crate::texture::{self, BoxProjection, Look, Pattern, Texture};

const CREASE_DEGREES: f64 = 38.0;
const GLB_MAGIC: &[u8; 4] = b"glTF";
const CHUNK_JSON: u32 = 0x4E4F_534A;
const CHUNK_BIN: u32 = 0x004E_4942;

fn err<T>(message: impl Into<String>) -> Result<T, EngineError> {
    Err(EngineError::new(message))
}

fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn hex_to_linear(hex: &str) -> [f64; 3] {
    let ch = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f64 / 255.0;
    [
        srgb_to_linear(ch(1)),
        srgb_to_linear(ch(3)),
        srgb_to_linear(ch(5)),
    ]
}

fn linear_to_hex(rgb: [f64; 3]) -> String {
    let b = |c: f64| (linear_to_srgb(c.clamp(0.0, 1.0)) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(rgb[0]), b(rgb[1]), b(rgb[2]))
}

fn newell(mesh: &Mesh, face: &[u32]) -> DVec3 {
    let mut n = DVec3::ZERO;
    for (k, &i) in face.iter().enumerate() {
        let a = DVec3::from(mesh.vertices[i as usize]);
        let b = DVec3::from(mesh.vertices[face[(k + 1) % face.len()] as usize]);
        n += DVec3::new(
            (a.y - b.y) * (a.z + b.z),
            (a.z - b.z) * (a.x + b.x),
            (a.x - b.x) * (a.y + b.y),
        );
    }
    n.normalize_or_zero()
}

/// Triangulated render data with normals split at creases, like the viewport.
pub(crate) struct Shaded {
    pub(crate) positions: Vec<[f32; 3]>,
    pub(crate) normals: Vec<[f32; 3]>,
    /// Texture coordinates (empty unless asked for): the mesh's own, or
    /// box-projected in metres.
    pub(crate) uvs: Vec<[f32; 2]>,
    /// Whether `uvs` are the mesh's own rather than box-projected.
    pub(crate) own_uvs: bool,
    /// Whether `uvs` are already in tiles (own or fitted), not metres.
    pub(crate) tiled: bool,
    pub(crate) indices: Vec<u32>,
}

/// `smooth` blends normals across every edge (smooth shading); `uv` adds
/// texture coordinates: the mesh's own if it has them, otherwise this box
/// projection, splitting vertices where the projection side changes.
pub(crate) fn shade(mesh: &Mesh, smooth: bool, uv: Option<&BoxProjection>) -> Shaded {
    let own_uvs = uv.is_some() && mesh.has_uvs();
    let normals: Vec<DVec3> = mesh.faces.iter().map(|f| newell(mesh, f)).collect();
    let mut around: Vec<Vec<usize>> = vec![Vec::new(); mesh.vertices.len()];
    for (fi, f) in mesh.faces.iter().enumerate() {
        for &v in f {
            around[v as usize].push(fi);
        }
    }
    let cos = if smooth {
        -2.0
    } else {
        CREASE_DEGREES.to_radians().cos()
    };
    let mut out = Shaded {
        positions: Vec::new(),
        normals: Vec::new(),
        uvs: Vec::new(),
        own_uvs,
        tiled: own_uvs || uv.is_some_and(BoxProjection::fitted),
        indices: Vec::new(),
    };
    let mut ids: HashMap<(u32, [i32; 3], [i64; 2]), u32> = HashMap::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        let corner: Vec<u32> = f
            .iter()
            .enumerate()
            .map(|(k, &v)| {
                let n = around[v as usize]
                    .iter()
                    .filter(|&&g| normals[g].dot(normals[fi]) >= cos)
                    .fold(DVec3::ZERO, |acc, &g| acc + normals[g])
                    .normalize_or(normals[fi]);
                let q = (n * 1e4).round().as_ivec3().to_array();
                let p = mesh.vertices[v as usize];
                let (corner_uv, uv_key) = match uv {
                    _ if own_uvs => {
                        let c = mesh.uvs[fi][k];
                        (c, c.map(|x| (x * 1e6).round() as i64))
                    }
                    Some(projection) => {
                        let (side, c) = projection.uv(DVec3::from_array(p), normals[fi]);
                        (c, [side as i64, 0])
                    }
                    None => ([0.0; 2], [0; 2]),
                };
                *ids.entry((v, q, uv_key)).or_insert_with(|| {
                    out.positions.push(p.map(|x| x as f32));
                    out.normals.push(n.as_vec3().to_array());
                    if uv.is_some() {
                        out.uvs.push(corner_uv.map(|x| x as f32));
                    }
                    (out.positions.len() - 1) as u32
                })
            })
            .collect();
        for k in 1..corner.len().saturating_sub(1) {
            out.indices.extend([corner[0], corner[k], corner[k + 1]]);
        }
    }
    out
}

/// Per-vertex tangents for normal maps: xyz along growing u, w the sign
/// that turns cross(normal, tangent) into the direction of growing v
/// (image up), as glTF expects.
fn tangents(shaded: &Shaded) -> Vec<[f32; 4]> {
    let n = shaded.positions.len();
    let (mut tu, mut tv) = (vec![DVec3::ZERO; n], vec![DVec3::ZERO; n]);
    let p = |i: usize| DVec3::from_array(shaded.positions[i].map(f64::from));
    let uv = |i: usize| shaded.uvs[i].map(f64::from);
    for t in shaded.indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| i as usize);
        let (e1, e2) = (p(b) - p(a), p(c) - p(a));
        let (d1, d2) = (
            [uv(b)[0] - uv(a)[0], uv(b)[1] - uv(a)[1]],
            [uv(c)[0] - uv(a)[0], uv(c)[1] - uv(a)[1]],
        );
        let det = d1[0] * d2[1] - d2[0] * d1[1];
        if det.abs() < 1e-12 {
            continue;
        }
        let du = (e1 * d2[1] - e2 * d1[1]) / det;
        let dv = (e2 * d1[0] - e1 * d2[0]) / det;
        for i in [a, b, c] {
            tu[i] += du;
            tv[i] += dv;
        }
    }
    (0..n)
        .map(|i| {
            let normal = DVec3::from_array(shaded.normals[i].map(f64::from));
            let t = (tu[i] - normal * normal.dot(tu[i]))
                .try_normalize()
                .unwrap_or_else(|| normal.any_orthonormal_vector());
            let w = if normal.cross(t).dot(tv[i]) < 0.0 {
                -1.0
            } else {
                1.0
            };
            [t.x as f32, t.y as f32, t.z as f32, w]
        })
        .collect()
}

/// `color_map` and `normal_map` are the glTF textures for the material's
/// texture, if any. A baked pattern already contains the base colour, so the
/// factor turns white; an image is tinted by it. The texture's parameters
/// ride along in `extras` for a lossless re-import.
fn material_json(
    name: &str,
    m: &Material,
    color_map: Option<usize>,
    normal_map: Option<usize>,
    orm_map: Option<usize>,
    used: &mut BTreeSet<String>,
) -> Value {
    let baked = m
        .texture
        .as_ref()
        .is_some_and(|t| !matches!(t.pattern, Pattern::Image | Pattern::None));
    let [r, g, b] = match color_map {
        Some(_) if baked => [1.0; 3],
        _ => hex_to_linear(&m.color),
    };
    let mut out = json!({
        "name": name,
        "pbrMetallicRoughness": {
            "baseColorFactor": [r, g, b, m.opacity],
            "metallicFactor": m.metalness,
            "roughnessFactor": m.roughness,
        },
    });
    if let Some(index) = color_map {
        out["pbrMetallicRoughness"]["baseColorTexture"] = json!({ "index": index });
    }
    if let Some(index) = normal_map {
        out["normalTexture"] = json!({ "index": index });
    }
    // A node graph's roughness (G) and metalness (B), baked with the
    // material's own values where the graph leaves them unset.
    if let Some(index) = orm_map {
        let pbr = &mut out["pbrMetallicRoughness"];
        pbr["metallicRoughnessTexture"] = json!({ "index": index });
        pbr["metallicFactor"] = json!(1.0);
        pbr["roughnessFactor"] = json!(1.0);
    }
    if let Some(t) = &m.texture {
        // The factors may be 1 for a baked roughness map: keep the real ones.
        out["extras"] = json!({ "tatara_texture": {
            "color": m.color, "texture": t, "roughness": m.roughness, "metalness": m.metalness,
        } });
    }
    let mut ext = serde_json::Map::new();
    let emissive = hex_to_linear(&m.emissive);
    if m.emissive_strength > 0.0 && emissive.iter().any(|&c| c > 0.0) {
        let scale = m.emissive_strength.min(1.0);
        out["emissiveFactor"] = json!(emissive.map(|c| c * scale));
        if m.emissive_strength > 1.0 {
            ext.insert(
                "KHR_materials_emissive_strength".into(),
                json!({ "emissiveStrength": m.emissive_strength }),
            );
        }
    }
    if m.opacity < 1.0 {
        out["alphaMode"] = json!("BLEND");
    }
    if m.transmission > 0.0 {
        ext.insert(
            "KHR_materials_transmission".into(),
            json!({ "transmissionFactor": m.transmission }),
        );
    }
    if !ext.is_empty() {
        used.extend(ext.keys().cloned());
        out["extensions"] = Value::Object(ext);
    }
    out
}

/// Binary glTF of the scene as displayed (modifiers applied).
pub fn export_glb(ed: &Editor) -> Vec<u8> {
    let mut bin: Vec<u8> = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut meshes = Vec::new();
    let mut materials = Vec::new();
    let mut nodes = Vec::new();
    let mut channels = Vec::new();
    let mut samplers = Vec::new();
    let mut extensions = BTreeSet::new();
    let mut images = Vec::new();
    let mut textures = Vec::new();
    let mut baked: HashMap<String, usize> = HashMap::new();
    let scene_images = &ed.scene().images;
    let mut decoded: HashMap<String, Option<Pixels>> = HashMap::new();
    let fps = ed.scene().animation.fps;
    let mut push_view = |bin: &mut Vec<u8>, bytes: &[u8], target: u32| -> usize {
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let mut view = json!({ "buffer": 0, "byteOffset": bin.len(), "byteLength": bytes.len() });
        if target != 0 {
            view["target"] = json!(target);
        }
        views.push(view);
        bin.extend_from_slice(bytes);
        views.len() - 1
    };
    for o in &ed.scene().objects {
        let tex = o.material.texture.as_ref();
        let mesh = ed.evaluated(o);
        let projection = tex.map(|t| BoxProjection::new(&mesh.vertices, o.transform.scale, t.fit));
        let shaded = shade(mesh, o.smooth, projection.as_ref());
        if shaded.indices.is_empty() {
            continue;
        }
        let pos_bytes: Vec<u8> = shaded
            .positions
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let nrm_bytes: Vec<u8> = shaded
            .normals
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let idx_bytes: Vec<u8> = shaded
            .indices
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let (mut min, mut max) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for p in &shaded.positions {
            for k in 0..3 {
                min[k] = min[k].min(p[k]);
                max[k] = max[k].max(p[k]);
            }
        }
        let pv = push_view(&mut bin, &pos_bytes, 34962);
        let nv = push_view(&mut bin, &nrm_bytes, 34962);
        let iv = push_view(&mut bin, &idx_bytes, 34963);
        let a = accessors.len();
        accessors.push(json!({ "bufferView": pv, "componentType": 5126, "count": shaded.positions.len(), "type": "VEC3", "min": min, "max": max }));
        accessors.push(json!({ "bufferView": nv, "componentType": 5126, "count": shaded.normals.len(), "type": "VEC3" }));
        accessors.push(json!({ "bufferView": iv, "componentType": 5125, "count": shaded.indices.len(), "type": "SCALAR" }));
        let mut attributes = json!({ "POSITION": a, "NORMAL": a + 1 });
        let mut extras = Value::Null;
        let (mut color_map, mut normal_map, mut orm_map) = (None, None, None);
        if let Some(t) = tex {
            // A node graph is baked once for this material.
            let graph = nodes::bake_material(&o.material, scene_images, 256);
            let graph_key = serde_json::to_string(&(
                &o.material.color,
                o.material.roughness,
                o.material.metalness,
                &t.graph,
            ))
            .expect("json serializes");
            // Box projection: one tile of the pattern spans `scale` metres.
            // glTF's V runs down the image while ours runs up.
            let s = if shaded.tiled { 1.0 } else { t.scale as f32 };
            let uv_bytes: Vec<u8> = shaded
                .uvs
                .iter()
                .flat_map(|[u, v]| [u / s, 1.0 - v / s])
                .flat_map(|v| v.to_le_bytes())
                .collect();
            let uvv = push_view(&mut bin, &uv_bytes, 34962);
            attributes["TEXCOORD_0"] = json!(accessors.len());
            accessors.push(json!({ "bufferView": uvv, "componentType": 5126, "count": shaded.uvs.len(), "type": "VEC2" }));
            if !shaded.own_uvs {
                extras = json!({ "tatara_box_uv": true });
            }
            let color = &o.material.color;
            let mut texture_of =
                |key: String, bytes: &dyn Fn() -> (Vec<u8>, &'static str, String)| {
                    *baked.entry(key).or_insert_with(|| {
                        let (data, mime, name) = bytes();
                        let view = push_view(&mut bin, &data, 0);
                        images.push(json!({ "bufferView": view, "mimeType": mime, "name": name }));
                        textures.push(json!({ "sampler": 0, "source": images.len() - 1 }));
                        textures.len() - 1
                    })
                };
            let stored = |name: &str| -> (Vec<u8>, &'static str, String) {
                let img = &scene_images[name];
                let mime = if img.mime == "image/png" {
                    "image/png"
                } else {
                    "image/jpeg"
                };
                (img.data.to_vec(), mime, name.to_string())
            };
            color_map = match (t.pattern, &t.image) {
                (Pattern::None, _) => None,
                (Pattern::Nodes, _) => graph.as_ref().map(|b| {
                    texture_of(format!("nodes:{graph_key}"), &|| {
                        (
                            texture::pixels_png(&b.color),
                            "image/png",
                            format!("{} nodes", o.name),
                        )
                    })
                }),
                (Pattern::Image, Some(name)) => {
                    Some(texture_of(format!("image:{name}"), &|| stored(name)))
                }
                _ => {
                    let key = serde_json::to_string(&(color, t.pattern, &t.color2))
                        .expect("json serializes");
                    Some(texture_of(format!("pattern:{key}"), &|| {
                        (
                            texture::bake_png(color, t, 256),
                            "image/png",
                            format!("{} pattern", o.name),
                        )
                    }))
                }
            };
            normal_map = if let Some(name) = &t.normal_map {
                Some(texture_of(format!("image:{name}"), &|| stored(name)))
            } else if t.relief > 0.0 {
                if let Some(name) = &t.image {
                    decoded
                        .entry(name.clone())
                        .or_insert_with(|| scene_images[name].decode().ok());
                }
                let look = Look {
                    base: color,
                    texture: t,
                    image: t.image.as_ref().and_then(|n| decoded[n].as_ref()),
                    normal_map: None,
                    baked: graph.as_ref(),
                };
                let key = serde_json::to_string(&(t.pattern, &t.image, t.relief, &graph_key))
                    .expect("json serializes");
                let index = texture_of(format!("relief:{key}"), &|| {
                    (
                        texture::bake_normal_png(&look, 256),
                        "image/png",
                        format!("{} relief", o.name),
                    )
                });
                Some(index)
            } else {
                None
            };
            orm_map = graph.as_ref().and_then(|b| b.orm.as_ref()).map(|orm| {
                texture_of(format!("orm:{graph_key}"), &|| {
                    (
                        texture::pixels_png(orm),
                        "image/png",
                        format!("{} roughness", o.name),
                    )
                })
            });
        }
        if normal_map.is_some() {
            let tangent_bytes: Vec<u8> = tangents(&shaded)
                .iter()
                .flatten()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            let tv = push_view(&mut bin, &tangent_bytes, 34962);
            attributes["TANGENT"] = json!(accessors.len());
            accessors.push(json!({ "bufferView": tv, "componentType": 5126, "count": shaded.positions.len(), "type": "VEC4" }));
        }
        materials.push(material_json(
            &o.name,
            &o.material,
            color_map,
            normal_map,
            orm_map,
            &mut extensions,
        ));
        let mut primitive = json!({ "attributes": attributes, "indices": a + 2, "material": materials.len() - 1, "mode": 4 });
        if !extras.is_null() {
            primitive["extras"] = extras;
        }
        meshes.push(json!({
            "name": o.name,
            "primitives": [primitive],
        }));
        let t = &o.transform;
        let [rx, ry, rz] = t.rotation;
        let q = DQuat::from_euler(EulerRot::XYZ, rx, ry, rz);
        nodes.push(json!({
            "name": o.name,
            "mesh": meshes.len() - 1,
            "translation": t.translation,
            "rotation": [q.x, q.y, q.z, q.w],
            "scale": t.scale,
        }));

        // Transform tracks become glTF animation channels, baked once per
        // frame (and at every key) so eased and stepped keys look the same.
        let node = nodes.len() - 1;
        for track in &o.tracks {
            let path = match track.property {
                Property::Translation => "translation",
                Property::Rotation => "rotation",
                Property::Scale => "scale",
                _ => continue,
            };
            let (first, last) = (track.keys[0].frame, track.keys[track.keys.len() - 1].frame);
            let mut frames: Vec<f64> = (first.ceil() as i64..=last.floor() as i64)
                .map(|f| f as f64)
                .collect();
            frames.extend(track.keys.iter().map(|k| k.frame));
            frames.sort_by(f64::total_cmp);
            frames.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
            let times: Vec<f32> = frames.iter().map(|f| (f / fps) as f32).collect();
            let values: Vec<f32> = frames
                .iter()
                .flat_map(|&f| {
                    let v = sample_track(track, f);
                    if track.property == Property::Rotation {
                        let q = DQuat::from_euler(EulerRot::XYZ, v[0], v[1], v[2]);
                        vec![q.x as f32, q.y as f32, q.z as f32, q.w as f32]
                    } else {
                        v.iter().map(|&x| x as f32).collect()
                    }
                })
                .collect();
            let time_bytes: Vec<u8> = times.iter().flat_map(|v| v.to_le_bytes()).collect();
            let value_bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
            let tv = push_view(&mut bin, &time_bytes, 0);
            let vv = push_view(&mut bin, &value_bytes, 0);
            let a = accessors.len();
            accessors.push(json!({ "bufferView": tv, "componentType": 5126, "count": times.len(), "type": "SCALAR", "min": [times[0]], "max": [times[times.len() - 1]] }));
            accessors.push(json!({ "bufferView": vv, "componentType": 5126, "count": times.len(), "type": if track.property == Property::Rotation { "VEC4" } else { "VEC3" } }));
            samplers.push(json!({ "input": a, "output": a + 1, "interpolation": "LINEAR" }));
            channels.push(
                json!({ "sampler": samplers.len() - 1, "target": { "node": node, "path": path } }),
            );
        }
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let mut doc = json!({
        "asset": { "version": "2.0", "generator": concat!("Tatara ", env!("CARGO_PKG_VERSION")) },
        "scene": 0,
        "scenes": [{ "name": "Scene", "nodes": (0..nodes.len()).collect::<Vec<_>>() }],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials,
        "accessors": accessors,
        "bufferViews": views,
    });
    if !bin.is_empty() {
        doc["buffers"] = json!([{ "byteLength": bin.len() }]);
    }
    if !textures.is_empty() {
        doc["images"] = json!(images);
        doc["textures"] = json!(textures);
        doc["samplers"] =
            json!([{ "magFilter": 9729, "minFilter": 9987, "wrapS": 10497, "wrapT": 10497 }]);
    }
    if !extensions.is_empty() {
        doc["extensionsUsed"] = json!(extensions);
    }
    if !channels.is_empty() {
        doc["animations"] =
            json!([{ "name": "Tatara", "channels": channels, "samplers": samplers }]);
    }
    let mut json_bytes = serde_json::to_vec(&doc).expect("json serializes");
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' ');
    }
    let total = 12 + 8 + json_bytes.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(GLB_MAGIC);
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&CHUNK_JSON.to_le_bytes());
    out.extend_from_slice(&json_bytes);
    if !bin.is_empty() {
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(&CHUNK_BIN.to_le_bytes());
        out.extend_from_slice(&bin);
    }
    out
}

// ---------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------

/// glTF images become scene images: each is added once, under a unique name.
struct ImageImport<'a> {
    doc: &'a Value,
    buffers: &'a [Vec<u8>],
    by_source: HashMap<usize, Option<String>>,
    taken: BTreeSet<String>,
    commands: Vec<Command>,
}

impl ImageImport<'_> {
    /// The scene image for a glTF texture reference (`{"index": n}`), if it
    /// uses the first UV set and holds a PNG or JPEG we can read.
    fn image(&mut self, info: &Value, name: Option<&str>) -> Option<String> {
        if info.is_null() || info["texCoord"].as_u64().unwrap_or(0) != 0 {
            return None;
        }
        let texture = &self.doc["textures"][info["index"].as_u64()? as usize];
        let source = texture["source"].as_u64()? as usize;
        if let Some(found) = self.by_source.get(&source) {
            return found.clone();
        }
        let image = &self.doc["images"][source];
        let found = self
            .bytes(image)
            .filter(|_| self.taken.len() < MAX_IMAGES)
            .and_then(|b| ImageAsset::from_bytes(b).ok())
            .map(|asset| {
                let fallback = format!("Image {}", source + 1);
                let base = clean_name(name.or(image["name"].as_str()), &fallback);
                let mut unique = base.clone();
                let mut k = 1;
                while !self.taken.insert(unique.clone()) {
                    k += 1;
                    unique = format!("{base} {k}");
                }
                self.commands.push(Command::AddImage {
                    name: unique.clone(),
                    data: base64::engine::general_purpose::STANDARD.encode(&asset.data),
                });
                unique
            });
        self.by_source.insert(source, found.clone());
        found
    }

    fn bytes(&self, image: &Value) -> Option<Vec<u8>> {
        if let Some(v) = image["bufferView"].as_u64() {
            let view = &self.doc["bufferViews"][v as usize];
            let buffer = self.buffers.get(view["buffer"].as_u64()? as usize)?;
            let start = view["byteOffset"].as_u64().unwrap_or(0) as usize;
            let len = view["byteLength"].as_u64()? as usize;
            return buffer
                .get(start..start.checked_add(len)?)
                .map(<[u8]>::to_vec);
        }
        let uri = image["uri"].as_str()?;
        if uri.starts_with("data:") {
            decode_base64(uri).ok()
        } else {
            None
        }
    }
}

/// Read a glTF material into ours (defaults follow the glTF spec).
/// Textures become scene images through `images`.
fn imported_material(m: Option<&Value>, images: &mut ImageImport) -> Option<Material> {
    let m = m?;
    let pbr = &m["pbrMetallicRoughness"];
    let num = |v: &Value, default: f64| v.as_f64().unwrap_or(default);
    let factor = |v: &Value, k: usize, default: f64| {
        v.as_array()
            .and_then(|f| f.get(k))
            .and_then(Value::as_f64)
            .unwrap_or(default)
            .max(0.0)
    };
    let base = &pbr["baseColorFactor"];
    let emissive = &m["emissiveFactor"];
    let ext = &m["extensions"];
    let blend = m["alphaMode"].as_str() == Some("BLEND");
    // Our own exports carry the texture's parameters, and the images it
    // names travel as glTF images. Other files' base colour and normal
    // textures become an image texture (tinted by the colour factor).
    let ours = &m["extras"]["tatara_texture"];
    let (color_map, normal_map) = (&pbr["baseColorTexture"], &m["normalTexture"]);
    let texture = match serde_json::from_value::<Texture>(ours["texture"].clone()) {
        Ok(mut t) => {
            if let Some(name) = t.image.clone() {
                t.image = images.image(color_map, Some(&name));
            }
            if let Some(name) = t.normal_map.clone() {
                t.normal_map = images.image(normal_map, Some(&name));
            }
            Some(t)
        }
        Err(_) => {
            let image = images.image(color_map, None);
            let normal_map = images.image(normal_map, None);
            (image.is_some() || normal_map.is_some()).then(|| Texture {
                pattern: if image.is_some() {
                    Pattern::Image
                } else {
                    Pattern::None
                },
                image,
                normal_map,
                ..Texture::new(Pattern::None, "#3b2a22", 1.0)
            })
        }
    }
    .filter(|t| t.validate().is_ok() && (t.pattern != Pattern::None || t.normal_map.is_some()));
    let own_color = ours["color"]
        .as_str()
        .filter(|_| texture.is_some() && !ours.is_null())
        .and_then(|c| check_color(c).ok());
    Some(Material {
        color: match own_color {
            Some(c) => c,
            None => linear_to_hex([0, 1, 2].map(|k| factor(base, k, 1.0))),
        },
        roughness: ours["roughness"]
            .as_f64()
            .unwrap_or(num(&pbr["roughnessFactor"], 1.0))
            .clamp(0.0, 1.0),
        metalness: ours["metalness"]
            .as_f64()
            .unwrap_or(num(&pbr["metallicFactor"], 1.0))
            .clamp(0.0, 1.0),
        emissive: linear_to_hex([0, 1, 2].map(|k| factor(emissive, k, 0.0))),
        emissive_strength: num(
            &ext["KHR_materials_emissive_strength"]["emissiveStrength"],
            1.0,
        )
        .clamp(0.0, 20.0),
        opacity: if blend {
            factor(base, 3, 1.0).clamp(0.0, 1.0)
        } else {
            1.0
        },
        transmission: num(
            &ext["KHR_materials_transmission"]["transmissionFactor"],
            0.0,
        )
        .clamp(0.0, 1.0),
        texture,
    })
}

fn parse_glb(bytes: &[u8]) -> Result<(Value, Option<Vec<u8>>), EngineError> {
    let u32_at = |i: usize| -> Result<u32, EngineError> {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .ok_or_else(|| EngineError::new("truncated .glb file"))
    };
    if u32_at(4)? != 2 {
        return err("only glTF 2.0 is supported");
    }
    let mut at = 12;
    let mut doc = None;
    let mut bin = None;
    while at + 8 <= bytes.len() {
        let len = u32_at(at)? as usize;
        let kind = u32_at(at + 4)?;
        let body = bytes
            .get(at + 8..at + 8 + len)
            .ok_or_else(|| EngineError::new("truncated .glb chunk"))?;
        match kind {
            CHUNK_JSON => {
                doc = Some(
                    serde_json::from_slice(body)
                        .map_err(|e| EngineError::new(format!("invalid glTF JSON: {e}")))?,
                )
            }
            CHUNK_BIN if bin.is_none() => bin = Some(body.to_vec()),
            _ => {}
        }
        at += 8 + len;
    }
    Ok((
        doc.ok_or_else(|| EngineError::new(".glb has no JSON chunk"))?,
        bin,
    ))
}

fn load_buffers(doc: &Value, glb_bin: Option<Vec<u8>>) -> Result<Vec<Vec<u8>>, EngineError> {
    let mut glb_bin = glb_bin;
    let mut out = Vec::new();
    for (i, b) in doc["buffers"].as_array().into_iter().flatten().enumerate() {
        match b["uri"].as_str() {
            None => out.push(
                glb_bin
                    .take()
                    .ok_or_else(|| EngineError::new(format!("buffer {i} has no data")))?,
            ),
            Some(uri) if uri.starts_with("data:") => {
                let data = uri
                    .split_once(";base64,")
                    .map(|(_, d)| d)
                    .ok_or_else(|| EngineError::new("only base64 data URIs are supported"))?;
                out.push(
                    base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .map_err(|e| EngineError::new(format!("buffer {i}: {e}")))?,
                );
            }
            Some(_) => {
                return err(
                    "external buffer files are not supported: use .glb or a .gltf with embedded data",
                );
            }
        }
    }
    Ok(out)
}

fn read_accessor(
    doc: &Value,
    buffers: &[Vec<u8>],
    index: usize,
) -> Result<(Vec<f64>, usize), EngineError> {
    let a = &doc["accessors"][index];
    if a.is_null() {
        return err(format!("accessor {index} does not exist"));
    }
    if !a["sparse"].is_null() {
        return err("sparse accessors are not supported");
    }
    let width = match a["type"].as_str() {
        Some("SCALAR") => 1,
        Some("VEC2") => 2,
        Some("VEC3") => 3,
        Some("VEC4") => 4,
        _ => return err(format!("accessor {index} has an unsupported type")),
    };
    let ctype = a["componentType"].as_u64().unwrap_or(0);
    let size = match ctype {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        5125 | 5126 => 4,
        _ => {
            return err(format!(
                "accessor {index} has an unsupported component type"
            ));
        }
    };
    let normalized = a["normalized"].as_bool().unwrap_or(false);
    let count = a["count"].as_u64().unwrap_or(0) as usize;
    if count > 50_000_000 {
        return err("accessor is too large");
    }
    let Some(view_index) = a["bufferView"].as_u64() else {
        return Ok((vec![0.0; count * width], width));
    };
    let view = &doc["bufferViews"][view_index as usize];
    let buffer = buffers
        .get(view["buffer"].as_u64().unwrap_or(u64::MAX) as usize)
        .ok_or_else(|| EngineError::new(format!("accessor {index} references a missing buffer")))?;
    let base = view["byteOffset"].as_u64().unwrap_or(0) as usize
        + a["byteOffset"].as_u64().unwrap_or(0) as usize;
    let stride = view["byteStride"]
        .as_u64()
        .map(|s| s as usize)
        .unwrap_or(size * width);
    let mut out = Vec::with_capacity(count * width);
    for i in 0..count {
        for k in 0..width {
            let at = base + i * stride + k * size;
            let b = buffer.get(at..at + size).ok_or_else(|| {
                EngineError::new(format!("accessor {index} reads past its buffer"))
            })?;
            let x = match ctype {
                5120 => b[0] as i8 as f64,
                5121 => b[0] as f64,
                5122 => i16::from_le_bytes([b[0], b[1]]) as f64,
                5123 => u16::from_le_bytes([b[0], b[1]]) as f64,
                5125 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
                _ => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            };
            out.push(match (normalized, ctype) {
                (true, 5120) => (x / 127.0).max(-1.0),
                (true, 5121) => x / 255.0,
                (true, 5122) => (x / 32767.0).max(-1.0),
                (true, 5123) => x / 65535.0,
                _ => x,
            });
        }
    }
    Ok((out, width))
}

fn local_matrix(node: &Value) -> DMat4 {
    if let Some(m) = node["matrix"].as_array().filter(|m| m.len() == 16) {
        let cols: Vec<f64> = m.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect();
        return DMat4::from_cols_slice(&cols);
    }
    let vec = |key: &str, default: &[f64]| -> Vec<f64> {
        node[key]
            .as_array()
            .map(|a| a.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect())
            .filter(|v: &Vec<f64>| v.len() == default.len())
            .unwrap_or_else(|| default.to_vec())
    };
    let t = vec("translation", &[0.0, 0.0, 0.0]);
    let r = vec("rotation", &[0.0, 0.0, 0.0, 1.0]);
    let s = vec("scale", &[1.0, 1.0, 1.0]);
    DMat4::from_scale_rotation_translation(
        DVec3::new(s[0], s[1], s[2]),
        DQuat::from_xyzw(r[0], r[1], r[2], r[3]).normalize(),
        DVec3::new(t[0], t[1], t[2]),
    )
}

/// Weld identical positions, drop degenerate triangles, then merge
/// coplanar triangle pairs back into convex quads. With per-vertex `uvs`,
/// each corner keeps its texture coordinate and pairs only merge where the
/// UVs of their shared edge agree (not across a seam).
pub fn rebuild_polygons(
    positions: &[Vec3],
    triangles: &[[u32; 3]],
    uvs: Option<&[[f64; 2]]>,
) -> Mesh {
    let mut weld: HashMap<[u64; 3], u32> = HashMap::new();
    let mut vertices: Vec<Vec3> = Vec::new();
    let remap: Vec<u32> = positions
        .iter()
        .map(|p| {
            let key = p.map(|v| ((v * 1e6).round() as i64) as u64);
            *weld.entry(key).or_insert_with(|| {
                vertices.push(*p);
                (vertices.len() - 1) as u32
            })
        })
        .collect();
    let (tris, tri_uvs): (Vec<[u32; 3]>, Vec<[[f64; 2]; 3]>) = triangles
        .iter()
        .map(|t| {
            (
                t.map(|i| remap[i as usize]),
                t.map(|i| uvs.map_or([0.0; 2], |u| u[i as usize])),
            )
        })
        .filter(|(t, _)| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
        .unzip();
    let mesh = Mesh::new(vertices, Vec::new());
    let uv_of = |ti: usize, v: u32| tri_uvs[ti][tris[ti].iter().position(|&x| x == v).unwrap()];
    let same_uv = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).abs() + (a[1] - b[1]).abs() < 1e-6;
    let normal = |t: &[u32]| newell(&mesh, t);
    let mut by_edge: HashMap<(u32, u32), usize> = HashMap::new();
    for (ti, t) in tris.iter().enumerate() {
        for k in 0..3 {
            by_edge.insert((t[k], t[(k + 1) % 3]), ti);
        }
    }
    let mut used = vec![false; tris.len()];
    let mut faces: Vec<Vec<u32>> = Vec::with_capacity(tris.len());
    let mut face_uvs: Vec<Vec<[f64; 2]>> = Vec::new();
    for ti in 0..tris.len() {
        if used[ti] {
            continue;
        }
        used[ti] = true;
        let t = tris[ti];
        let n = normal(&t);
        // Prefer the partner that comes next in index order: exporters (this
        // one included) emit a polygon's triangles consecutively, and
        // coplanar neighbours from other polygons would otherwise steal it.
        let mut merged: Option<(usize, [u32; 4])> = None;
        for k in 0..3 {
            let (a, b, c) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
            let Some(&ui) = by_edge.get(&(b, a)) else {
                continue;
            };
            if used[ui] {
                continue;
            }
            let u = tris[ui];
            let d = *u.iter().find(|&&x| x != a && x != b).expect("third vertex");
            if normal(&u).dot(n) < 0.99999 {
                continue;
            }
            if uvs.is_some()
                && !(same_uv(uv_of(ti, a), uv_of(ui, a)) && same_uv(uv_of(ti, b), uv_of(ui, b)))
            {
                continue;
            }
            let quad = [a, d, b, c];
            let convex = (0..4).all(|i| {
                let p = DVec3::from(mesh.vertices[quad[i] as usize]);
                let q = DVec3::from(mesh.vertices[quad[(i + 1) % 4] as usize]);
                let r = DVec3::from(mesh.vertices[quad[(i + 2) % 4] as usize]);
                (q - p).cross(r - q).dot(n) > 1e-12
            });
            let better = merged.is_none_or(|(best, _)| ui.abs_diff(ti) < best.abs_diff(ti));
            if convex && better {
                merged = Some((ui, quad));
            }
        }
        match merged {
            Some((ui, quad)) => {
                used[ui] = true;
                faces.push(quad.to_vec());
                if uvs.is_some() {
                    let d = quad[1];
                    face_uvs.push(vec![
                        uv_of(ti, quad[0]),
                        uv_of(ui, d),
                        uv_of(ti, quad[2]),
                        uv_of(ti, quad[3]),
                    ]);
                }
            }
            None => {
                faces.push(t.to_vec());
                if uvs.is_some() {
                    face_uvs.push(tri_uvs[ti].to_vec());
                }
            }
        }
    }
    Mesh {
        vertices: mesh.vertices,
        faces,
        uvs: face_uvs,
    }
}

fn clean_name(raw: Option<&str>, fallback: &str) -> String {
    let name: String = raw
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(72)
        .collect::<String>()
        .trim()
        .to_string();
    if name.is_empty() {
        fallback.to_string()
    } else {
        name
    }
}

/// Parse a `.glb` or `.gltf` file into `add_mesh` commands.
pub fn import(bytes: &[u8]) -> Result<Vec<Command>, EngineError> {
    let (doc, bin) = if bytes.starts_with(GLB_MAGIC) {
        parse_glb(bytes)?
    } else {
        let doc: Value = serde_json::from_slice(bytes)
            .map_err(|e| EngineError::new(format!("not a .glb or .gltf file: {e}")))?;
        (doc, None)
    };
    if !doc["asset"]["version"]
        .as_str()
        .unwrap_or("")
        .starts_with('2')
    {
        return err("only glTF 2.0 is supported");
    }
    let buffers = load_buffers(&doc, bin)?;
    let nodes = doc["nodes"].as_array().cloned().unwrap_or_default();
    let roots: Vec<usize> =
        match doc["scenes"][doc["scene"].as_u64().unwrap_or(0) as usize]["nodes"].as_array() {
            Some(list) => list
                .iter()
                .filter_map(|v| v.as_u64().map(|v| v as usize))
                .collect(),
            None => {
                let mut child = vec![false; nodes.len()];
                for n in &nodes {
                    for c in n["children"].as_array().into_iter().flatten() {
                        if let Some(c) = c.as_u64().and_then(|c| child.get_mut(c as usize)) {
                            *c = true;
                        }
                    }
                }
                (0..nodes.len()).filter(|&i| !child[i]).collect()
            }
        };

    let mut commands = Vec::new();
    let mut images = ImageImport {
        doc: &doc,
        buffers: &buffers,
        by_source: HashMap::new(),
        taken: BTreeSet::new(),
        commands: Vec::new(),
    };
    let mut names: HashMap<String, usize> = HashMap::new();
    let mut stack: Vec<(usize, DMat4, usize)> = roots
        .into_iter()
        .rev()
        .map(|i| (i, DMat4::IDENTITY, 0))
        .collect();
    while let Some((ni, parent, depth)) = stack.pop() {
        let Some(node) = nodes.get(ni) else {
            return err(format!("node {ni} does not exist"));
        };
        if depth > 64 {
            return err("node hierarchy is too deep");
        }
        let world = parent * local_matrix(node);
        for c in node["children"].as_array().into_iter().flatten().rev() {
            if let Some(c) = c.as_u64() {
                stack.push((c as usize, world, depth + 1));
            }
        }
        let Some(mi) = node["mesh"].as_u64() else {
            continue;
        };
        let mesh = &doc["meshes"][mi as usize];
        let prims: Vec<&Value> = mesh["primitives"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["mode"].as_u64().unwrap_or(4) == 4)
            .collect();
        for (pi, prim) in prims.iter().enumerate() {
            let Some(pos_index) = prim["attributes"]["POSITION"].as_u64() else {
                continue;
            };
            let (pos, width) = read_accessor(&doc, &buffers, pos_index as usize)?;
            if width != 3 {
                return err("POSITION must be VEC3");
            }
            let positions: Vec<Vec3> = pos.as_chunks::<3>().0.to_vec();
            let indices: Vec<u32> = match prim["indices"].as_u64() {
                Some(ii) => read_accessor(&doc, &buffers, ii as usize)?
                    .0
                    .into_iter()
                    .map(|v| v as u32)
                    .collect(),
                None => (0..positions.len() as u32).collect(),
            };
            if indices.iter().any(|&i| i as usize >= positions.len()) {
                return err("triangle index out of range");
            }
            let triangles: Vec<[u32; 3]> = indices.as_chunks::<3>().0.to_vec();
            if triangles.len() > MAX_FACES * 2 {
                return err(format!("a primitive exceeds {MAX_FACES} faces"));
            }

            // Keep the node transform when it is a clean TRS; otherwise bake it.
            let (s, r, t) = world.to_scale_rotation_translation();
            let rebuilt = DMat4::from_scale_rotation_translation(s, r, t);
            let clean = rebuilt.abs_diff_eq(
                world,
                1e-6 * (1.0
                    + world
                        .abs()
                        .to_cols_array()
                        .iter()
                        .cloned()
                        .fold(0.0, f64::max)),
            ) && s.min_element().abs() > 1e-6;
            let (positions, translation, rotation, scale) = if clean {
                let (x, y, z) = r.to_euler(EulerRot::XYZ);
                (positions, t.to_array(), [x, y, z], s.to_array())
            } else {
                let baked = positions
                    .iter()
                    .map(|p| world.transform_point3(DVec3::from(*p)).to_array())
                    .collect();
                (baked, [0.0; 3], [0.0; 3], [1.0; 3])
            };
            let material = prim["material"]
                .as_u64()
                .map(|m| &doc["materials"][m as usize]);
            let m = imported_material(material.filter(|m| !m.is_null()), &mut images);
            // Textured meshes keep their UVs, unless they were our box
            // projection (which the importer redoes from the geometry).
            let uvs: Option<Vec<[f64; 2]>> = match prim["attributes"]["TEXCOORD_0"].as_u64() {
                Some(ui)
                    if m.as_ref().is_some_and(|m| m.texture.is_some())
                        && prim["extras"]["tatara_box_uv"] != json!(true) =>
                {
                    let (uv, width) = read_accessor(&doc, &buffers, ui as usize)?;
                    (width == 2 && uv.len() == positions.len() * 2).then(|| {
                        uv.as_chunks::<2>()
                            .0
                            .iter()
                            .map(|[s, t]| [*s, 1.0 - t])
                            .collect()
                    })
                }
                _ => None,
            };
            let mut poly = rebuild_polygons(&positions, &triangles, uvs.as_deref());
            if poly.faces.is_empty() {
                continue;
            }
            if poly.faces.len() > MAX_FACES {
                return err(format!("a primitive exceeds {MAX_FACES} faces"));
            }
            if world.determinant() < 0.0 && !clean {
                for f in &mut poly.faces {
                    f.reverse();
                }
                for f in &mut poly.uvs {
                    f.reverse();
                }
            }

            let base = clean_name(node["name"].as_str().or(mesh["name"].as_str()), "Mesh");
            let base = if prims.len() > 1 {
                format!("{base}.{pi}")
            } else {
                base
            };
            let n = names.entry(base.clone()).or_insert(0);
            *n += 1;
            let name = if *n > 1 { format!("{base} {n}") } else { base };
            commands.push(Command::AddMesh {
                name: Some(name),
                vertices: poly.vertices,
                faces: poly.faces,
                uvs: (!poly.uvs.is_empty()).then_some(poly.uvs),
                translation: Some(translation),
                rotation: Some(rotation),
                scale: Some(scale),
                color: m.as_ref().map(|m| m.color.clone()),
                roughness: m.as_ref().map(|m| m.roughness),
                metalness: m.as_ref().map(|m| m.metalness),
                emissive: m.as_ref().map(|m| m.emissive.clone()),
                emissive_strength: m.as_ref().map(|m| m.emissive_strength),
                opacity: m.as_ref().map(|m| m.opacity),
                transmission: m.as_ref().map(|m| m.transmission),
                texture: m.as_ref().and_then(|m| m.texture.clone()),
                preset: None,
            });
            if commands.len() > MAX_OBJECTS {
                return err(format!("a scene is limited to {MAX_OBJECTS} objects"));
            }
        }
    }
    if commands.is_empty() {
        return err("the file contains no triangle meshes");
    }
    // Images first: the meshes' textures refer to them.
    let mut all = std::mem::take(&mut images.commands);
    all.extend(commands);
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CommandBatch;

    fn editor_with(commands: serde_json::Value) -> Editor {
        let mut ed = Editor::new();
        ed.apply(&serde_json::from_value::<CommandBatch>(json!({ "commands": commands })).unwrap())
            .unwrap();
        ed
    }

    fn reimport(bytes: &[u8]) -> Editor {
        let commands = import(bytes).unwrap();
        let mut ed = Editor::new();
        ed.apply(&CommandBatch {
            commands,
            expected_revision: None,
        })
        .unwrap();
        ed
    }

    #[test]
    fn smooth_shading_shares_normals_across_edges() {
        let cube = |smooth| {
            let mut ed = editor_with(json!([{"op": "add", "primitive": {"kind": "cube"}}]));
            if smooth {
                ed.apply(
                    &serde_json::from_value::<CommandBatch>(
                        json!({"commands": [{"op": "shade", "id": 1, "smooth": true}]}),
                    )
                    .unwrap(),
                )
                .unwrap();
            }
            shade(
                &ed.scene().objects[0].mesh,
                ed.scene().objects[0].smooth,
                None,
            )
            .positions
            .len()
        };
        assert_eq!(cube(false), 24, "creased: four corners per face");
        assert_eq!(cube(true), 8, "smooth: one normal per vertex");
    }

    #[test]
    fn glb_round_trip_keeps_glow_glass_and_opacity() {
        let ed = editor_with(json!([
            {"op": "add", "name": "Sign", "primitive": {"kind": "torus"}, "preset": "neon", "color": "#30e0ff"},
            {"op": "add", "name": "Vase", "primitive": {"kind": "cube"}, "preset": "glass"},
            {"op": "add", "name": "Ghost", "primitive": {"kind": "cube"}, "opacity": 0.4},
            {"op": "add", "name": "Plain", "primitive": {"kind": "cube"}}
        ]));
        let glb = export_glb(&ed);
        let len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        let doc: Value = serde_json::from_slice(&glb[20..20 + len]).unwrap();
        assert_eq!(
            doc["extensionsUsed"],
            json!([
                "KHR_materials_emissive_strength",
                "KHR_materials_transmission"
            ])
        );
        assert_eq!(doc["materials"][2]["alphaMode"], "BLEND");
        assert!(doc["materials"][3].get("extensions").is_none());
        assert!(doc["materials"][3].get("emissiveFactor").is_none());

        let back = reimport(&glb);
        let m = |i: usize| back.scene().objects[i].material.clone();
        assert_eq!(m(0).emissive, "#30e0ff");
        assert_eq!(m(0).emissive_strength, 2.0);
        assert_eq!(m(1).transmission, 1.0);
        assert!((m(2).opacity - 0.4).abs() < 1e-9);
        assert_eq!(m(3), crate::engine::Material::default());
    }

    #[test]
    fn glb_bakes_textures_and_round_trips_them() {
        let ed = editor_with(json!([
            {"op": "add", "name": "Floor", "primitive": {"kind": "cube"}, "preset": "tiles"},
            {"op": "add", "name": "Table", "primitive": {"kind": "cube"}, "preset": "wood"},
            {"op": "add", "name": "Shelf", "primitive": {"kind": "cube"}, "preset": "wood"},
            {"op": "add", "name": "Plain", "primitive": {"kind": "cube"}}
        ]));
        let glb = export_glb(&ed);
        let len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        let doc: Value = serde_json::from_slice(&glb[20..20 + len]).unwrap();
        // Tiles and wood: a colour and a relief normal map each, shared by
        // the two wooden objects.
        assert_eq!(
            doc["images"].as_array().unwrap().len(),
            4,
            "identical textures share an image"
        );
        assert_eq!(doc["materials"][0]["normalTexture"]["index"], 1);
        let tangent = doc["meshes"][0]["primitives"][0]["attributes"]["TANGENT"]
            .as_u64()
            .unwrap();
        assert_eq!(doc["accessors"][tangent as usize]["type"], "VEC4");
        assert!(
            doc["meshes"][3]["primitives"][0]["attributes"]
                .get("TANGENT")
                .is_none()
        );
        assert_eq!(
            doc["materials"][1]["normalTexture"],
            doc["materials"][2]["normalTexture"]
        );
        assert_eq!(doc["images"][0]["mimeType"], "image/png");
        let pbr = &doc["materials"][0]["pbrMetallicRoughness"];
        assert_eq!(
            pbr["baseColorFactor"],
            json!([1.0, 1.0, 1.0, 1.0]),
            "the colour is in the image"
        );
        assert_eq!(pbr["baseColorTexture"]["index"], 0);
        let uv = doc["meshes"][0]["primitives"][0]["attributes"]["TEXCOORD_0"]
            .as_u64()
            .unwrap();
        assert_eq!(
            doc["accessors"][uv as usize]["count"], 24,
            "a cube's sides project separately"
        );
        assert!(
            doc["meshes"][3]["primitives"][0]["attributes"]
                .get("TEXCOORD_0")
                .is_none()
        );

        let back = reimport(&glb);
        for i in 0..4 {
            assert_eq!(
                back.scene().objects[i].material,
                ed.scene().objects[i].material
            );
        }
    }

    /// A textured quad as another tool would write it: two triangles with
    /// UVs, a base colour texture and a normal texture, all embedded.
    fn foreign_gltf() -> Vec<u8> {
        use base64::Engine as _;
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let pos: [f32; 12] = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0];
        let uv: [f32; 8] = [0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0];
        let idx: [u16; 6] = [0, 1, 2, 0, 2, 3];
        let mut buf: Vec<u8> = pos.iter().flat_map(|v| v.to_le_bytes()).collect();
        buf.extend(uv.iter().flat_map(|v| v.to_le_bytes()));
        buf.extend(idx.iter().flat_map(|v| v.to_le_bytes()));
        let png = crate::image::tests::tiny_png();
        serde_json::to_vec(&json!({
            "asset": {"version": "2.0"},
            "scenes": [{"nodes": [0]}],
            "nodes": [{"mesh": 0, "name": "Poster"}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "TEXCOORD_0": 1}, "indices": 2, "material": 0}]}],
            "materials": [{"name": "Paper", "pbrMetallicRoughness": {"baseColorFactor": [1, 0.5, 0.5, 1], "baseColorTexture": {"index": 0}}, "normalTexture": {"index": 1}}],
            "textures": [{"source": 0}, {"source": 1}],
            "images": [
                {"name": "photo", "uri": format!("data:image/png;base64,{}", b64(&png))},
                {"uri": format!("data:image/png;base64,{}", b64(&png))}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC2"},
                {"bufferView": 2, "componentType": 5123, "count": 6, "type": "SCALAR"}
            ],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 48},
                {"buffer": 0, "byteOffset": 48, "byteLength": 32},
                {"buffer": 0, "byteOffset": 80, "byteLength": 12}
            ],
            "buffers": [{"byteLength": buf.len(), "uri": format!("data:application/octet-stream;base64,{}", b64(&buf))}]
        }))
        .unwrap()
    }

    #[test]
    fn imports_image_textures_normal_maps_and_uvs() {
        let ed = reimport(&foreign_gltf());
        assert_eq!(
            ed.scene().images.keys().collect::<Vec<_>>(),
            ["Image 2", "photo"]
        );
        let o = &ed.scene().objects[0];
        assert_eq!(
            o.mesh.faces.len(),
            1,
            "a quad again: its seam-free UVs agree"
        );
        // UVs come in with V flipped up: the corner at t = 1 sits at v = 0.
        let corner = o.mesh.faces[0]
            .iter()
            .position(|&v| o.mesh.vertices[v as usize] == [0.0; 3])
            .unwrap();
        assert_eq!(o.mesh.uvs[0][corner], [0.0, 0.0]);
        let t = o.material.texture.as_ref().unwrap();
        assert_eq!(
            (t.pattern, t.image.as_deref(), t.normal_map.as_deref()),
            (Pattern::Image, Some("photo"), Some("Image 2"))
        );
        assert_eq!(
            o.material.color, "#ffbcbc",
            "the colour factor tints the image"
        );

        // Our export keeps the image bytes and the UVs, and reads back the same.
        let glb = export_glb(&ed);
        let len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        let doc: Value = serde_json::from_slice(&glb[20..20 + len]).unwrap();
        assert_eq!(doc["images"][0]["name"], "photo");
        assert!(
            doc["meshes"][0]["primitives"][0].get("extras").is_none(),
            "own UVs, not box projection"
        );
        let back = reimport(&glb);
        assert_eq!(back.scene().images, ed.scene().images);
        let b = &back.scene().objects[0];
        assert_eq!((&b.material, b.mesh.has_uvs()), (&o.material, true));
    }

    #[test]
    fn box_projected_image_textures_round_trip_without_uvs() {
        use base64::Engine as _;
        let png = base64::engine::general_purpose::STANDARD.encode(crate::image::tests::tiny_png());
        let ed = editor_with(json!([
            {"op": "add_image", "name": "Label", "data": png},
            {"op": "add", "name": "Crate", "primitive": {"kind": "cube"}, "color": "#ffffff", "texture": {"pattern": "image", "image": "Label", "scale": 0.5, "relief": 0.5}}
        ]));
        let back = reimport(&export_glb(&ed));
        let o = &back.scene().objects[0];
        assert_eq!(o.material, ed.scene().objects[0].material);
        assert!(
            o.mesh.uvs.is_empty(),
            "box projection is redone, so scale still works"
        );
        assert_eq!(back.scene().images, ed.scene().images);
    }

    #[test]
    fn node_graphs_bake_into_textures_and_round_trip() {
        let ed = editor_with(json!([
            {"op": "add", "name": "Rust", "primitive": {"kind": "cube"}, "roughness": 0.3, "metalness": 0.9, "texture": {"pattern": "nodes", "scale": 0.5, "relief": 0.5, "graph": {
                "nodes": [
                    {"id": "n", "type": "noise", "scale": 4},
                    {"id": "r", "type": "ramp", "factor": {"node": "n"}, "stops": [{"at": 0.3, "color": "#8a8f96"}, {"at": 0.6, "color": "#8a3b1c"}]},
                    {"id": "rough", "type": "math", "op": "greater_than", "a": {"node": "n"}, "b": 0.5}
                ],
                "output": {"color": {"node": "r"}, "roughness": {"node": "rough"}, "height": {"node": "n"}}
            }}}
        ]));
        let glb = export_glb(&ed);
        let len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        let doc: Value = serde_json::from_slice(&glb[20..20 + len]).unwrap();
        let m = &doc["materials"][0];
        let pbr = &m["pbrMetallicRoughness"];
        assert!(pbr.get("baseColorTexture").is_some() && m.get("normalTexture").is_some());
        assert_eq!(
            (
                pbr["roughnessFactor"].as_f64(),
                pbr["metallicFactor"].as_f64()
            ),
            (Some(1.0), Some(1.0))
        );
        assert!(
            pbr.get("metallicRoughnessTexture").is_some(),
            "roughness comes from the graph"
        );
        assert_eq!(doc["images"].as_array().unwrap().len(), 3);

        let back = reimport(&glb);
        assert_eq!(
            back.scene().objects[0].material,
            ed.scene().objects[0].material
        );
    }

    #[test]
    fn glb_round_trip_keeps_topology_transform_and_material() {
        let ed = editor_with(json!([
            {"op": "add", "name": "Box", "primitive": {"kind": "cube"}, "translation": [1, 0.5, -2], "rotation": [0.3, 0.6, -0.2], "scale": [1, 2, 0.5], "color": "#8fb9a0", "roughness": 0.22, "metalness": 0.1},
            {"op": "add", "name": "Ball", "primitive": {"kind": "sphere", "segments": 16, "rings": 8}, "color": "#2f4f8f"}
        ]));
        let glb = export_glb(&ed);
        assert_eq!(&glb[..4], b"glTF");
        assert_eq!(
            u32::from_le_bytes(glb[8..12].try_into().unwrap()) as usize,
            glb.len()
        );
        let back = reimport(&glb);
        let objs = &back.scene().objects;
        assert_eq!(objs.len(), 2);
        let (a, b) = (&ed.scene().objects[0], &objs[0]);
        assert_eq!(b.name, "Box");
        assert_eq!(b.mesh.vertices.len(), 8, "split normals are welded");
        assert_eq!(
            b.mesh.faces.len(),
            6,
            "triangles are merged back into quads"
        );
        assert_eq!(b.material.color, "#8fb9a0");
        assert!((b.material.roughness - 0.22).abs() < 1e-6);
        for k in 0..3 {
            assert!((a.transform.translation[k] - b.transform.translation[k]).abs() < 1e-9);
            assert!((a.transform.rotation[k] - b.transform.rotation[k]).abs() < 1e-9);
            assert!((a.transform.scale[k] - b.transform.scale[k]).abs() < 1e-9);
        }
        assert_eq!(
            objs[1].mesh.faces.len(),
            ed.scene().objects[1].mesh.faces.len()
        );
    }

    #[test]
    fn modifiers_are_applied_on_export() {
        let ed = editor_with(json!([
            {"op": "add", "name": "Box", "primitive": {"kind": "cube"}},
            {"op": "add_modifier", "id": "Box", "modifier": {"type": "array", "count": 3, "offset": [2, 0, 0]}}
        ]));
        let back = reimport(&export_glb(&ed));
        assert_eq!(back.scene().objects[0].mesh.faces.len(), 18);
        assert!(back.scene().objects[0].modifiers.is_empty());
    }

    #[test]
    fn gltf_with_embedded_buffer_and_hierarchy() {
        let ed = editor_with(
            json!([{"op": "add", "name": "Box", "primitive": {"kind": "cube"}, "translation": [1, 0, 0]}]),
        );
        let glb = export_glb(&ed);
        let (mut doc, bin) = parse_glb(&glb).unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(bin.unwrap());
        doc["buffers"][0]["uri"] = json!(format!("data:application/octet-stream;base64,{data}"));
        // Wrap the object in a parent that moves it up by 2.
        let child = doc["nodes"][0].clone();
        doc["nodes"] =
            json!([{"name": "Parent", "translation": [0, 2, 0], "children": [1]}, child]);
        doc["scenes"][0]["nodes"] = json!([0]);
        let back = reimport(&serde_json::to_vec(&doc).unwrap());
        let o = &back.scene().objects[0];
        assert_eq!(o.transform.translation, [1.0, 2.0, 0.0]);

        doc["buffers"][0]["uri"] = json!("scene.bin");
        assert!(
            import(&serde_json::to_vec(&doc).unwrap())
                .unwrap_err()
                .message
                .contains("external")
        );
        assert!(import(b"not a model").is_err());
    }

    #[test]
    fn coplanar_neighbours_do_not_steal_partners() {
        // Extruding a side face leaves the new side quads coplanar with
        // their neighbours; the round trip must still give back 10 quads.
        for face in 0..6 {
            let ed = editor_with(json!([
                {"op": "add", "name": "Box", "primitive": {"kind": "cube"}},
                {"op": "extrude", "id": "Box", "face": face, "distance": 0.3}
            ]));
            let back = reimport(&export_glb(&ed));
            let faces = &back.scene().objects[0].mesh.faces;
            assert_eq!(faces.len(), 10, "face {face}");
            assert!(faces.iter().all(|f| f.len() == 4));
        }
    }

    #[test]
    fn transform_tracks_export_as_animation_channels() {
        let ed = editor_with(json!([
            {"op": "add", "name": "Box", "primitive": {"kind": "cube"}},
            {"op": "set_keyframe", "id": "Box", "property": "translation", "frame": 0, "value": [0, 0, 0]},
            {"op": "set_keyframe", "id": "Box", "property": "translation", "frame": 24, "value": [2, 0, 0]},
            {"op": "set_keyframe", "id": "Box", "property": "rotation", "frame": 12, "value": [0, 1.0, 0]},
            {"op": "set_keyframe", "id": "Box", "property": "color", "frame": 12, "value": "#ff0000"}
        ]));
        let (doc, bin) = parse_glb(&export_glb(&ed)).unwrap();
        let anim = &doc["animations"][0];
        assert_eq!(
            anim["channels"].as_array().unwrap().len(),
            2,
            "colour is not a glTF channel"
        );
        assert_eq!(anim["channels"][0]["target"]["path"], "translation");
        let input = &doc["accessors"][anim["samplers"][0]["input"].as_u64().unwrap() as usize];
        assert_eq!(input["count"], 25);
        assert_eq!(input["max"][0], 1.0, "24 frames at 24 fps is one second");
        let buffers = load_buffers(&doc, bin).unwrap();
        let (out, width) = read_accessor(
            &doc,
            &buffers,
            anim["samplers"][0]["output"].as_u64().unwrap() as usize,
        )
        .unwrap();
        assert_eq!(width, 3);
        assert!((out[out.len() - 3] - 2.0).abs() < 1e-6);
        let (rot, w) = read_accessor(
            &doc,
            &buffers,
            anim["samplers"][1]["output"].as_u64().unwrap() as usize,
        )
        .unwrap();
        assert_eq!((w, rot.len()), (4, 4), "a single key gives one quaternion");
        // Static scenes have no animations block.
        let still = editor_with(json!([{"op": "add", "primitive": {"kind": "cube"}}]));
        assert!(parse_glb(&export_glb(&still)).unwrap().0["animations"].is_null());
    }

    #[test]
    fn shear_is_baked_into_vertices() {
        let ed = editor_with(json!([{"op": "add", "name": "Box", "primitive": {"kind": "cube"}}]));
        let (mut doc, bin) = parse_glb(&export_glb(&ed)).unwrap();
        let child = doc["nodes"][0].clone();
        doc["nodes"] = json!([{"name": "Squash", "scale": [2, 1, 1], "rotation": [0, 0, 0.3826834, 0.9238795], "children": [1]}, child]);
        doc["nodes"][1]["rotation"] = json!([0, 0, 0.3826834, 0.9238795]);
        doc["scenes"][0]["nodes"] = json!([0]);
        let data = base64::engine::general_purpose::STANDARD.encode(bin.unwrap());
        doc["buffers"][0]["uri"] = json!(format!("data:application/octet-stream;base64,{data}"));
        let back = reimport(&serde_json::to_vec(&doc).unwrap());
        let o = &back.scene().objects[0];
        assert_eq!(o.transform.scale, [1.0; 3], "non-TRS world matrix is baked");
        assert_eq!(o.mesh.faces.len(), 6);
    }
}
