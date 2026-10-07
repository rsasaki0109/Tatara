//! glTF 2.0 interchange. Export writes a binary `.glb` of the evaluated
//! scene (one node, mesh and PBR material per object). Import reads `.glb`
//! or `.gltf` with embedded buffers and turns every triangle primitive into an
//! `add_mesh` command, welding split vertices and restoring planar quads so
//! the result is editable.

use std::collections::HashMap;

use base64::Engine as _;
use glam::{DMat4, DQuat, DVec3, EulerRot};
use serde_json::{Value, json};

use crate::engine::{Command, Editor, EngineError, MAX_FACES, MAX_OBJECTS, Mesh, Vec3};

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
struct Shaded {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

fn shade(mesh: &Mesh) -> Shaded {
    let normals: Vec<DVec3> = mesh.faces.iter().map(|f| newell(mesh, f)).collect();
    let mut around: Vec<Vec<usize>> = vec![Vec::new(); mesh.vertices.len()];
    for (fi, f) in mesh.faces.iter().enumerate() {
        for &v in f {
            around[v as usize].push(fi);
        }
    }
    let cos = CREASE_DEGREES.to_radians().cos();
    let mut out = Shaded {
        positions: Vec::new(),
        normals: Vec::new(),
        indices: Vec::new(),
    };
    let mut ids: HashMap<(u32, [i32; 3]), u32> = HashMap::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        let corner: Vec<u32> = f
            .iter()
            .map(|&v| {
                let n = around[v as usize]
                    .iter()
                    .filter(|&&g| normals[g].dot(normals[fi]) >= cos)
                    .fold(DVec3::ZERO, |acc, &g| acc + normals[g])
                    .normalize_or(normals[fi]);
                let q = (n * 1e4).round().as_ivec3().to_array();
                *ids.entry((v, q)).or_insert_with(|| {
                    out.positions
                        .push(mesh.vertices[v as usize].map(|x| x as f32));
                    out.normals.push(n.as_vec3().to_array());
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

/// Binary glTF of the scene as displayed (modifiers applied).
pub fn export_glb(ed: &Editor) -> Vec<u8> {
    let mut bin: Vec<u8> = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut meshes = Vec::new();
    let mut materials = Vec::new();
    let mut nodes = Vec::new();
    let mut push_view = |bin: &mut Vec<u8>, bytes: &[u8], target: u32| -> usize {
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        views.push(json!({ "buffer": 0, "byteOffset": bin.len(), "byteLength": bytes.len(), "target": target }));
        bin.extend_from_slice(bytes);
        views.len() - 1
    };
    for o in &ed.scene().objects {
        let shaded = shade(ed.evaluated(o));
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
        let [r, g, b] = hex_to_linear(&o.material.color);
        materials.push(json!({
            "name": o.name,
            "pbrMetallicRoughness": {
                "baseColorFactor": [r, g, b, 1.0],
                "metallicFactor": o.material.metalness,
                "roughnessFactor": o.material.roughness,
            },
        }));
        meshes.push(json!({
            "name": o.name,
            "primitives": [{ "attributes": { "POSITION": a, "NORMAL": a + 1 }, "indices": a + 2, "material": materials.len() - 1, "mode": 4 }],
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
            let positions: Vec<Vec3> = pos.chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
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
            let triangles: Vec<[u32; 3]> = indices
                .chunks_exact(3)
                .map(|c| [c[0], c[1], c[2]])
                .collect();
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
            let (color, roughness, metalness) = match material {
                Some(m) if !m.is_null() => {
                    let pbr = &m["pbrMetallicRoughness"];
                    let f = pbr["baseColorFactor"].as_array();
                    let c = |k: usize| {
                        f.and_then(|f| f.get(k))
                            .and_then(Value::as_f64)
                            .unwrap_or(1.0)
                    };
                    (
                        Some(linear_to_hex([c(0), c(1), c(2)])),
                        Some(
                            pbr["roughnessFactor"]
                                .as_f64()
                                .unwrap_or(1.0)
                                .clamp(0.0, 1.0),
                        ),
                        Some(
                            pbr["metallicFactor"]
                                .as_f64()
                                .unwrap_or(1.0)
                                .clamp(0.0, 1.0),
                        ),
                    )
                }
                _ => (None, None, None),
            };
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
                color,
                roughness,
                metalness,
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
