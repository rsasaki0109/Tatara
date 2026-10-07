//! Non-destructive modifier stack. An object's `mesh` is the editable base;
//! its modifiers are evaluated in order to produce the mesh that is shown,
//! measured and exported. Editing the base or a modifier parameter
//! re-evaluates the stack, so nothing is baked until `apply_modifiers`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::{EngineError, MAX_FACES, Mesh, Vec3, catmull_clark};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    X,
    Y,
    Z,
}

impl Axis {
    fn index(self) -> usize {
        match self {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        }
    }
}

fn d_merge() -> f64 {
    0.001
}

fn d_levels() -> u32 {
    2
}

/// One step of an object's modifier stack, evaluated in object space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Modifier {
    /// Mirror across the object's local plane through the origin. Vertices
    /// within `merge` of the plane are welded, and faces lying on it removed.
    Mirror {
        axis: Axis,
        #[serde(default = "d_merge")]
        merge: f64,
    },
    /// Catmull-Clark subdivision surface, 1 to 4 levels.
    Subdivision {
        #[serde(default = "d_levels")]
        levels: u32,
    },
    /// `count` copies in total, each shifted by `offset` from the previous.
    Array { count: u32, offset: Vec3 },
    /// Rotate around Y, from 0 at the lowest point to `angle` radians at the top.
    Twist { angle: f64 },
    /// Scale X and Z from 1 at the lowest point to `factor` at the top.
    Taper { factor: f64 },
}

impl Modifier {
    pub fn validate(&self) -> Result<(), EngineError> {
        let ok = match self {
            Modifier::Mirror { merge, .. } => merge.is_finite() && (0.0..=1.0).contains(merge),
            Modifier::Subdivision { levels } => (1..=4).contains(levels),
            Modifier::Array { count, offset } => {
                (1..=100).contains(count) && offset.iter().all(|v| v.is_finite() && v.abs() < 1e4)
            }
            Modifier::Twist { angle } => {
                angle.is_finite() && angle.abs() <= 8.0 * std::f64::consts::PI
            }
            Modifier::Taper { factor } => factor.is_finite() && (0.0..=10.0).contains(factor),
        };
        if ok {
            return Ok(());
        }
        Err(EngineError::new(match self {
            Modifier::Mirror { .. } => "mirror merge must be between 0 and 1",
            Modifier::Subdivision { .. } => "subdivision levels must be between 1 and 4",
            Modifier::Array { .. } => "array count must be 1-100 with a finite offset",
            Modifier::Twist { .. } => "twist angle must be within ±8π radians",
            Modifier::Taper { .. } => "taper factor must be between 0 and 10",
        }))
    }
}

/// Evaluate a modifier stack on a base mesh.
pub fn evaluate(base: &Mesh, stack: &[Modifier]) -> Result<Mesh, EngineError> {
    let mut mesh = base.clone();
    for m in stack {
        m.validate()?;
        mesh = match *m {
            Modifier::Mirror { axis, merge } => mirror(&mesh, axis, merge),
            Modifier::Subdivision { levels } => {
                for _ in 0..levels {
                    check_size(mesh.faces.iter().map(Vec::len).sum())?;
                    mesh = catmull_clark(&mesh);
                }
                mesh
            }
            Modifier::Array { count, offset } => {
                check_size(mesh.faces.len() * count as usize)?;
                array(&mesh, count, offset)
            }
            Modifier::Twist { angle } => deform(&mesh, |p, t| {
                let (s, c) = (angle * t).sin_cos();
                [p[0] * c - p[2] * s, p[1], p[0] * s + p[2] * c]
            }),
            Modifier::Taper { factor } => deform(&mesh, |p, t| {
                let k = 1.0 + (factor - 1.0) * t;
                [p[0] * k, p[1], p[2] * k]
            }),
        };
        check_size(mesh.faces.len())?;
    }
    Ok(mesh)
}

fn check_size(faces: usize) -> Result<(), EngineError> {
    if faces > MAX_FACES {
        return Err(EngineError::new(format!(
            "modifier stack would exceed {MAX_FACES} faces"
        )));
    }
    Ok(())
}

fn mirror(mesh: &Mesh, axis: Axis, merge: f64) -> Mesh {
    let k = axis.index();
    let on_plane = |i: u32| mesh.vertices[i as usize][k].abs() <= merge;
    let mut vertices = mesh.vertices.clone();
    let mut map = Vec::with_capacity(mesh.vertices.len());
    for (i, v) in mesh.vertices.iter().enumerate() {
        if v[k].abs() <= merge {
            vertices[i][k] = 0.0;
            map.push(i as u32);
        } else {
            let mut m = *v;
            m[k] = -m[k];
            map.push(vertices.len() as u32);
            vertices.push(m);
        }
    }
    // A face lying on the mirror plane would end up inside the solid.
    let kept: Vec<&Vec<u32>> = mesh
        .faces
        .iter()
        .filter(|f| !f.iter().all(|&i| on_plane(i)))
        .collect();
    let mut faces: Vec<Vec<u32>> = kept.iter().map(|f| (*f).clone()).collect();
    for f in kept {
        // Mirroring flips orientation, so reverse the loop to keep normals outward.
        faces.push(f.iter().rev().map(|&i| map[i as usize]).collect());
    }
    Mesh { vertices, faces }
}

fn array(mesh: &Mesh, count: u32, offset: Vec3) -> Mesh {
    let n = mesh.vertices.len() as u32;
    let mut out = Mesh {
        vertices: Vec::with_capacity(mesh.vertices.len() * count as usize),
        faces: Vec::with_capacity(mesh.faces.len() * count as usize),
    };
    for c in 0..count {
        let d = c as f64;
        out.vertices.extend(mesh.vertices.iter().map(|v| {
            [
                v[0] + offset[0] * d,
                v[1] + offset[1] * d,
                v[2] + offset[2] * d,
            ]
        }));
        out.faces.extend(
            mesh.faces
                .iter()
                .map(|f| f.iter().map(|&i| i + c * n).collect()),
        );
    }
    out
}

/// Move every vertex by `f(position, t)` where t runs 0..1 over the Y extent.
fn deform(mesh: &Mesh, f: impl Fn(Vec3, f64) -> Vec3) -> Mesh {
    let (lo, hi) = mesh
        .vertices
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v[1]), hi.max(v[1]))
        });
    let h = (hi - lo).max(1e-9);
    Mesh {
        vertices: mesh
            .vertices
            .iter()
            .map(|&v| f(v, (v[1] - lo) / h))
            .collect(),
        faces: mesh.faces.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Primitive, build_primitive, extrude};

    fn cube() -> Mesh {
        build_primitive(&Primitive::Cube { size: 1.0 }).unwrap()
    }

    #[test]
    fn mirror_welds_the_seam() {
        // Half a box: push the -X face onto the mirror plane.
        let mut half = cube();
        for v in &mut half.vertices {
            if v[0] < 0.0 {
                v[0] = 0.0;
            }
        }
        let m = mirror(&half, Axis::X, 0.001);
        // 8 + 4 mirrored vertices; the seam face (-X, on the plane) is dropped twice.
        assert_eq!(m.vertices.len(), 12);
        assert_eq!(m.faces.len(), 10);
        let x: Vec<f64> = m.vertices.iter().map(|v| v[0]).collect();
        assert!(x.iter().any(|&v| v < -0.4) && x.iter().any(|&v| v > 0.4));
    }

    #[test]
    fn stack_evaluates_in_order() {
        let base = cube();
        let stack = vec![
            Modifier::Array {
                count: 3,
                offset: [0.0, 1.5, 0.0],
            },
            Modifier::Twist {
                angle: std::f64::consts::FRAC_PI_2,
            },
            Modifier::Taper { factor: 0.5 },
            Modifier::Subdivision { levels: 1 },
        ];
        let m = evaluate(&base, &stack).unwrap();
        assert_eq!(m.faces.len(), 3 * 6 * 4);
        // Base mesh is untouched; the top is narrower than the bottom.
        assert_eq!(base.vertices.len(), 8);
        let top = m
            .vertices
            .iter()
            .filter(|v| v[1] > 3.0)
            .map(|v| v[0].hypot(v[2]))
            .fold(0.0, f64::max);
        let bottom = m
            .vertices
            .iter()
            .filter(|v| v[1] < 0.0)
            .map(|v| v[0].hypot(v[2]))
            .fold(0.0, f64::max);
        assert!(top < bottom * 0.75, "top {top} bottom {bottom}");
    }

    #[test]
    fn base_edits_flow_through_the_stack() {
        let mut base = cube();
        let stack = [Modifier::Mirror {
            axis: Axis::X,
            merge: 0.001,
        }];
        let before = evaluate(&base, &stack).unwrap().faces.len();
        extrude(&mut base, 2, 0.5).unwrap(); // +X face
        let after = evaluate(&base, &stack).unwrap();
        assert_eq!(after.faces.len(), before + 8);
        let min_x = after
            .vertices
            .iter()
            .map(|v| v[0])
            .fold(f64::INFINITY, f64::min);
        assert!(
            (min_x + 1.0).abs() < 1e-9,
            "mirrored extrusion reaches -1.0"
        );
    }

    #[test]
    fn rejects_bad_parameters_and_huge_results() {
        assert!(evaluate(&cube(), &[Modifier::Subdivision { levels: 9 }]).is_err());
        assert!(evaluate(&cube(), &[Modifier::Taper { factor: -1.0 }]).is_err());
        let huge = [
            Modifier::Subdivision { levels: 1 },
            Modifier::Array {
                count: 100,
                offset: [1.0, 0.0, 0.0],
            },
            Modifier::Subdivision { levels: 4 },
        ];
        assert!(evaluate(&cube(), &huge).is_err());
    }
}
