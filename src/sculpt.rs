//! Sculpting: brush strokes that push base-mesh vertices with a smooth
//! falloff. With `detail` set (dynamic topology), each dab first splits the
//! edges under the brush that are longer than the detail size, so a coarse
//! mesh gains resolution exactly where it is sculpted; otherwise topology
//! never changes. `web/src/sculpt.js` mirrors this file for the live preview
//! while dragging; the committed result comes from here.

use std::collections::HashMap;

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::{EngineError, MAX_FACES, Mesh, Vec3};
use crate::modifiers::Axis;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Brush {
    /// Raise the surface along the brush's average normal (carve with `invert`).
    Draw,
    /// Swell the surface along each vertex's own normal (shrink with `invert`).
    Inflate,
    /// Relax vertices toward their neighbours.
    Smooth,
    /// Pull vertices onto the plane under the brush.
    Flatten,
    /// Move the region under the first point by `offset`.
    Grab,
}

/// Dab spacing along a stroke, as a fraction of the radius.
pub const SPACING: f64 = 0.2;
/// Height of one draw/inflate dab at full strength, as a fraction of the radius.
const DAB_HEIGHT: f64 = 0.15;
const MAX_DABS: usize = 20_000;
/// Edge-splitting rounds per dab (each splits at most one edge per face).
const REFINE_PASSES: usize = 4;
/// Edges longer than this many times the detail size are split.
const SPLIT_RATIO: f64 = 4.0 / 3.0;

pub struct Stroke<'a> {
    pub brush: Brush,
    pub points: &'a [Vec3],
    pub radius: f64,
    pub strength: f64,
    pub invert: bool,
    pub offset: Option<Vec3>,
    pub symmetry: Option<Axis>,
    /// Dynamic topology: the longest edge length to leave under the brush.
    pub detail: Option<f64>,
}

fn err<T>(message: impl Into<String>) -> Result<T, EngineError> {
    Err(EngineError::new(message))
}

/// Smoothstep falloff: 1 at the centre, 0 at the radius.
pub fn falloff(d: f64, r: f64) -> f64 {
    if d >= r {
        return 0.0;
    }
    let t = 1.0 - d / r;
    t * t * (3.0 - 2.0 * t)
}

/// Dab centres along a polyline: the first point, then one every `spacing`.
pub fn dabs(points: &[DVec3], spacing: f64) -> Vec<DVec3> {
    let mut out = vec![points[0]];
    // Distance travelled since the last dab.
    let mut carry = 0.0;
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let len = a.distance(b);
        let mut t = spacing - carry;
        while t <= len {
            out.push(a + (b - a) * (t / len));
            t += spacing;
        }
        carry = len - (t - spacing);
    }
    out
}

/// Area-weighted vertex normals (zero for unused vertices).
pub fn vertex_normals(pos: &[DVec3], faces: &[Vec<u32>]) -> Vec<DVec3> {
    let mut n = vec![DVec3::ZERO; pos.len()];
    for f in faces {
        let mut face = DVec3::ZERO;
        for (k, &i) in f.iter().enumerate() {
            face += pos[i as usize].cross(pos[f[(k + 1) % f.len()] as usize]);
        }
        for &i in f {
            n[i as usize] += face;
        }
    }
    n.iter().map(|v| v.normalize_or_zero()).collect()
}

fn neighbours(mesh: &Mesh) -> Vec<Vec<u32>> {
    vertex_links(mesh.vertices.len(), &mesh.faces)
}

fn vertex_links(count: usize, faces: &[Vec<u32>]) -> Vec<Vec<u32>> {
    let mut out = vec![Vec::new(); count];
    for f in faces {
        for (k, &a) in f.iter().enumerate() {
            let b = f[(k + 1) % f.len()];
            for (x, y) in [(a, b), (b, a)] {
                if !out[x as usize].contains(&y) {
                    out[x as usize].push(y);
                }
            }
        }
    }
    out
}

fn mirror(v: DVec3, axis: Option<Axis>) -> Option<DVec3> {
    let mut m = v;
    match axis? {
        Axis::X => m.x = -m.x,
        Axis::Y => m.y = -m.y,
        Axis::Z => m.z = -m.z,
    }
    Some(m)
}

/// Apply one dab at `c`; displacements are computed from the current
/// positions and then added, so a dab never reads its own output.
fn dab(
    pos: &mut [DVec3],
    links: &[Vec<u32>],
    faces: &[Vec<u32>],
    s: &Stroke,
    c: DVec3,
    offset: DVec3,
) {
    let r = s.radius;
    let hit: Vec<(usize, f64)> = pos
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let w = falloff(p.distance(c), r);
            (w > 0.0).then_some((i, w))
        })
        .collect();
    if hit.is_empty() {
        return;
    }
    let sign = if s.invert { -1.0 } else { 1.0 };
    let moves: Vec<(usize, DVec3)> = match s.brush {
        Brush::Draw | Brush::Inflate | Brush::Flatten => {
            let n = vertex_normals(pos, faces);
            let area = hit
                .iter()
                .fold(DVec3::ZERO, |acc, &(i, w)| acc + n[i] * w)
                .normalize_or_zero();
            let height = sign * s.strength * r * DAB_HEIGHT;
            match s.brush {
                Brush::Draw => hit.iter().map(|&(i, w)| (i, area * height * w)).collect(),
                Brush::Inflate => hit.iter().map(|&(i, w)| (i, n[i] * height * w)).collect(),
                _ => {
                    let total: f64 = hit.iter().map(|&(_, w)| w).sum();
                    let centre = hit
                        .iter()
                        .fold(DVec3::ZERO, |acc, &(i, w)| acc + pos[i] * w)
                        / total;
                    hit.iter()
                        .map(|&(i, w)| {
                            let depth = (pos[i] - centre).dot(area);
                            (i, -area * depth * s.strength * w * 0.5)
                        })
                        .collect()
                }
            }
        }
        Brush::Smooth => hit
            .iter()
            .filter(|&&(i, _)| !links[i].is_empty())
            .map(|&(i, w)| {
                let avg = links[i]
                    .iter()
                    .fold(DVec3::ZERO, |acc, &j| acc + pos[j as usize])
                    / links[i].len() as f64;
                (i, (avg - pos[i]) * (s.strength * w))
            })
            .collect(),
        Brush::Grab => hit.iter().map(|&(i, w)| (i, offset * w)).collect(),
    };
    for (i, d) in moves {
        pos[i] += d;
    }
}

/// Split the triangle loop `f` (with a vertex `m` just inserted after
/// position `k`, so it has four corners) into two triangles that share the
/// edge from `m` to the opposite corner.
fn split_triangle(f: &[u32], k: usize) -> [Vec<u32>; 2] {
    let at = |i: usize| f[(k + i) % 4];
    // at(0) = a, at(1) = m, at(2) = b, at(3) = the opposite corner.
    [vec![at(0), at(1), at(3)], vec![at(1), at(2), at(3)]]
}

/// Split polygon `f` into triangles: a quad along its shorter diagonal,
/// anything larger as a fan.
fn triangulate(f: &[u32], pos: &[DVec3]) -> Vec<Vec<u32>> {
    if f.len() == 4 {
        let p = |i: usize| pos[f[i] as usize];
        return if p(0).distance_squared(p(2)) <= p(1).distance_squared(p(3)) {
            vec![vec![f[0], f[1], f[2]], vec![f[0], f[2], f[3]]]
        } else {
            vec![vec![f[0], f[1], f[3]], vec![f[1], f[2], f[3]]]
        };
    }
    (1..f.len() - 1)
        .map(|k| vec![f[0], f[k], f[k + 1]])
        .collect()
}

/// Dynamic topology for one dab at `c`: polygons under the brush become
/// triangles, then edges there longer than 4/3 `detail` are split at their
/// midpoints, longest first. A split edge's other face outside the region
/// just gains a vertex. Returns whether the mesh changed.
pub fn refine(
    pos: &mut Vec<DVec3>,
    faces: &mut Vec<Vec<u32>>,
    c: DVec3,
    radius: f64,
    detail: f64,
    max_faces: usize,
) -> bool {
    let reach = (radius + detail).powi(2);
    let near = |p: DVec3| p.distance_squared(c) < reach;
    let mut changed = false;
    let mut extra = Vec::new();
    for f in faces.iter_mut() {
        if f.len() > 3 && f.iter().any(|&i| near(pos[i as usize])) {
            let mut tris = triangulate(f, pos).into_iter();
            *f = tris.next().expect("a triangle");
            extra.extend(tris);
            changed = true;
        }
    }
    faces.extend(extra);
    let limit = detail * SPLIT_RATIO;
    for _ in 0..REFINE_PASSES {
        if faces.len() >= max_faces {
            break;
        }
        // Long edges under the brush, longest first.
        let mut long: Vec<(f64, u32, u32)> = Vec::new();
        for f in faces.iter().filter(|f| f.len() == 3) {
            for k in 0..3 {
                let (a, b) = (f[k], f[(k + 1) % 3]);
                let (pa, pb) = (pos[a as usize], pos[b as usize]);
                let len = pa.distance(pb);
                if len > limit && near((pa + pb) * 0.5) {
                    long.push((len, a.min(b), a.max(b)));
                }
            }
        }
        if long.is_empty() {
            break;
        }
        long.sort_by(|x, y| y.0.total_cmp(&x.0).then((x.1, x.2).cmp(&(y.1, y.2))));
        long.dedup_by(|x, y| (x.1, x.2) == (y.1, y.2));
        // Only faces touching a long edge's ends can own one.
        let mut ends = vec![false; pos.len()];
        for &(_, a, b) in &long {
            ends[a as usize] = true;
            ends[b as usize] = true;
        }
        let mut owners: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
        for (fi, f) in faces.iter().enumerate() {
            if !f.iter().any(|&v| ends[v as usize]) {
                continue;
            }
            for k in 0..f.len() {
                let (a, b) = (f[k], f[(k + 1) % f.len()]);
                owners.entry((a.min(b), a.max(b))).or_default().push(fi);
            }
        }
        // One split per face per round keeps the bookkeeping simple; the
        // next round (or dab) picks up the rest.
        let mut busy = vec![false; faces.len()];
        let mut added = Vec::new();
        for (_, a, b) in long {
            let owned = &owners[&(a, b)];
            if owned.iter().any(|&f| busy[f]) || faces.len() + added.len() + owned.len() > max_faces
            {
                continue;
            }
            let m = pos.len() as u32;
            pos.push((pos[a as usize] + pos[b as usize]) * 0.5);
            for &fi in owned {
                busy[fi] = true;
                let f = &mut faces[fi];
                let n = f.len();
                let k = (0..n)
                    .find(|&k| {
                        let (x, y) = (f[k], f[(k + 1) % n]);
                        (x, y) == (a, b) || (x, y) == (b, a)
                    })
                    .expect("the face owns the edge");
                f.insert(k + 1, m);
                if n == 3 {
                    let [first, second] = split_triangle(f, k);
                    *f = first;
                    added.push(second);
                }
            }
            changed = true;
        }
        faces.extend(added);
    }
    changed
}

/// Sculpt `mesh` in place with one stroke (object space).
pub fn sculpt(mesh: &mut Mesh, s: &Stroke) -> Result<(), EngineError> {
    if !(s.radius.is_finite() && s.radius > 0.0 && s.radius <= 1e4) {
        return err("radius must be positive");
    }
    if !(0.0..=1.0).contains(&s.strength) {
        return err("strength must be between 0 and 1");
    }
    if s.points.is_empty() || s.points.len() > 2000 {
        return err("a stroke needs 1 to 2000 points");
    }
    let finite = |v: &Vec3| v.iter().all(|x| x.is_finite() && x.abs() <= 1e6);
    if !s.points.iter().all(finite) || !s.offset.as_ref().is_none_or(finite) {
        return err("stroke points and offset must be finite");
    }
    if let Some(d) = s.detail
        && !(d.is_finite() && d >= s.radius / 40.0 && d <= s.radius)
    {
        return err("detail must be between radius/40 and the radius");
    }
    let offset = match (s.brush, s.offset) {
        (Brush::Grab, Some(o)) => DVec3::from(o),
        (Brush::Grab, None) => return err("the grab brush needs an offset"),
        _ => DVec3::ZERO,
    };
    let points: Vec<DVec3> = s.points.iter().map(|p| DVec3::from(*p)).collect();
    // Grab moves one region once; other brushes dab along the whole path.
    let centres = if s.brush == Brush::Grab {
        vec![points[0]]
    } else {
        let length: f64 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
        if length / (s.radius * SPACING) > MAX_DABS as f64 {
            return err(format!(
                "stroke is too long for its radius (over {MAX_DABS} dabs)"
            ));
        }
        dabs(&points, s.radius * SPACING)
    };
    let mut links = neighbours(mesh);
    let mut pos: Vec<DVec3> = mesh.vertices.iter().map(|v| DVec3::from(*v)).collect();
    let mut faces = std::mem::take(&mut mesh.faces);
    // Grab moves what is there; every other brush can add detail first.
    let detail = s.detail.filter(|_| s.brush != Brush::Grab);
    let mut apply = |pos: &mut Vec<DVec3>, faces: &mut Vec<Vec<u32>>, c: DVec3, o: DVec3| {
        if let Some(d) = detail
            && refine(pos, faces, c, s.radius, d, MAX_FACES)
            && s.brush == Brush::Smooth
        {
            links = vertex_links(pos.len(), faces);
        }
        dab(pos, &links, faces, s, c, o);
    };
    for c in centres {
        apply(&mut pos, &mut faces, c, offset);
        if let (Some(m), Some(o)) = (mirror(c, s.symmetry), mirror(offset, s.symmetry)) {
            apply(&mut pos, &mut faces, m, o);
        }
    }
    mesh.vertices = pos.into_iter().map(|p| p.to_array()).collect();
    mesh.faces = faces;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(n: usize) -> Mesh {
        // An n x n patch of quads in the XZ plane, facing +Y.
        let mut vertices = Vec::new();
        for z in 0..=n {
            for x in 0..=n {
                vertices.push([x as f64 / n as f64 - 0.5, 0.0, z as f64 / n as f64 - 0.5]);
            }
        }
        let at = |x: usize, z: usize| (z * (n + 1) + x) as u32;
        let mut faces = Vec::new();
        for z in 0..n {
            for x in 0..n {
                faces.push(vec![at(x, z), at(x, z + 1), at(x + 1, z + 1), at(x + 1, z)]);
            }
        }
        Mesh::new(vertices, faces)
    }

    fn stroke(brush: Brush, points: &[Vec3]) -> Stroke<'_> {
        Stroke {
            brush,
            points,
            radius: 0.3,
            strength: 1.0,
            invert: false,
            offset: None,
            symmetry: None,
            detail: None,
        }
    }

    fn height(m: &Mesh, x: f64, z: f64) -> f64 {
        m.vertices
            .iter()
            .min_by(|a, b| {
                let d = |v: &&Vec3| (v[0] - x).powi(2) + (v[2] - z).powi(2);
                d(a).total_cmp(&d(b))
            })
            .unwrap()[1]
    }

    #[test]
    fn dabs_are_evenly_spaced_across_segments() {
        let pts = [
            DVec3::ZERO,
            DVec3::new(0.25, 0.0, 0.0),
            DVec3::new(0.25, 0.0, 0.5),
        ];
        let d = dabs(&pts, 0.1);
        assert_eq!(d.len(), 8);
        for w in d.windows(2) {
            let step = (w[1] - w[0]).length();
            assert!(step <= 0.1 + 1e-9, "{step}");
        }
        assert!((d[3] - DVec3::new(0.25, 0.0, 0.05)).length() < 1e-9);
    }

    #[test]
    fn draw_raises_and_invert_carves_along_the_stroke() {
        let mut m = grid(24);
        sculpt(
            &mut m,
            &stroke(Brush::Draw, &[[-0.2, 0.0, 0.0], [0.2, 0.0, 0.0]]),
        )
        .unwrap();
        assert!(
            height(&m, 0.0, 0.0) > 0.05,
            "ridge: {}",
            height(&m, 0.0, 0.0)
        );
        assert_eq!(height(&m, 0.0, 0.45), 0.0, "outside the radius");
        let mut s = stroke(Brush::Draw, &[[0.0, 0.0, 0.0]]);
        s.invert = true;
        let mut m = grid(24);
        sculpt(&mut m, &s).unwrap();
        assert!(height(&m, 0.0, 0.0) < 0.0);
        assert_eq!(m.faces, grid(24).faces, "topology is untouched");
    }

    #[test]
    fn smooth_flatten_grab_and_symmetry() {
        let mut m = grid(24);
        sculpt(&mut m, &stroke(Brush::Draw, &[[0.0, 0.0, 0.0]])).unwrap();
        let peak = height(&m, 0.0, 0.0);
        sculpt(&mut m, &stroke(Brush::Smooth, &[[0.0, 0.0, 0.0]])).unwrap();
        assert!(height(&m, 0.0, 0.0) < peak, "smooth lowers a peak");
        let before = height(&m, 0.0, 0.0);
        sculpt(&mut m, &stroke(Brush::Flatten, &[[0.0, 0.0, 0.0]])).unwrap();
        assert!(height(&m, 0.0, 0.0) < before, "flatten pulls the bump down");

        let mut m = grid(24);
        let mut s = stroke(Brush::Grab, &[[0.25, 0.0, 0.0]]);
        s.offset = Some([0.0, 0.2, 0.0]);
        s.symmetry = Some(Axis::X);
        sculpt(&mut m, &s).unwrap();
        assert!((height(&m, 0.25, 0.0) - 0.2).abs() < 0.03);
        assert!((height(&m, -0.25, 0.0) - 0.2).abs() < 0.03, "mirrored");
        s.offset = None;
        assert!(sculpt(&mut m, &s).is_err(), "grab needs an offset");
    }

    /// Every edge is shared by at most two faces, each way round once.
    fn assert_manifold(m: &Mesh) {
        let mut seen = std::collections::HashSet::new();
        for f in &m.faces {
            assert!(f.len() >= 3);
            for k in 0..f.len() {
                assert!(
                    seen.insert((f[k], f[(k + 1) % f.len()])),
                    "edge used twice the same way"
                );
            }
        }
    }

    #[test]
    fn dynamic_detail_is_fast_on_big_meshes() {
        let mut m = grid(100);
        let mut s = stroke(Brush::Draw, &[[-0.4, 0.0, 0.0], [0.4, 0.0, 0.2]]);
        s.radius = 0.05;
        s.detail = Some(0.004);
        let t = std::time::Instant::now();
        sculpt(&mut m, &s).unwrap();
        assert!(m.faces.len() > 10_000 + 1000);
        // Generous for debug builds on slow runners; a quadratic pass would
        // take minutes.
        assert!(t.elapsed().as_secs_f64() < 20.0, "took {:?}", t.elapsed());
        assert_manifold(&m);
    }

    #[test]
    fn dynamic_detail_adds_faces_only_under_the_brush() {
        let mut m = grid(6);
        let mut s = stroke(Brush::Draw, &[[-0.1, 0.0, 0.0], [0.1, 0.0, 0.0]]);
        s.radius = 0.2;
        s.detail = Some(0.03);
        sculpt(&mut m, &s).unwrap();
        assert!(m.faces.len() > 200, "refined: {} faces", m.faces.len());
        assert_manifold(&m);
        // Edges near the stroke are short (the last dab may stretch them a
        // little after splitting); far corners keep their quads.
        let far = m.faces.iter().filter(|f| f.len() == 4).count();
        assert!(far > 10, "untouched quads stay: {far}");
        let mut longest = 0.0f64;
        for f in &m.faces {
            for k in 0..f.len() {
                let a = DVec3::from(m.vertices[f[k] as usize]);
                let b = DVec3::from(m.vertices[f[(k + 1) % f.len()] as usize]);
                if ((a + b) * 0.5).length() < 0.08 {
                    longest = longest.max(a.distance(b));
                }
            }
        }
        assert!(
            longest <= 0.03 * SPLIT_RATIO * 1.5,
            "longest edge near the stroke: {longest}"
        );
        assert!(height(&m, 0.0, 0.0) > 0.02, "and the brush still draws");
        // Already fine enough: another stroke adds nothing new there.
        let n = m.faces.len();
        let mut again = stroke(Brush::Smooth, &[[0.0, 0.0, 0.0]]);
        again.radius = 0.05;
        again.detail = Some(0.03);
        sculpt(&mut m, &again).unwrap();
        assert!(m.faces.len() - n < 20, "{} new faces", m.faces.len() - n);
        assert_manifold(&m);

        let mut bad = stroke(Brush::Draw, &[[0.0, 0.0, 0.0]]);
        bad.detail = Some(0.0001);
        assert!(
            sculpt(&mut grid(4), &bad).is_err(),
            "detail too fine for the radius"
        );
    }
}
