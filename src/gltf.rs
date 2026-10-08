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
use crate::texture::{self, Pattern, Texture};

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
    /// Box-projected texture coordinates in metres (empty unless asked for).
    pub(crate) uvs: Vec<[f32; 2]>,
    pub(crate) indices: Vec<u32>,
}

/// `smooth` blends normals across every edge (smooth shading); `uv` adds
/// box-projected texture coordinates, splitting vertices where the
/// projection axis changes.
pub(crate) fn shade(mesh: &Mesh, smooth: bool, uv: bool) -> Shaded {
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
        indices: Vec::new(),
    };
    let mut ids: HashMap<(u32, [i32; 3], u8), u32> = HashMap::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        let axis = if uv { projection_axis(normals[fi]) } else { 0 };
        let corner: Vec<u32> = f
            .iter()
            .map(|&v| {
                let n = around[v as usize]
                    .iter()
                    .filter(|&&g| normals[g].dot(normals[fi]) >= cos)
                    .fold(DVec3::ZERO, |acc, &g| acc + normals[g])
                    .normalize_or(normals[fi]);
                let q = (n * 1e4).round().as_ivec3().to_array();
                *ids.entry((v, q, axis)).or_insert_with(|| {
                    let p = mesh.vertices[v as usize];
                    out.positions.push(p.map(|x| x as f32));
                    out.normals.push(n.as_vec3().to_array());
                    if uv {
                        let [u, w] = texture::box_uv(DVec3::from_array(p), normals[fi]);
                        out.uvs.push([u as f32, w as f32]);
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

/// Which of the six box-projection sides a face normal falls on.
fn projection_axis(n: DVec3) -> u8 {
    let a = n.abs();
    let (k, c) = if a.x >= a.y && a.x >= a.z {
        (0, n.x)
    } else if a.y >= a.z {
        (1, n.y)
    } else {
        (2, n.z)
    };
    k * 2 + u8::from(c < 0.0)
}

/// A glTF PBR material, with the KHR extensions it needs added to `used`.
/// `texture` is the glTF texture holding the baked pattern, if any: it
/// already contains the base colour, so the factor turns white and the
/// procedural parameters ride along in `extras` for a lossless re-import.
fn material_json(
    name: &str,
    m: &Material,
    texture: Option<usize>,
    used: &mut BTreeSet<String>,
) -> Value {
    let [r, g, b] = match texture {
        Some(_) => [1.0; 3],
        None => hex_to_linear(&m.color),
    };
    let mut out = json!({
        "name": name,
        "pbrMetallicRoughness": {
            "baseColorFactor": [r, g, b, m.opacity],
            "metallicFactor": m.metalness,
            "roughnessFactor": m.roughness,
        },
    });
    if let (Some(index), Some(t)) = (texture, &m.texture) {
        out["pbrMetallicRoughness"]["baseColorTexture"] = json!({ "index": index });
        out["extras"] = json!({ "tatara_texture": { "color": m.color, "texture": t } });
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
        let shaded = shade(ed.evaluated(o), o.smooth, tex.is_some());
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
        let texture = tex.map(|t| {
            // One tile of the pattern spans `scale` metres. glTF's V runs
            // down the image while ours runs up, hence the minus.
            let s = t.scale as f32;
            let uv_bytes: Vec<u8> = shaded
                .uvs
                .iter()
                .flat_map(|[u, v]| [u / s, -v / s])
                .flat_map(|v| v.to_le_bytes())
                .collect();
            let uvv = push_view(&mut bin, &uv_bytes, 34962);
            attributes["TEXCOORD_0"] = json!(accessors.len());
            accessors.push(json!({ "bufferView": uvv, "componentType": 5126, "count": shaded.uvs.len(), "type": "VEC2" }));
            let key = serde_json::to_string(&(&o.material.color, t)).expect("json serializes");
            *baked.entry(key).or_insert_with(|| {
                let png = texture::bake_png(&o.material.color, t, 256);
                let view = push_view(&mut bin, &png, 0);
                images.push(json!({ "bufferView": view, "mimeType": "image/png" }));
                textures.push(json!({ "sampler": 0, "source": images.len() - 1 }));
                textures.len() - 1
            })
        });
        materials.push(material_json(
            &o.name,
            &o.material,
            texture,
            &mut extensions,
        ));
        meshes.push(json!({
            "name": o.name,
            "primitives": [{ "attributes": attributes, "indices": a + 2, "material": materials.len() - 1, "mode": 4 }],
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

/// Read a glTF material into ours (defaults follow the glTF spec).
fn imported_material(m: Option<&Value>) -> Option<Material> {
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
    // Our own exports carry the procedural pattern; other textures are
    // dropped (the base colour factor stays).
    let ours = &m["extras"]["tatara_texture"];
    let texture = serde_json::from_value::<Texture>(ours["texture"].clone())
        .ok()
        .filter(|t| t.pattern != Pattern::None && t.validate().is_ok());
    let own_color = ours["color"]
        .as_str()
        .filter(|_| texture.is_some())
        .and_then(|c| check_color(c).ok());
    Some(Material {
        color: match own_color {
            Some(c) => c,
            None => linear_to_hex([0, 1, 2].map(|k| factor(base, k, 1.0))),
        },
        roughness: num(&pbr["roughnessFactor"], 1.0).clamp(0.0, 1.0),
        metalness: num(&pbr["metallicFactor"], 1.0).clamp(0.0, 1.0),
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
            out.push(match ctype {
                5120 => b[0] as i8 as f64,
                5121 => b[0] as f64,
                5122 => i16::from_le_bytes([b[0], b[1]]) as f64,
                5123 => u16::from_le_bytes([b[0], b[1]]) as f64,
                5125 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
                _ => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
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
/// coplanar triangle pairs back into convex quads.
pub fn rebuild_polygons(positions: &[Vec3], triangles: &[[u32; 3]]) -> Mesh {
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
    let tris: Vec<[u32; 3]> = triangles
        .iter()
        .map(|t| t.map(|i| remap[i as usize]))
        .filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2])
        .collect();
    let mesh = Mesh {
        vertices,
        faces: Vec::new(),
    };
    let normal = |t: &[u32]| newell(&mesh, t);
    let mut by_edge: HashMap<(u32, u32), usize> = HashMap::new();
    for (ti, t) in tris.iter().enumerate() {
        for k in 0..3 {
            by_edge.insert((t[k], t[(k + 1) % 3]), ti);
        }
    }
    let mut used = vec![false; tris.len()];
    let mut faces: Vec<Vec<u32>> = Vec::with_capacity(tris.len());
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
            }
            None => faces.push(t.to_vec()),
        }
    }
    Mesh {
        vertices: mesh.vertices,
        faces,
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
            let mut poly = rebuild_polygons(&positions, &triangles);
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
            }

            let material = prim["material"]
                .as_u64()
                .map(|m| &doc["materials"][m as usize]);
            let m = imported_material(material.filter(|m| !m.is_null()));
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
    Ok(commands)
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
                false,
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
        assert_eq!(
            doc["images"].as_array().unwrap().len(),
            2,
            "identical textures share an image"
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
