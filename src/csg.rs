//! Boolean operations on meshes (constructive solid geometry), after the
//! BSP-tree method of Evan Wallace's csg.js. Each mesh becomes a BSP tree of
//! convex polygons; clipping one tree against the other and merging the
//! surviving polygons yields the union, difference or intersection.
//!
//! The trees are stored in arenas and walked with explicit stacks, so deep
//! trees (a sphere is a chain one node per face) cannot overflow the stack.
//! Inputs should be closed; the output is welded by position and may contain
//! T-junctions where polygons were split.

use std::collections::HashMap;

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::{EngineError, MAX_FACES, Mesh};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BoolOp {
    /// Merge both volumes.
    Union,
    /// Cut the second volume out of the first.
    Difference,
    /// Keep only where the volumes overlap.
    Intersect,
}

const EPS: f64 = 1e-6;
const COPLANAR: u8 = 0;
const FRONT: u8 = 1;
const BACK: u8 = 2;
const SPANNING: u8 = 3;

#[derive(Debug, Clone, Copy)]
struct Plane {
    n: DVec3,
    w: f64,
}

impl Plane {
    fn through(a: DVec3, b: DVec3, c: DVec3) -> Option<Plane> {
        let n = (b - a).cross(c - a);
        let len = n.length();
        (len > 1e-14).then(|| {
            let n = n / len;
            Plane { n, w: n.dot(a) }
        })
    }

    fn flip(&mut self) {
        self.n = -self.n;
        self.w = -self.w;
    }
}

#[derive(Debug, Clone)]
struct Poly {
    verts: Vec<DVec3>,
    plane: Plane,
}

impl Poly {
    fn new(verts: Vec<DVec3>) -> Option<Poly> {
        // Newell normal: robust for any convex loop.
        let mut n = DVec3::ZERO;
        for (i, a) in verts.iter().enumerate() {
            let b = verts[(i + 1) % verts.len()];
            n += DVec3::new(
                (a.y - b.y) * (a.z + b.z),
                (a.z - b.z) * (a.x + b.x),
                (a.x - b.x) * (a.y + b.y),
            );
        }
        let len = n.length();
        if verts.len() < 3 || len < 1e-14 {
            return None;
        }
        let n = n / len;
        let centre = verts.iter().sum::<DVec3>() / verts.len() as f64;
        Some(Poly {
            plane: Plane {
                n,
                w: n.dot(centre),
            },
            verts,
        })
    }

    fn flip(&mut self) {
        self.verts.reverse();
        self.plane.flip();
    }
}

/// Split `poly` by `plane` into the four output lists (csg.js semantics).
fn split(
    plane: &Plane,
    poly: Poly,
    coplanar_front: &mut Vec<Poly>,
    coplanar_back: &mut Vec<Poly>,
    front: &mut Vec<Poly>,
    back: &mut Vec<Poly>,
) {
    let mut kind = 0u8;
    let types: Vec<u8> = poly
        .verts
        .iter()
        .map(|v| {
            let t = plane.n.dot(*v) - plane.w;
            let k = if t < -EPS {
                BACK
            } else if t > EPS {
                FRONT
            } else {
                COPLANAR
            };
            kind |= k;
            k
        })
        .collect();
    match kind {
        COPLANAR => {
            if plane.n.dot(poly.plane.n) > 0.0 {
                coplanar_front.push(poly);
            } else {
                coplanar_back.push(poly);
            }
        }
        FRONT => front.push(poly),
        BACK => back.push(poly),
        _ => {
            let (mut f, mut b) = (Vec::new(), Vec::new());
            let n = poly.verts.len();
            for i in 0..n {
                let j = (i + 1) % n;
                let (ti, tj) = (types[i], types[j]);
                let (vi, vj) = (poly.verts[i], poly.verts[j]);
                if ti != BACK {
                    f.push(vi);
                }
                if ti != FRONT {
                    b.push(vi);
                }
                if (ti | tj) == SPANNING {
                    let t = (plane.w - plane.n.dot(vi)) / plane.n.dot(vj - vi);
                    let v = vi + (vj - vi) * t;
                    f.push(v);
                    b.push(v);
                }
            }
            if f.len() >= 3 {
                front.push(Poly {
                    verts: f,
                    plane: poly.plane,
                });
            }
            if b.len() >= 3 {
                back.push(Poly {
                    verts: b,
                    plane: poly.plane,
                });
            }
        }
    }
}

struct Node {
    plane: Plane,
    front: Option<usize>,
    back: Option<usize>,
    polys: Vec<Poly>,
}

#[derive(Default)]
struct Bsp {
    nodes: Vec<Node>,
    /// Polygons may be split into at most this many pieces in total.
    budget: usize,
    overflow: bool,
}

/// Fragments allowed per input polygon before an operation gives up.
const SPLIT_BUDGET: usize = 40;

impl Bsp {
    fn new(polys: Vec<Poly>, budget: usize) -> Bsp {
        let mut t = Bsp {
            budget,
            ..Bsp::default()
        };
        t.build(polys);
        t
    }

    fn node(&mut self, polys: &[Poly]) -> usize {
        self.nodes.push(Node {
            plane: polys[0].plane,
            front: None,
            back: None,
            polys: Vec::new(),
        });
        self.nodes.len() - 1
    }

    /// Add polygons to the tree, growing it where they fall outside it.
    fn build(&mut self, polys: Vec<Poly>) {
        if polys.is_empty() {
            return;
        }
        if self.nodes.is_empty() {
            self.node(&polys);
        }
        let mut work = vec![(0usize, polys)];
        let mut stored: usize = self.nodes.iter().map(|n| n.polys.len()).sum();
        while let Some((i, polys)) = work.pop() {
            if stored > self.budget {
                self.overflow = true;
                return;
            }
            let plane = self.nodes[i].plane;
            let (mut f, mut b, mut here) = (Vec::new(), Vec::new(), Vec::new());
            let mut here_back = Vec::new();
            for p in polys {
                split(&plane, p, &mut here, &mut here_back, &mut f, &mut b);
            }
            stored += here.len() + here_back.len();
            self.nodes[i].polys.extend(here);
            self.nodes[i].polys.extend(here_back);
            for (side, list) in [(true, f), (false, b)] {
                if list.is_empty() {
                    continue;
                }
                let child = if side {
                    self.nodes[i].front
                } else {
                    self.nodes[i].back
                };
                let child = match child {
                    Some(c) => c,
                    None => {
                        let c = self.node(&list);
                        if side {
                            self.nodes[i].front = Some(c);
                        } else {
                            self.nodes[i].back = Some(c);
                        }
                        c
                    }
                };
                work.push((child, list));
            }
        }
    }

    /// Swap solid and empty space.
    fn invert(&mut self) {
        for n in &mut self.nodes {
            for p in &mut n.polys {
                p.flip();
            }
            n.plane.flip();
            std::mem::swap(&mut n.front, &mut n.back);
        }
    }

    /// The parts of `polys` outside this tree's solid.
    fn clip_polygons(&self, polys: Vec<Poly>) -> Vec<Poly> {
        if self.nodes.is_empty() {
            return polys;
        }
        let mut out = Vec::new();
        let mut work = vec![(0usize, polys)];
        while let Some((i, polys)) = work.pop() {
            let n = &self.nodes[i];
            let (mut f, mut b) = (Vec::new(), Vec::new());
            let (mut cf, mut cb) = (Vec::new(), Vec::new());
            for p in polys {
                split(&n.plane, p, &mut cf, &mut cb, &mut f, &mut b);
            }
            f.extend(cf);
            b.extend(cb);
            match n.front {
                Some(c) => work.push((c, f)),
                None => out.extend(f),
            }
            if let Some(c) = n.back {
                work.push((c, b));
            }
        }
        out
    }

    /// Remove every polygon of this tree that lies inside `other`.
    fn clip_to(&mut self, other: &Bsp) {
        for n in &mut self.nodes {
            let polys = std::mem::take(&mut n.polys);
            n.polys = other.clip_polygons(polys);
        }
    }

    fn all(&self) -> Vec<Poly> {
        self.nodes
            .iter()
            .flat_map(|n| n.polys.iter().cloned())
            .collect()
    }
}

/// Faces as convex polygons. A non-planar quad is split along the diagonal
/// that keeps it convex (its other corner behind the first triangle), so a
/// curved surface stays locally convex and its triangles do not cut each
/// other into slivers; other non-planar faces are fanned.
fn polygons(mesh: &Mesh) -> Vec<Poly> {
    let mut out = Vec::new();
    for f in &mesh.faces {
        let verts: Vec<DVec3> = f
            .iter()
            .map(|&i| DVec3::from(mesh.vertices[i as usize]))
            .collect();
        let Some(p) = Poly::new(verts.clone()) else {
            continue;
        };
        let flat = verts
            .iter()
            .all(|v| (p.plane.n.dot(*v) - p.plane.w).abs() < EPS);
        if flat {
            out.push(p);
            continue;
        }
        let tris: Vec<[DVec3; 3]> = if verts.len() == 4 {
            let [a, b, c, d] = [verts[0], verts[1], verts[2], verts[3]];
            let behind = Plane::through(a, b, c).is_some_and(|pl| pl.n.dot(d) - pl.w <= 0.0);
            if behind {
                vec![[a, b, c], [a, c, d]]
            } else {
                vec![[b, c, d], [b, d, a]]
            }
        } else {
            (1..verts.len() - 1)
                .map(|k| [verts[0], verts[k], verts[k + 1]])
                .collect()
        };
        out.extend(tris.into_iter().filter_map(|t| Poly::new(t.to_vec())));
    }
    out
}

/// Weld polygons back into an indexed mesh and repair T-junctions: a corner
/// of one polygon lying on another's edge is inserted into that edge, so
/// neighbours share their vertices and no pixel-wide cracks open between them.
fn to_mesh(polys: Vec<Poly>) -> Mesh {
    let mut ids: HashMap<[i64; 3], u32> = HashMap::new();
    let mut vertices: Vec<DVec3> = Vec::new();
    let mut faces: Vec<Vec<u32>> = Vec::new();
    for p in polys {
        let mut face: Vec<u32> = Vec::with_capacity(p.verts.len());
        for v in p.verts {
            let key = [v.x, v.y, v.z].map(|c| (c * 1e6).round() as i64);
            let id = *ids.entry(key).or_insert_with(|| {
                vertices.push(v);
                (vertices.len() - 1) as u32
            });
            if face.last() != Some(&id) {
                face.push(id);
            }
        }
        while face.len() > 1 && face.first() == face.last() {
            face.pop();
        }
        if face.len() >= 3 {
            faces.push(face);
        }
    }

    // Bucket vertices on a grid to find the ones lying on each edge.
    let (lo, hi) = vertices.iter().fold(
        (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)),
        |(l, h), v| (l.min(*v), h.max(*v)),
    );
    let cell = ((hi - lo).max_element() / 128.0).max(1e-4);
    let key = |v: DVec3| ((v - lo) / cell).floor().as_ivec3().to_array();
    let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
    for (i, v) in vertices.iter().enumerate() {
        grid.entry(key(*v)).or_default().push(i as u32);
    }
    let on_edge = |a: u32, b: u32| -> Vec<u32> {
        let (pa, pb) = (vertices[a as usize], vertices[b as usize]);
        let (ka, kb) = (key(pa.min(pb)), key(pa.max(pb)));
        let d = pb - pa;
        let len2 = d.length_squared();
        let mut hits: Vec<(f64, u32)> = Vec::new();
        if len2 < 1e-18 || (0..3).map(|k| (kb[k] - ka[k] + 1) as i64).product::<i64>() > 4096 {
            return Vec::new();
        }
        for x in ka[0]..=kb[0] {
            for y in ka[1]..=kb[1] {
                for z in ka[2]..=kb[2] {
                    for &i in grid.get(&[x, y, z]).into_iter().flatten() {
                        if i == a || i == b {
                            continue;
                        }
                        let v = vertices[i as usize];
                        let t = (v - pa).dot(d) / len2;
                        if t > 1e-9 && t < 1.0 - 1e-9 && (pa + d * t).distance_squared(v) < 1e-14 {
                            hits.push((t, i));
                        }
                    }
                }
            }
        }
        hits.sort_by(|x, y| x.0.total_cmp(&y.0));
        hits.into_iter().map(|(_, i)| i).collect()
    };
    for face in &mut faces {
        let mut out = Vec::with_capacity(face.len());
        for k in 0..face.len() {
            let (a, b) = (face[k], face[(k + 1) % face.len()]);
            out.push(a);
            out.extend(on_edge(a, b));
        }
        *face = out;
    }
    Mesh {
        vertices: vertices.into_iter().map(|v| v.to_array()).collect(),
        faces,
        uvs: Vec::new(),
    }
}

/// Combine two meshes given in the same space.
pub fn boolean(a: &Mesh, b: &Mesh, op: BoolOp) -> Result<Mesh, EngineError> {
    if a.faces.len() + b.faces.len() > 60_000 {
        return Err(EngineError::new(
            "boolean inputs are limited to 60000 faces together",
        ));
    }
    let (pa, pb) = (polygons(a), polygons(b));
    let budget = (pa.len() + pb.len()).max(64) * SPLIT_BUDGET;
    let mut ta = Bsp::new(pa, budget);
    let mut tb = Bsp::new(pb, budget);
    match op {
        BoolOp::Union => {
            ta.clip_to(&tb);
            tb.clip_to(&ta);
            tb.invert();
            tb.clip_to(&ta);
            tb.invert();
            ta.build(tb.all());
        }
        BoolOp::Difference => {
            ta.invert();
            ta.clip_to(&tb);
            tb.clip_to(&ta);
            tb.invert();
            tb.clip_to(&ta);
            tb.invert();
            ta.build(tb.all());
            ta.invert();
        }
        BoolOp::Intersect => {
            ta.invert();
            tb.clip_to(&ta);
            tb.invert();
            ta.clip_to(&tb);
            tb.clip_to(&ta);
            ta.build(tb.all());
            ta.invert();
        }
    }
    if ta.overflow || tb.overflow {
        return Err(EngineError::new(
            "these meshes are too intricate to combine (try fewer faces)",
        ));
    }
    let mesh = to_mesh(ta.all());
    if mesh.faces.is_empty() {
        return Err(EngineError::new(
            "the result is empty (do the objects overlap?)",
        ));
    }
    if mesh.faces.len() > MAX_FACES {
        return Err(EngineError::new(format!(
            "the result exceeds {MAX_FACES} faces"
        )));
    }
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Primitive, build_primitive};

    fn volume(m: &Mesh) -> f64 {
        // Divergence theorem over fanned triangles.
        let mut v = 0.0;
        for f in &m.faces {
            let a = DVec3::from(m.vertices[f[0] as usize]);
            for k in 1..f.len() - 1 {
                let b = DVec3::from(m.vertices[f[k] as usize]);
                let c = DVec3::from(m.vertices[f[k + 1] as usize]);
                v += a.dot(b.cross(c)) / 6.0;
            }
        }
        v
    }

    fn cube(size: f64, at: [f64; 3]) -> Mesh {
        let mut m = build_primitive(&Primitive::Cube { size }).unwrap();
        for v in &mut m.vertices {
            for k in 0..3 {
                v[k] += at[k];
            }
        }
        m
    }

    #[test]
    fn volumes_add_up_for_overlapping_cubes() {
        let (a, b) = (cube(1.0, [0.0; 3]), cube(1.0, [0.5, 0.5, 0.5]));
        let u = volume(&boolean(&a, &b, BoolOp::Union).unwrap());
        let d = volume(&boolean(&a, &b, BoolOp::Difference).unwrap());
        let i = volume(&boolean(&a, &b, BoolOp::Intersect).unwrap());
        assert!((i - 0.125).abs() < 1e-9, "intersection {i}");
        assert!((d - 0.875).abs() < 1e-9, "difference {d}");
        assert!((u - 1.875).abs() < 1e-9, "union {u}");
    }

    #[test]
    fn a_cylinder_drills_a_hole() {
        let block = cube(1.0, [0.0; 3]);
        let drill = build_primitive(&Primitive::Cylinder {
            radius: 0.25,
            radius_top: None,
            height: 2.0,
            segments: 32,
        })
        .unwrap();
        let holed = boolean(&block, &drill, BoolOp::Difference).unwrap();
        let hole = volume(&boolean(&block, &drill, BoolOp::Intersect).unwrap());
        assert!((volume(&holed) + hole - 1.0).abs() < 1e-9);
        // T-junctions are repaired: every edge is shared by exactly two faces.
        let mut edges: HashMap<(u32, u32), i32> = HashMap::new();
        for f in &holed.faces {
            for k in 0..f.len() {
                let (a, b) = (f[k], f[(k + 1) % f.len()]);
                *edges.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
        assert!(edges.values().all(|&n| n == 2), "watertight after the cut");
        assert!(
            hole > 0.18 && hole < 0.2,
            "a 0.25 m drill through 1 m: {hole}"
        );
        // Disjoint intersection is reported, not returned empty.
        assert!(boolean(&block, &cube(1.0, [5.0, 0.0, 0.0]), BoolOp::Intersect).is_err());
    }

    #[test]
    fn dense_meshes_do_not_overflow_the_stack() {
        let ball = build_primitive(&Primitive::Quadsphere {
            radius: 0.6,
            level: 4,
        })
        .unwrap();
        let r = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                boolean(&ball, &cube(1.0, [0.0; 3]), BoolOp::Intersect).map(|m| m.faces.len())
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(r.unwrap() > 100);
    }
}
