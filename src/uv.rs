//! UV unwrapping and editing: lay a mesh's faces out flat in the unit
//! square so an image or node texture maps onto it exactly, like Blender's
//! UV editor.
//!
//! Unwrapping splits the faces into charts and flattens each one:
//! `smart` groups faces whose normals stay within an angle and projects
//! each group flat (Blender's Smart UV Project); `cube` groups by box side;
//! `cylinder` wraps the side around the Y axis (a can label) with planar
//! caps; `seams` cuts along the edges marked as seams and flattens each
//! piece with least-squares conformal maps (LSCM), so curved pieces unroll
//! without overlapping. Every method then packs the charts into the unit
//! square at one texel density.

use std::collections::{HashMap, HashSet, VecDeque};

use glam::{DVec2, DVec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::{EngineError, Mesh};

fn err<T>(message: impl Into<String>) -> Result<T, EngineError> {
    Err(EngineError::new(message))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    /// Group faces facing within 66° of each other; project each group flat.
    #[default]
    Smart,
    /// One chart per box side, as box projection would show it.
    Cube,
    /// The side wraps around the Y axis in one piece; the caps lie flat.
    Cylinder,
    /// Cut along the marked seams and unroll each piece (LSCM).
    Seams,
}

/// Faces flattened together: `uv[i][k]` for corner `k` of face `faces[i]`.
struct Chart {
    faces: Vec<usize>,
    uv: Vec<Vec<DVec2>>,
}

fn pos(mesh: &Mesh, v: u32) -> DVec3 {
    DVec3::from_array(mesh.vertices[v as usize])
}

/// Newell normal, scaled by twice the face area.
fn area_normal(mesh: &Mesh, f: &[u32]) -> DVec3 {
    let mut n = DVec3::ZERO;
    for k in 0..f.len() {
        n += pos(mesh, f[k]).cross(pos(mesh, f[(k + 1) % f.len()]));
    }
    n
}

fn edge(a: u32, b: u32) -> (u32, u32) {
    (a.min(b), a.max(b))
}

/// Every edge of the mesh, as (low, high) vertex pairs.
pub fn edges(mesh: &Mesh) -> HashSet<(u32, u32)> {
    mesh.faces
        .iter()
        .flat_map(|f| (0..f.len()).map(move |k| edge(f[k], f[(k + 1) % f.len()])))
        .collect()
}

/// Faces on each side of each edge.
fn edge_faces(mesh: &Mesh) -> HashMap<(u32, u32), Vec<usize>> {
    let mut out: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (fi, f) in mesh.faces.iter().enumerate() {
        for k in 0..f.len() {
            out.entry(edge(f[k], f[(k + 1) % f.len()]))
                .or_default()
                .push(fi);
        }
    }
    out
}

/// Grow charts over edges that are not seams, keeping faces for which
/// `accept(seed, face)` holds. Seeds are taken largest face first.
fn grow(
    mesh: &Mesh,
    faces: &[usize],
    seams: &HashSet<(u32, u32)>,
    accept: impl Fn(usize, usize) -> bool,
) -> Vec<Vec<usize>> {
    let links = edge_faces(mesh);
    let member: HashSet<usize> = faces.iter().copied().collect();
    let mut order = faces.to_vec();
    let area = |f: usize| area_normal(mesh, &mesh.faces[f]).length();
    order.sort_by(|&a, &b| area(b).total_cmp(&area(a)).then(a.cmp(&b)));
    let mut taken = HashSet::new();
    let mut charts = Vec::new();
    for seed in order {
        if !taken.insert(seed) {
            continue;
        }
        let mut chart = vec![seed];
        let mut queue = VecDeque::from([seed]);
        while let Some(f) = queue.pop_front() {
            let face = &mesh.faces[f];
            for k in 0..face.len() {
                let e = edge(face[k], face[(k + 1) % face.len()]);
                if seams.contains(&e) {
                    continue;
                }
                for &g in &links[&e] {
                    if member.contains(&g) && !taken.contains(&g) && accept(seed, g) {
                        taken.insert(g);
                        chart.push(g);
                        queue.push_back(g);
                    }
                }
            }
        }
        charts.push(chart);
    }
    charts
}

/// Flatten faces onto the plane across their average normal.
fn planar(mesh: &Mesh, faces: Vec<usize>, normal: DVec3) -> Chart {
    let n = normal.normalize_or(DVec3::Y);
    let (t, b) = n.any_orthonormal_pair();
    let uv = faces
        .iter()
        .map(|&f| {
            mesh.faces[f]
                .iter()
                .map(|&v| {
                    let p = pos(mesh, v);
                    DVec2::new(p.dot(t), p.dot(b))
                })
                .collect()
        })
        .collect();
    Chart { faces, uv }
}

fn smart(mesh: &Mesh, faces: &[usize], seams: &HashSet<(u32, u32)>) -> Vec<Chart> {
    let normals: Vec<DVec3> = mesh
        .faces
        .iter()
        .map(|f| area_normal(mesh, f).normalize_or_zero())
        .collect();
    let limit = 66f64.to_radians().cos();
    grow(mesh, faces, seams, |seed, f| {
        normals[seed].dot(normals[f]) >= limit
    })
    .into_iter()
    .map(|c| {
        let n = c.iter().map(|&f| area_normal(mesh, &mesh.faces[f])).sum();
        planar(mesh, c, n)
    })
    .collect()
}

fn cube(mesh: &Mesh) -> Vec<Chart> {
    let all: Vec<usize> = (0..mesh.faces.len()).collect();
    let sides: Vec<u8> = mesh
        .faces
        .iter()
        .map(|f| crate::texture::side(area_normal(mesh, f)))
        .collect();
    grow(mesh, &all, &HashSet::new(), |seed, f| {
        sides[seed] == sides[f]
    })
    .into_iter()
    .map(|faces| {
        let n = [
            DVec3::X,
            DVec3::NEG_X,
            DVec3::Y,
            DVec3::NEG_Y,
            DVec3::Z,
            DVec3::NEG_Z,
        ][sides[faces[0]] as usize];
        let uv = faces
            .iter()
            .map(|&f| {
                mesh.faces[f]
                    .iter()
                    .map(|&v| DVec2::from_array(crate::texture::box_uv(pos(mesh, v), n)))
                    .collect()
            })
            .collect();
        Chart { faces, uv }
    })
    .collect()
}

fn cylinder(mesh: &Mesh) -> Vec<Chart> {
    let (mut side, mut top, mut bottom) = (Vec::new(), Vec::new(), Vec::new());
    for (fi, f) in mesh.faces.iter().enumerate() {
        let n = area_normal(mesh, f).normalize_or_zero();
        if n.y > 0.7 {
            top.push(fi);
        } else if n.y < -0.7 {
            bottom.push(fi);
        } else {
            side.push(fi);
        }
    }
    let mut charts = Vec::new();
    if !side.is_empty() {
        // Arc length around the axis at the side's average radius.
        let verts: HashSet<u32> = side
            .iter()
            .flat_map(|&f| mesh.faces[f].iter().copied())
            .collect();
        let radius = verts
            .iter()
            .map(|&v| {
                let p = pos(mesh, v);
                (p.x * p.x + p.z * p.z).sqrt()
            })
            .sum::<f64>()
            / verts.len() as f64;
        let uv = side
            .iter()
            .map(|&f| {
                let face = &mesh.faces[f];
                let mut angles: Vec<f64> = face
                    .iter()
                    .map(|&v| {
                        let p = pos(mesh, v);
                        p.x.atan2(p.z)
                    })
                    .collect();
                // A face across the back seam keeps its corners together.
                let (lo, hi) = angles
                    .iter()
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &x| {
                        (a.min(x), b.max(x))
                    });
                if hi - lo > std::f64::consts::PI {
                    for a in &mut angles {
                        if *a < 0.0 {
                            *a += std::f64::consts::TAU;
                        }
                    }
                }
                face.iter()
                    .zip(angles)
                    .map(|(&v, a)| DVec2::new(a * radius.max(1e-6), pos(mesh, v).y))
                    .collect()
            })
            .collect();
        charts.push(Chart { faces: side, uv });
    }
    for (faces, n) in [(top, DVec3::Y), (bottom, DVec3::NEG_Y)] {
        if !faces.is_empty() {
            charts.push(planar(mesh, faces, n));
        }
    }
    charts
}

/// Least-squares conformal map of one chart (Lévy et al. 2002), solved by
/// conjugate gradients with two vertices pinned. `None` if it degenerates.
/// A mesh vertex on a seam becomes one chart vertex per side of the cut.
fn lscm(mesh: &Mesh, faces: &[usize], cut: &HashSet<(u32, u32)>) -> Option<Vec<Vec<DVec2>>> {
    // Corners join into one chart vertex across edges that are not cut.
    let mut start = Vec::with_capacity(faces.len());
    let mut corners = 0;
    for &f in faces {
        start.push(corners);
        corners += mesh.faces[f].len();
    }
    let mut parent: Vec<usize> = (0..corners).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    // Uncut edge -> (piece face, corner k, corner k + 1) of each face on it.
    type Sides = Vec<(usize, usize, usize)>;
    let mut by_edge: HashMap<(u32, u32), Sides> = HashMap::new();
    for (i, &f) in faces.iter().enumerate() {
        let face = &mesh.faces[f];
        for k in 0..face.len() {
            let k2 = (k + 1) % face.len();
            let e = edge(face[k], face[k2]);
            if !cut.contains(&e) {
                by_edge.entry(e).or_default().push((i, k, k2));
            }
        }
    }
    for list in by_edge.values() {
        for w in list.windows(2) {
            let ((i, a, b), (j, c, d)) = (w[0], w[1]);
            let (fi, fj) = (&mesh.faces[faces[i]], &mesh.faces[faces[j]]);
            for (x, y) in [(a, c), (a, d), (b, c), (b, d)] {
                if fi[x] == fj[y] {
                    let (r1, r2) = (
                        root(&mut parent, start[i] + x),
                        root(&mut parent, start[j] + y),
                    );
                    parent[r1] = r2;
                }
            }
        }
    }
    let mut local_of = vec![usize::MAX; corners];
    let mut verts: Vec<u32> = Vec::new();
    let mut ids: HashMap<usize, usize> = HashMap::new();
    for (i, &f) in faces.iter().enumerate() {
        for (k, &v) in mesh.faces[f].iter().enumerate() {
            let r = root(&mut parent, start[i] + k);
            let l = *ids.entry(r).or_insert_with(|| {
                verts.push(v);
                verts.len() - 1
            });
            local_of[start[i] + k] = l;
        }
    }
    let n = verts.len();
    if n < 3 {
        return None;
    }
    // Pins: the two extremes along the chart's longest extent.
    let (lo, hi) = verts.iter().fold(
        (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY)),
        |(a, b), &v| (a.min(pos(mesh, v)), b.max(pos(mesh, v))),
    );
    let span = hi - lo;
    let axis = if span.x >= span.y && span.x >= span.z {
        DVec3::X
    } else if span.y >= span.z {
        DVec3::Y
    } else {
        DVec3::Z
    };
    let along = |l: usize| pos(mesh, verts[l]).dot(axis);
    let pin_a = (0..n).min_by(|&a, &b| along(a).total_cmp(&along(b)))?;
    let pin_b = (0..n).max_by(|&a, &b| along(a).total_cmp(&along(b)))?;
    if pin_a == pin_b {
        return None;
    }
    let pinned = |l: usize| l == pin_a || l == pin_b;
    let pin_value = |l: usize| -> DVec2 {
        if l == pin_a {
            DVec2::ZERO
        } else {
            DVec2::new(1.0, 0.0)
        }
    };
    // Unknowns: u then v of each free vertex.
    let mut free = vec![usize::MAX; n];
    let mut count = 0;
    for (l, slot) in free.iter_mut().enumerate() {
        if !pinned(l) {
            *slot = count;
            count += 1;
        }
    }
    let dim = count * 2;
    // Rows of A (sparse) and the right-hand side from the pins.
    let mut rows: Vec<Vec<(usize, f64)>> = Vec::new();
    let mut rhs: Vec<f64> = Vec::new();
    for (i, &f) in faces.iter().enumerate() {
        let face = &mesh.faces[f];
        for k in 1..face.len() - 1 {
            let tri = [face[0], face[k], face[k + 1]];
            let tl = [0, k, k + 1].map(|c| local_of[start[i] + c]);
            let p = tri.map(|v| pos(mesh, v));
            let e1 = p[1] - p[0];
            let x1 = e1.length();
            let normal = e1.cross(p[2] - p[0]);
            if x1 < 1e-12 || normal.length() < 1e-14 {
                continue;
            }
            let ex = e1 / x1;
            let ey = normal.cross(ex).normalize();
            let (x2, y2) = ((p[2] - p[0]).dot(ex), (p[2] - p[0]).dot(ey));
            let xs = [0.0, x1, x2];
            let ys = [0.0, 0.0, y2];
            let s = 1.0 / (x1 * y2).abs().sqrt();
            let w = |j: usize| {
                let (a, b) = ((j + 1) % 3, (j + 2) % 3);
                (xs[b] - xs[a], ys[b] - ys[a])
            };
            for imag in [false, true] {
                let mut row = Vec::new();
                let mut b = 0.0;
                for (j, &l) in tl.iter().enumerate() {
                    let (wr, wi) = w(j);
                    // Real part: wr*u - wi*v; imaginary part: wi*u + wr*v.
                    let (cu, cv) = if imag {
                        (wi * s, wr * s)
                    } else {
                        (wr * s, -wi * s)
                    };
                    if pinned(l) {
                        let pv = pin_value(l);
                        b -= cu * pv.x + cv * pv.y;
                    } else {
                        row.push((free[l], cu));
                        row.push((free[l] + count, cv));
                    }
                }
                rows.push(row);
                rhs.push(b);
            }
        }
    }
    if rows.is_empty() {
        return None;
    }
    // Normal equations AᵀA x = Aᵀb, matrix-free conjugate gradients.
    let apply = |x: &[f64]| -> Vec<f64> {
        let mut out = vec![0.0; dim];
        for row in &rows {
            let ax: f64 = row.iter().map(|&(c, a)| a * x[c]).sum();
            for &(c, a) in row {
                out[c] += a * ax;
            }
        }
        out
    };
    let mut atb = vec![0.0; dim];
    for (row, &b) in rows.iter().zip(&rhs) {
        for &(c, a) in row {
            atb[c] += a * b;
        }
    }
    let mut x = vec![0.0; dim];
    let mut r = atb.clone();
    let mut p = r.clone();
    let mut rr: f64 = r.iter().map(|v| v * v).sum();
    let target = rr * 1e-20;
    // Converges in far fewer steps than unknowns; the cap bounds big charts.
    for _ in 0..(dim * 2).clamp(100, 6000) {
        if rr <= target || rr < 1e-30 {
            break;
        }
        let ap = apply(&p);
        let pap: f64 = p.iter().zip(&ap).map(|(a, b)| a * b).sum();
        if pap.abs() < 1e-300 {
            break;
        }
        let alpha = rr / pap;
        for i in 0..dim {
            x[i] += alpha * p[i];
            r[i] -= alpha * ap[i];
        }
        let next: f64 = r.iter().map(|v| v * v).sum();
        let beta = next / rr;
        rr = next;
        for i in 0..dim {
            p[i] = r[i] + beta * p[i];
        }
    }
    let at = |l: usize| -> DVec2 {
        if pinned(l) {
            pin_value(l)
        } else {
            DVec2::new(x[free[l]], x[free[l] + count])
        }
    };
    let uv: Vec<Vec<DVec2>> = faces
        .iter()
        .enumerate()
        .map(|(i, &f)| {
            (0..mesh.faces[f].len())
                .map(|k| at(local_of[start[i] + k]))
                .collect()
        })
        .collect();
    uv.iter().flatten().all(|p| p.is_finite()).then_some(uv)
}

fn seams(mesh: &Mesh) -> Vec<Chart> {
    let cut: HashSet<(u32, u32)> = mesh.seams.iter().map(|&[a, b]| edge(a, b)).collect();
    let all: Vec<usize> = (0..mesh.faces.len()).collect();
    let links = edge_faces(mesh);
    let mut charts = Vec::new();
    for piece in grow(mesh, &all, &cut, |_, _| true) {
        // A piece needs a boundary (a seam or an open edge) to unroll.
        let inside: HashSet<usize> = piece.iter().copied().collect();
        let open = piece.iter().any(|&f| {
            let face = &mesh.faces[f];
            (0..face.len()).any(|k| {
                let e = edge(face[k], face[(k + 1) % face.len()]);
                cut.contains(&e) || links[&e].iter().filter(|g| inside.contains(g)).count() < 2
            })
        });
        match open.then(|| lscm(mesh, &piece, &cut)).flatten() {
            Some(uv) => charts.push(Chart { faces: piece, uv }),
            // Closed or degenerate: fall back to smart projection.
            None => charts.extend(smart(mesh, &piece, &cut)),
        }
    }
    charts
}

fn area3(mesh: &Mesh, f: usize) -> f64 {
    area_normal(mesh, &mesh.faces[f]).length() * 0.5
}

fn area2(uv: &[DVec2]) -> f64 {
    let mut a = 0.0;
    for k in 0..uv.len() {
        let (p, q) = (uv[k], uv[(k + 1) % uv.len()]);
        a += p.x * q.y - q.x * p.y;
    }
    a * 0.5
}

/// Scale each chart to its true size, turn it to its tightest box, and
/// shelf-pack all of them into the unit square with `margin` between.
fn pack(mesh: &Mesh, charts: &mut [Chart], margin: f64) {
    for c in charts.iter_mut() {
        let flat: f64 = c.uv.iter().map(|f| area2(f)).sum();
        if flat < 0.0 {
            // Mirrored: flip so faces keep their winding.
            for p in c.uv.iter_mut().flatten() {
                p.x = -p.x;
            }
        }
        let real: f64 = c.faces.iter().map(|&f| area3(mesh, f)).sum();
        let k = (real / flat.abs().max(1e-18)).sqrt();
        let k = if k.is_finite() { k } else { 1.0 };
        // Tightest of 18 turns (every 5°).
        let pts: Vec<DVec2> = c.uv.iter().flatten().map(|p| *p * k).collect();
        let best = (0..18)
            .map(|i| (i as f64 * 5.0).to_radians())
            .min_by(|&a, &b| {
                let size = |t: f64| {
                    let rot = DVec2::from_angle(t);
                    let (lo, hi) = pts.iter().fold(
                        (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY)),
                        |(l, h), p| (l.min(rot.rotate(*p)), h.max(rot.rotate(*p))),
                    );
                    (hi - lo).x * (hi - lo).y
                };
                size(a).total_cmp(&size(b))
            })
            .unwrap_or(0.0);
        let rot = DVec2::from_angle(best);
        for p in c.uv.iter_mut().flatten() {
            *p = rot.rotate(*p * k);
        }
        let lo =
            c.uv.iter()
                .flatten()
                .fold(DVec2::splat(f64::INFINITY), |l, p| l.min(*p));
        for p in c.uv.iter_mut().flatten() {
            *p -= lo;
        }
    }
    let size = |c: &Chart| c.uv.iter().flatten().fold(DVec2::ZERO, |h, p| h.max(*p));
    let total: f64 = charts.iter().map(|c| size(c).x * size(c).y).sum();
    let pad = margin * total.sqrt().max(1e-9);
    let widest = charts.iter().map(|c| size(c).x).fold(0.0, f64::max);
    let width = widest.max(total.sqrt() * 1.15) + 2.0 * pad;
    let mut order: Vec<usize> = (0..charts.len()).collect();
    order.sort_by(|&a, &b| {
        size(&charts[b])
            .y
            .total_cmp(&size(&charts[a]).y)
            .then(a.cmp(&b))
    });
    let (mut x, mut y, mut shelf, mut used) = (pad, pad, 0.0f64, DVec2::ZERO);
    for i in order {
        let s = size(&charts[i]);
        if x > pad && x + s.x + pad > width {
            x = pad;
            y += shelf + pad;
            shelf = 0.0;
        }
        for p in charts[i].uv.iter_mut().flatten() {
            *p += DVec2::new(x, y);
        }
        used = used.max(DVec2::new(x + s.x + pad, y + s.y + pad));
        x += s.x + pad;
        shelf = shelf.max(s.y);
    }
    let scale = 1.0 / used.x.max(used.y).max(1e-12);
    for p in charts.iter_mut().flat_map(|c| c.uv.iter_mut().flatten()) {
        *p *= scale;
    }
}

/// Unwrap the mesh's faces into its `uvs` (one per face corner), packed
/// into the unit square.
pub fn unwrap(mesh: &mut Mesh, method: Method, margin: f64) -> Result<(), EngineError> {
    if !(0.0..=0.2).contains(&margin) {
        return err("margin must be between 0 and 0.2");
    }
    if mesh.faces.is_empty() {
        return err("nothing to unwrap");
    }
    let mut charts = match method {
        Method::Smart => {
            let all: Vec<usize> = (0..mesh.faces.len()).collect();
            smart(mesh, &all, &HashSet::new())
        }
        Method::Cube => cube(mesh),
        Method::Cylinder => cylinder(mesh),
        Method::Seams => seams(mesh),
    };
    pack(mesh, &mut charts, margin);
    let mut uvs: Vec<Vec<[f64; 2]>> = mesh.faces.iter().map(|f| vec![[0.0; 2]; f.len()]).collect();
    for c in &charts {
        for (&f, corners) in c.faces.iter().zip(&c.uv) {
            uvs[f] = corners.iter().map(|p| p.to_array()).collect();
        }
    }
    mesh.uvs = uvs;
    Ok(())
}

/// Move, turn (radians) and scale the UVs of `faces` (all faces when
/// empty) about the centre of their bounding box.
pub fn transform(
    mesh: &mut Mesh,
    faces: &[u32],
    offset: [f64; 2],
    rotate: f64,
    scale: f64,
) -> Result<(), EngineError> {
    if !mesh.has_uvs() {
        return err("the mesh has no UVs yet: unwrap it first");
    }
    if !(scale.is_finite() && scale > 0.0 && scale <= 100.0) || !rotate.is_finite() {
        return err("scale must be 0-100 and rotate finite");
    }
    if offset.iter().any(|x| !x.is_finite() || x.abs() > 100.0) {
        return err("offset must be finite");
    }
    let picked: Vec<usize> = if faces.is_empty() {
        (0..mesh.faces.len()).collect()
    } else {
        let mut v: Vec<usize> = faces.iter().map(|&f| f as usize).collect();
        v.sort_unstable();
        v.dedup();
        if v.iter().any(|&f| f >= mesh.faces.len()) {
            return err("face index out of range");
        }
        v
    };
    let (lo, hi) = picked.iter().flat_map(|&f| mesh.uvs[f].iter()).fold(
        (DVec2::splat(f64::INFINITY), DVec2::splat(f64::NEG_INFINITY)),
        |(l, h), p| (l.min(DVec2::from_array(*p)), h.max(DVec2::from_array(*p))),
    );
    let centre = (lo + hi) * 0.5;
    let rot = DVec2::from_angle(rotate);
    for &f in &picked {
        for p in &mut mesh.uvs[f] {
            let q = rot.rotate((DVec2::from_array(*p) - centre) * scale)
                + centre
                + DVec2::from_array(offset);
            *p = q.to_array();
        }
    }
    Ok(())
}

/// Mark (or with `clear`, unmark) edges as seams for `seams` unwrapping.
pub fn mark_seams(mesh: &mut Mesh, list: &[[u32; 2]], clear: bool) -> Result<(), EngineError> {
    let all = edges(mesh);
    let mut set: HashSet<(u32, u32)> = mesh.seams.iter().map(|&[a, b]| edge(a, b)).collect();
    for &[a, b] in list {
        let e = edge(a, b);
        if !all.contains(&e) {
            return err(format!("{a}-{b} is not an edge of the mesh"));
        }
        if clear {
            set.remove(&e);
        } else {
            set.insert(e);
        }
    }
    let mut out: Vec<[u32; 2]> = set.into_iter().map(|(a, b)| [a, b]).collect();
    out.sort_unstable();
    mesh.seams = out;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Editor, Primitive};

    fn primitive(json: serde_json::Value) -> Mesh {
        let p: Primitive = serde_json::from_value(json).unwrap();
        let mut ed = Editor::new();
        let _ = &mut ed;
        crate::engine::build_primitive(&p).unwrap()
    }

    /// Signed UV area of every face is positive and the UVs fill most of
    /// the unit square without leaving it.
    fn check_layout(m: &Mesh, min_fill: f64) {
        assert!(m.has_uvs());
        let mut fill = 0.0;
        for uv in &m.uvs {
            let pts: Vec<DVec2> = uv.iter().map(|p| DVec2::from_array(*p)).collect();
            for p in &pts {
                assert!(
                    (-1e-9..=1.0 + 1e-9).contains(&p.x) && (-1e-9..=1.0 + 1e-9).contains(&p.y),
                    "{p}"
                );
            }
            let a = area2(&pts);
            assert!(a > -1e-9, "flipped face: {a}");
            fill += a;
        }
        assert!(fill > min_fill && fill <= 1.0 + 1e-6, "fill {fill}");
    }

    #[test]
    fn unwraps_a_cube_six_ways_and_packs_it() {
        for method in [Method::Smart, Method::Cube] {
            let mut m = primitive(serde_json::json!({"kind": "cube"}));
            unwrap(&mut m, method, 0.02).unwrap();
            check_layout(&m, 0.35);
            // Six separate squares of equal size.
            let areas: Vec<f64> = m
                .uvs
                .iter()
                .map(|uv| area2(&uv.iter().map(|p| DVec2::from_array(*p)).collect::<Vec<_>>()))
                .collect();
            for a in &areas {
                assert!((a - areas[0]).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn cylinder_wraps_the_side_in_one_piece() {
        let mut m = primitive(serde_json::json!({"kind": "cylinder", "segments": 24}));
        unwrap(&mut m, Method::Cylinder, 0.02).unwrap();
        check_layout(&m, 0.3);
        // Side faces share UVs along their vertical edges except at the seam.
        let side: Vec<usize> = (0..m.faces.len())
            .filter(|&f| m.faces[f].len() == 4)
            .collect();
        assert_eq!(side.len(), 24);
        let mut shared = 0;
        for &a in &side {
            for &b in &side {
                if a < b {
                    let common: Vec<(usize, usize)> = (0..4)
                        .flat_map(|i| (0..4).map(move |j| (i, j)))
                        .filter(|&(i, j)| {
                            m.faces[a][i] == m.faces[b][j] && m.uvs[a][i] == m.uvs[b][j]
                        })
                        .collect();
                    if common.len() == 2 {
                        shared += 1;
                    }
                }
            }
        }
        assert_eq!(shared, 23, "one seam around the side");
    }

    #[test]
    fn seams_unroll_a_curved_piece_without_flips() {
        // A cylinder cut along one vertical edge and around both caps.
        let mut m =
            primitive(serde_json::json!({"kind": "cylinder", "segments": 16, "height": 2.0}));
        let all = edges(&m);
        let mut cut: Vec<[u32; 2]> = Vec::new();
        let n = m.faces.len();
        // The caps are triangle fans: cut the rings where they meet the side.
        for (&(a, b), owners) in &edge_faces(&m) {
            let lens: Vec<usize> = owners.iter().map(|&f| m.faces[f].len()).collect();
            if lens.contains(&3) && lens.contains(&4) {
                cut.push([a, b]);
            }
        }
        // One vertical edge: shared by two side quads.
        let side = m.faces.iter().find(|f| f.len() == 4).unwrap().clone();
        let vertical = (0..4)
            .map(|k| [side[k], side[(k + 1) % 4]])
            .find(|[a, b]| (m.vertices[*a as usize][1] - m.vertices[*b as usize][1]).abs() > 0.5)
            .unwrap();
        assert!(all.contains(&edge(vertical[0], vertical[1])));
        cut.push(vertical);
        mark_seams(&mut m, &cut, false).unwrap();
        unwrap(&mut m, Method::Seams, 0.02).unwrap();
        check_layout(&m, 0.25);
        // The side unrolls to a long strip: about circumference x height.
        let side_faces: Vec<usize> = (0..n).filter(|&f| m.faces[f].len() == 4).collect();
        let (lo, hi) = side_faces
            .iter()
            .flat_map(|&f| m.uvs[f].iter())
            .fold((DVec2::splat(9.0), DVec2::splat(-9.0)), |(l, h), p| {
                (l.min(DVec2::from_array(*p)), h.max(DVec2::from_array(*p)))
            });
        let ext = hi - lo;
        let ratio = ext.x.max(ext.y) / ext.x.min(ext.y);
        let expect = std::f64::consts::TAU * 0.5 / 2.0;
        assert!(
            (ratio - expect).abs() < 0.25,
            "strip ratio {ratio} vs {expect}"
        );
        assert!(mark_seams(&mut m, &[[0, 9999]], false).is_err());
    }

    #[test]
    fn transforms_islands_about_their_centre() {
        let mut m = primitive(serde_json::json!({"kind": "cube"}));
        assert!(
            transform(&mut m, &[], [0.1, 0.0], 0.0, 1.0).is_err(),
            "needs UVs"
        );
        unwrap(&mut m, Method::Cube, 0.0).unwrap();
        let before = m.uvs.clone();
        transform(&mut m, &[0], [0.0, 0.0], std::f64::consts::PI, 1.0).unwrap();
        let c = |uv: &Vec<[f64; 2]>| {
            uv.iter()
                .fold(DVec2::ZERO, |a, p| a + DVec2::from_array(*p))
                / uv.len() as f64
        };
        assert!(
            (c(&m.uvs[0]) - c(&before[0])).length() < 1e-9,
            "turned in place"
        );
        assert_ne!(m.uvs[0], before[0]);
        assert_eq!(m.uvs[1], before[1], "other faces stay");
        transform(&mut m, &[1], [0.25, -0.5], 0.0, 2.0).unwrap();
        assert!((c(&m.uvs[1]) - c(&before[1]) - DVec2::new(0.25, -0.5)).length() < 1e-9);
    }
}
