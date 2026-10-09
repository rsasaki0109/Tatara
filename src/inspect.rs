//! Scene inspection for agents: the problems a picture can hide. Objects
//! that intersect, float above whatever is beneath them or sink into the
//! floor are reported with distances, and `drop` settles an object onto the
//! surface below it. Everything is measured on the displayed (modifier
//! evaluated) meshes in world space; the floor is y = 0.

use std::collections::HashMap;

use glam::DVec3;
use serde_json::{Value, json};

use crate::engine::{Editor, EngineError, Mesh, Object, Scene};
use crate::modifiers;

/// Gaps and penetrations below this (metres) count as touching.
pub const TOLERANCE: f64 = 0.002;
/// Vertices sampled per object for the pairwise tests.
const SAMPLES: usize = 1500;

pub struct Solid {
    pub id: u64,
    pub name: String,
    /// The assembly this object belongs to; parts of one group may touch.
    pub group: Option<String>,
    tris: Vec<[DVec3; 3]>,
    verts: Vec<DVec3>,
    pub min: DVec3,
    pub max: DVec3,
    /// Every edge shared by exactly two faces, so inside/outside is defined.
    closed: bool,
}

pub fn solid(o: &Object, mesh: &Mesh) -> Solid {
    let m = o.transform.matrix();
    let verts: Vec<DVec3> = mesh
        .vertices
        .iter()
        .map(|v| m.transform_point3(DVec3::from(*v)))
        .collect();
    let mut tris = Vec::new();
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    for f in &mesh.faces {
        for k in 1..f.len().saturating_sub(1) {
            tris.push([
                verts[f[0] as usize],
                verts[f[k] as usize],
                verts[f[k + 1] as usize],
            ]);
        }
        for (k, &a) in f.iter().enumerate() {
            let b = f[(k + 1) % f.len()];
            *edges.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let (min, max) = verts.iter().fold(
        (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)),
        |(lo, hi), v| (lo.min(*v), hi.max(*v)),
    );
    Solid {
        id: o.id,
        name: o.name.clone(),
        group: o.group.clone(),
        tris,
        verts,
        min,
        max,
        closed: !edges.is_empty() && edges.values().all(|&n| n == 2),
    }
}

/// Every object of a scene as a world-space solid, modifiers evaluated.
pub fn scene_solids(scene: &Scene) -> Result<Vec<Solid>, EngineError> {
    scene
        .objects
        .iter()
        .map(|o| {
            let mesh = if o.modifiers.is_empty() {
                o.mesh.clone()
            } else {
                modifiers::evaluate(&o.mesh, &o.modifiers)?
            };
            Ok(solid(o, &mesh))
        })
        .collect()
}

/// Several solids treated as one rigid body (an assembly).
pub fn merge(parts: &[&Solid]) -> Solid {
    let mut out = Solid {
        id: parts.first().map_or(0, |p| p.id),
        name: parts.first().map_or(String::new(), |p| p.name.clone()),
        group: parts.first().and_then(|p| p.group.clone()),
        tris: Vec::new(),
        verts: Vec::new(),
        min: DVec3::splat(f64::INFINITY),
        max: DVec3::splat(f64::NEG_INFINITY),
        closed: parts.iter().all(|p| p.closed),
    };
    for p in parts {
        out.tris.extend_from_slice(&p.tris);
        out.verts.extend_from_slice(&p.verts);
        out.min = out.min.min(p.min);
        out.max = out.max.max(p.max);
    }
    out
}

/// World bounds of a set of objects (by id), if any has faces.
pub fn bounds(solids: &[Solid], ids: &[u64]) -> Option<(DVec3, DVec3)> {
    solids
        .iter()
        .filter(|s| ids.contains(&s.id) && !s.tris.is_empty())
        .map(|s| (s.min, s.max))
        .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
}

/// Every object of the editor as a world-space solid.
pub fn solids(ed: &Editor) -> Vec<Solid> {
    ed.scene()
        .objects
        .iter()
        .map(|o| solid(o, &ed.posed(o, None)))
        .filter(|s| !s.tris.is_empty())
        .collect()
}

fn sample(v: &[DVec3]) -> impl Iterator<Item = &DVec3> {
    v.iter().step_by(v.len().div_ceil(SAMPLES).max(1))
}

fn overlap(a: &Solid, b: &Solid, margin: f64) -> bool {
    (0..3).all(|k| a.min[k] <= b.max[k] + margin && b.min[k] <= a.max[k] + margin)
}

fn overlap_xz(a: &Solid, b: &Solid) -> bool {
    a.min.x <= b.max.x && b.min.x <= a.max.x && a.min.z <= b.max.z && b.min.z <= a.max.z
}

/// Ray/triangle hit distance (Möller–Trumbore), `t > 0` only.
fn ray_hit(o: DVec3, d: DVec3, [a, b, c]: &[DVec3; 3]) -> Option<f64> {
    let (e1, e2) = (*b - *a, *c - *a);
    let p = d.cross(e2);
    let det = e1.dot(p);
    if det.abs() < 1e-14 {
        return None;
    }
    let inv = 1.0 / det;
    let s = o - *a;
    let u = s.dot(p) * inv;
    let q = s.cross(e1);
    let v = d.dot(q) * inv;
    let t = e2.dot(q) * inv;
    (u >= 0.0 && v >= 0.0 && u + v <= 1.0 && t > 1e-12).then_some(t)
}

/// Whether `p` is inside a closed solid (parity of hits along a skewed ray).
fn inside(s: &Solid, p: DVec3) -> bool {
    if (0..3).any(|k| p[k] < s.min[k] || p[k] > s.max[k]) {
        return false;
    }
    let d = DVec3::new(1.0, 0.000_731, 0.001_37).normalize();
    s.tris.iter().filter(|t| ray_hit(p, d, t).is_some()).count() % 2 == 1
}

/// Distance from `p` to the closest point of a triangle (Ericson, RTCD 5.1.5).
fn tri_distance(p: DVec3, [a, b, c]: &[DVec3; 3]) -> f64 {
    let (ab, ac, ap) = (*b - *a, *c - *a, p - *a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return p.distance(*a);
    }
    let bp = p - *b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return p.distance(*b);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return p.distance(*a + ab * (d1 / (d1 - d3)));
    }
    let cp = p - *c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return p.distance(*c);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return p.distance(*a + ac * (d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return p.distance(*b + (*c - *b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6))));
    }
    let denom = 1.0 / (va + vb + vc);
    p.distance(*a + ab * (vb * denom) + ac * (vc * denom))
}

/// How deep `a` and `b` interpenetrate: the furthest sampled vertex of one
/// that lies inside the other, measured to the other's surface.
pub fn penetration(a: &Solid, b: &Solid) -> f64 {
    if !overlap(a, b, 0.0) {
        return 0.0;
    }
    let depth = |x: &Solid, y: &Solid| {
        if !y.closed {
            return 0.0;
        }
        sample(&x.verts)
            .filter(|p| inside(y, **p))
            .map(|p| {
                y.tris
                    .iter()
                    .map(|t| tri_distance(*p, t))
                    .fold(f64::INFINITY, f64::min)
            })
            .fold(0.0, f64::max)
    };
    depth(a, b).max(depth(b, a))
}

/// Heights where the vertical line through (x, z) crosses a triangle.
fn vertical_hit(x: f64, z: f64, [a, b, c]: &[DVec3; 3]) -> Option<f64> {
    let det = (b.z - c.z) * (a.x - c.x) + (c.x - b.x) * (a.z - c.z);
    if det.abs() < 1e-14 {
        return None;
    }
    let l1 = ((b.z - c.z) * (x - c.x) + (c.x - b.x) * (z - c.z)) / det;
    let l2 = ((c.z - a.z) * (x - c.x) + (a.x - c.x) * (z - c.z)) / det;
    let l3 = 1.0 - l1 - l2;
    (l1 >= 0.0 && l2 >= 0.0 && l3 >= 0.0).then_some(l1 * a.y + l2 * b.y + l3 * c.y)
}

/// How far `s` can move straight down before it rests on the floor or on
/// another solid (negative: it must rise), and what it would rest on. An
/// object sunk into the one below it is lifted onto that object's top,
/// rather than dropped through it.
pub fn drop_distance<'a>(s: &Solid, others: &[&'a Solid]) -> (f64, Option<&'a Solid>) {
    let mut best = (s.min.y, None);
    let mut consider = |gap: f64, b: &'a Solid| {
        if gap < best.0 {
            best = (gap, Some(b));
        }
    };
    let highest = |b: &Solid, x: f64, z: f64, limit: f64| {
        b.tris
            .iter()
            .filter_map(|t| vertical_hit(x, z, t))
            .filter(|h| *h <= limit)
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let lowest = |b: &Solid, x: f64, z: f64, limit: f64| {
        b.tris
            .iter()
            .filter_map(|t| vertical_hit(x, z, t))
            .filter(|h| *h >= limit)
            .fold(f64::INFINITY, f64::min)
    };
    for &b in others.iter().filter(|b| b.id != s.id && overlap_xz(s, b)) {
        if s.closed && b.closed && penetration(s, b) > TOLERANCE {
            // Only the upper of two intersecting objects moves: it rises until
            // it sits on b's top (side-by-side overlaps are b's to resolve).
            if (s.min.y + s.max.y) > (b.min.y + b.max.y) + 2.0 * TOLERANCE {
                for v in sample(&s.verts) {
                    consider(v.y - highest(b, v.x, v.z, f64::INFINITY), b);
                }
                for u in sample(&b.verts).filter(|u| {
                    u.x >= s.min.x && u.x <= s.max.x && u.z >= s.min.z && u.z <= s.max.z
                }) {
                    consider(lowest(s, u.x, u.z, f64::NEG_INFINITY) - u.y, b);
                }
            }
            continue;
        }
        for v in sample(&s.verts) {
            consider(v.y - highest(b, v.x, v.z, v.y + TOLERANCE), b);
        }
        // Peaks of `b` that poke up into the underside of `s`.
        for u in sample(&b.verts)
            .filter(|u| u.x >= s.min.x && u.x <= s.max.x && u.z >= s.min.z && u.z <= s.max.z)
        {
            consider(lowest(s, u.x, u.z, u.y - TOLERANCE) - u.y, b);
        }
    }
    best
}

fn cm(m: f64) -> String {
    format!("{:.1} cm", m * 100.0)
}

/// The inspection report: per-object measurements and a list of issues.
pub fn inspect(ed: &Editor) -> Value {
    let solids = solids(ed);
    let refs: Vec<&Solid> = solids.iter().collect();
    let mut issues = Vec::new();
    let mut objects = Vec::new();
    for (i, a) in solids.iter().enumerate() {
        for b in &solids[i + 1..] {
            if a.group.is_some() && a.group == b.group {
                continue;
            }
            let depth = penetration(a, b);
            if depth > TOLERANCE {
                // Name the upper object first: "Cup sinks 2 cm into Table".
                let (hi, lo) = if a.min.y + a.max.y >= b.min.y + b.max.y {
                    (a, b)
                } else {
                    (b, a)
                };
                let stacked = hi.min.y > lo.min.y + TOLERANCE;
                let message = if stacked {
                    format!("{} sinks {} into {}", hi.name, cm(depth), lo.name)
                } else {
                    format!("{} intersects {} by {}", a.name, b.name, cm(depth))
                };
                issues.push(json!({
                    "kind": "intersection",
                    "objects": [hi.name, lo.name],
                    "depth": round(depth),
                    "message": message,
                }));
            }
        }
        // Parts of an assembly hold each other up: test the whole group once,
        // against everything outside it.
        let (gap, on, body_name) = match &a.group {
            Some(g) => {
                let parts: Vec<&Solid> = refs
                    .iter()
                    .copied()
                    .filter(|s| s.group.as_ref() == Some(g))
                    .collect();
                let outside: Vec<&Solid> = refs
                    .iter()
                    .copied()
                    .filter(|s| s.group.as_ref() != Some(g))
                    .collect();
                let (gap, on) = drop_distance(&merge(&parts), &outside);
                (gap, on, g.clone())
            }
            None => {
                let (gap, on) = drop_distance(a, &refs);
                (gap, on, a.name.clone())
            }
        };
        let support = on.map_or("the floor".to_string(), |s| {
            s.group.clone().unwrap_or_else(|| s.name.clone())
        });
        let first_of_group = a.group.is_none()
            || refs.iter().find(|s| s.group == a.group).map(|s| s.id) == Some(a.id);
        if a.min.y < -TOLERANCE {
            issues.push(json!({
                "kind": "below_floor",
                "object": a.name,
                "depth": round(-a.min.y),
                "message": format!("{} sinks {} below the floor", a.name, cm(-a.min.y)),
            }));
        } else if gap > 0.01 && first_of_group {
            issues.push(json!({
                "kind": "floating",
                "object": body_name,
                "gap": round(gap),
                "message": format!("{} floats {} above {}", body_name, cm(gap), support),
            }));
        }
        let size = a.max - a.min;
        objects.push(json!({
            "id": a.id,
            "name": a.name,
            "group": a.group,
            "min": a.min.to_array().map(round),
            "max": a.max.to_array().map(round),
            "size": size.to_array().map(round),
            "rests_on": (gap.abs() <= 0.01).then_some(support),
            "closed": a.closed,
        }));
    }
    issues.extend(crate::constraint::violations(
        ed.scene(),
        &ed.scene().constraints,
    ));
    let summary = match issues.len() {
        0 => "no issues".to_string(),
        1 => "1 issue".to_string(),
        n => format!("{n} issues"),
    };
    json!({ "revision": ed.scene().revision, "summary": summary, "issues": issues, "objects": objects })
}

fn round(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

/// The vertical move that settles the objects `ids` as one rigid body onto
/// the floor or whatever is beneath them (see [`drop_distance`]).
pub fn settle(scene_solids: &[Solid], ids: &[u64]) -> Result<f64, EngineError> {
    let parts: Vec<&Solid> = scene_solids
        .iter()
        .filter(|s| ids.contains(&s.id) && !s.tris.is_empty())
        .collect();
    if parts.is_empty() {
        return Err(EngineError::new("nothing with faces to drop"));
    }
    let body = merge(&parts);
    let rest: Vec<&Solid> = scene_solids
        .iter()
        .filter(|s| !ids.contains(&s.id))
        .collect();
    Ok(-drop_distance(&body, &rest).0)
}

/// Like [`settle`], but only the objects `onto` (and the floor) can hold
/// the body up.
pub fn settle_onto(scene_solids: &[Solid], ids: &[u64], onto: &[u64]) -> Result<f64, EngineError> {
    let parts: Vec<&Solid> = scene_solids
        .iter()
        .filter(|s| ids.contains(&s.id) && !s.tris.is_empty())
        .collect();
    if parts.is_empty() {
        return Err(EngineError::new("nothing with faces to drop"));
    }
    let body = merge(&parts);
    let rest: Vec<&Solid> = scene_solids
        .iter()
        .filter(|s| onto.contains(&s.id))
        .collect();
    Ok(-drop_distance(&body, &rest).0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CommandBatch;

    fn editor(commands: Value) -> Editor {
        let mut ed = Editor::new();
        ed.apply(&serde_json::from_value::<CommandBatch>(json!({ "commands": commands })).unwrap())
            .unwrap();
        ed
    }

    #[test]
    fn reports_intersections_floating_and_sinking() {
        let ed = editor(json!([
            {"op": "add", "name": "Table", "primitive": {"kind": "cube"}, "translation": [0, 0.5, 0]},
            {"op": "add", "name": "Ball", "primitive": {"kind": "sphere", "radius": 0.2}, "translation": [0, 1.35, 0]},
            {"op": "add", "name": "Box", "primitive": {"kind": "cube", "size": 0.4}, "translation": [0.6, 0.5, 0]},
            {"op": "add", "name": "Sunk", "primitive": {"kind": "cube", "size": 0.4}, "translation": [3, 0.1, 0]}
        ]));
        let r = inspect(&ed);
        let messages: Vec<&str> = r["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["message"].as_str().unwrap())
            .collect();
        assert!(
            messages.contains(&"Table intersects Box by 10.0 cm"),
            "{messages:?}"
        );
        assert!(
            messages.contains(&"Ball floats 15.0 cm above Table"),
            "{messages:?}"
        );
        assert!(
            messages.contains(&"Sunk sinks 10.0 cm below the floor"),
            "{messages:?}"
        );
        assert_eq!(r["objects"][0]["rests_on"], "the floor");
    }

    #[test]
    fn drop_settles_onto_the_surface_below() {
        let mut ed = editor(json!([
            {"op": "add", "name": "Table", "primitive": {"kind": "cube"}, "translation": [0, 0.5, 0]},
            {"op": "add", "name": "Ball", "primitive": {"kind": "sphere", "radius": 0.2, "segments": 48, "rings": 24}, "translation": [0.1, 1.6, 0]},
            {"op": "add", "name": "Cup", "primitive": {"kind": "cube", "size": 0.2}, "translation": [-0.3, 0.95, 0]},
            {"op": "add", "name": "Low", "primitive": {"kind": "cube", "size": 0.2}, "translation": [2, -0.05, 0]},
            {"op": "add", "name": "Book", "primitive": {"kind": "cube"}, "translation": [-3, 0.04, 0], "scale": [0.5, 0.08, 0.35]},
            {"op": "add", "name": "Apple", "primitive": {"kind": "sphere", "radius": 0.12, "segments": 32, "rings": 16}, "translation": [-3, 0.1, 0]}
        ]));
        ed.apply(
            &serde_json::from_value::<CommandBatch>(json!({"commands": [
                {"op": "drop", "id": "Ball"}, {"op": "drop", "id": "Cup"}, {"op": "drop", "id": "Low"},
                {"op": "drop", "id": "Apple"}
            ]}))
            .unwrap(),
        )
        .unwrap();
        let y = |name: &str| {
            ed.scene()
                .objects
                .iter()
                .find(|o| o.name == name)
                .unwrap()
                .transform
                .translation[1]
        };
        assert!(
            (y("Ball") - 1.2).abs() < 0.003,
            "ball rests on the table: {}",
            y("Ball")
        );
        assert!(
            (y("Cup") - 1.1).abs() < 0.003,
            "sunk cup is lifted out: {}",
            y("Cup")
        );
        assert!(
            (y("Low") - 0.1).abs() < 1e-9,
            "lifted out of the floor: {}",
            y("Low")
        );
        assert!(
            (y("Apple") - 0.2).abs() < 0.003,
            "apple sits on the book: {}",
            y("Apple")
        );
        let r = inspect(&ed);
        assert_eq!(r["summary"], "no issues", "{}", r["issues"]);
    }
}
