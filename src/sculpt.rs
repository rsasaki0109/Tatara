//! Sculpting: brush strokes that push base-mesh vertices with a smooth
//! falloff. Topology never changes, so a stroke is cheap to undo and every
//! modifier on top keeps working. `web/src/sculpt.js` mirrors this file for
//! the live preview while dragging; the committed result comes from here.

use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::{EngineError, Mesh, Vec3};
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

pub struct Stroke<'a> {
    pub brush: Brush,
    pub points: &'a [Vec3],
    pub radius: f64,
    pub strength: f64,
    pub invert: bool,
    pub offset: Option<Vec3>,
    pub symmetry: Option<Axis>,
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
    let mut out = vec![Vec::new(); mesh.vertices.len()];
    for f in &mesh.faces {
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
    let links = neighbours(mesh);
    let mut pos: Vec<DVec3> = mesh.vertices.iter().map(|v| DVec3::from(*v)).collect();
    for c in centres {
        dab(&mut pos, &links, &mesh.faces, s, c, offset);
        if let (Some(m), Some(o)) = (mirror(c, s.symmetry), mirror(offset, s.symmetry)) {
            dab(&mut pos, &links, &mesh.faces, s, m, o);
        }
    }
    for (v, p) in mesh.vertices.iter_mut().zip(pos) {
        *v = p.to_array();
    }
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
}
