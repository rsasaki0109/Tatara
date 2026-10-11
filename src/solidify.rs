//! An inward shell retaining the original surface and closing open boundaries.
use std::collections::BTreeMap;

use glam::{DMat3, DVec3};

use crate::engine::{EngineError, MAX_FACES, Mesh, face_normal};

pub(crate) fn shell(mesh: &Mesh, thickness: f64) -> Result<Mesh, EngineError> {
    if mesh.faces.len() > MAX_FACES / 2 || mesh.vertices.len() > u32::MAX as usize / 2 {
        return Err(EngineError::new("solidify would exceed the mesh budget"));
    }
    let mut edges: BTreeMap<(u32, u32), Vec<(u32, u32)>> = BTreeMap::new();
    let mut matrices = vec![DMat3::ZERO; mesh.vertices.len()];
    let mut normals = vec![DVec3::ZERO; mesh.vertices.len()];
    for face in &mesh.faces {
        let normal = DVec3::from(face_normal(mesh, face))
            .try_normalize()
            .ok_or_else(|| EngineError::new("solidify needs non-degenerate faces"))?;
        let projection = DMat3::from_cols(normal * normal.x, normal * normal.y, normal * normal.z);
        for (k, &a) in face.iter().enumerate() {
            let b = face[(k + 1) % face.len()];
            let uses = edges.entry((a.min(b), a.max(b))).or_default();
            uses.push((a, b));
            if uses.len() > 2 || (uses.len() == 2 && uses[0] != (b, a)) {
                return Err(EngineError::new(
                    "solidify needs manifold edges with consistent face winding",
                ));
            }
            matrices[a as usize] += projection;
            normals[a as usize] += normal;
        }
    }
    let boundary: Vec<_> = edges
        .values()
        .filter(|e| e.len() == 1)
        .map(|e| e[0])
        .collect();
    if mesh.faces.len() * 2 + boundary.len() > MAX_FACES {
        return Err(EngineError::new("solidify would exceed the face budget"));
    }
    let n = mesh.vertices.len() as u32;
    let mut out = mesh.clone();
    for (i, &vertex) in mesh.vertices.iter().enumerate() {
        let matrix = matrices[i];
        let scale = matrix.x_axis.x.max(matrix.y_axis.y).max(matrix.z_axis.z);
        let offset = if scale == 0.0 {
            DVec3::ZERO // Unreferenced vertices do not define a surface normal.
        } else {
            // Least-squares face-plane offsets give equal thickness at box corners.
            // A small regularizer also handles planar and edge vertices.
            let offset =
                (matrix + DMat3::IDENTITY * (scale * 1e-12)).inverse() * normals[i] * thickness;
            if !offset.is_finite()
                || offset.length() < thickness * 1e-6
                || offset.length() > thickness * 8.0
            {
                return Err(EngineError::new(
                    "solidify cannot offset these extreme or cancelling corners",
                ));
            }
            offset
        };
        let inner = DVec3::from(vertex) - offset;
        if !inner.is_finite() {
            return Err(EngineError::new("solidify produced non-finite geometry"));
        }
        out.vertices.push(inner.to_array());
    }
    out.faces.extend(
        mesh.faces
            .iter()
            .map(|f| f.iter().rev().map(|i| i + n).collect()),
    );
    if mesh.has_uvs() {
        out.uvs
            .extend(mesh.uvs.iter().map(|uv| uv.iter().rev().copied().collect()));
    }
    out.seams
        .extend(mesh.seams.iter().map(|[a, b]| [a + n, b + n]));
    for (a, b) in boundary {
        out.faces.push(vec![b, a, a + n, b + n]);
        if mesh.has_uvs() {
            out.uvs
                .push(vec![[1.0, 0.0], [0.0, 0.0], [0.0, 1.0], [1.0, 1.0]]);
        }
        out.seams.extend([[a, a + n], [b, b + n]]);
    }
    out.seams.sort_unstable();
    out.seams.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Primitive, build_primitive};

    fn sheet() -> Mesh {
        let mut mesh = Mesh::new(
            vec![
                [-1.0, 0.0, -1.0],
                [1.0, 0.0, -1.0],
                [1.0, 0.0, 1.0],
                [-1.0, 0.0, 1.0],
            ],
            vec![vec![0, 3, 2, 1]],
        );
        mesh.uvs = vec![vec![[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]]];
        mesh.seams = vec![[0, 1]];
        mesh
    }
    #[test]
    fn open_sheet_becomes_a_watertight_uv_shell_without_editing_the_base() {
        let base = sheet();
        let out = shell(&base, 0.2).unwrap();
        assert_eq!(out.vertices.len(), 8);
        assert_eq!(out.faces.len(), 6);
        assert_eq!(&out.vertices[..4], base.vertices);
        assert!(out.vertices[4..].iter().all(|v| (v[1] + 0.2).abs() < 1e-9));
        assert!(out.has_uvs());
        assert_eq!(
            out.uvs[1],
            base.uvs[0].iter().rev().copied().collect::<Vec<_>>()
        );
        let mut edges: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for f in &out.faces {
            for (k, &a) in f.iter().enumerate() {
                let b = f[(k + 1) % f.len()];
                edges.entry((a.min(b), a.max(b))).or_default().push((a, b));
            }
        }
        assert!(
            edges
                .values()
                .all(|e| e.len() == 2 && e[0] == (e[1].1, e[1].0))
        );
        assert_eq!(base.faces.len(), 1);
    }
    #[test]
    fn box_corners_keep_face_plane_thickness_and_the_outer_surface() {
        let base = build_primitive(&Primitive::Cube { size: 1.0 }).unwrap();
        let out = shell(&base, 0.1).unwrap();
        assert_eq!(out.faces.len(), 12);
        assert_eq!(&out.vertices[..8], base.vertices);
        assert!(
            out.vertices[8..]
                .iter()
                .flatten()
                .all(|x| (x.abs() - 0.4).abs() < 1e-9)
        );
    }
    #[test]
    fn inconsistent_nonmanifold_and_degenerate_surfaces_are_rejected() {
        let base = sheet();
        let mut same = base.clone();
        same.faces.push(same.faces[0].clone());
        assert!(shell(&same, 0.1).is_err());
        let mut many = base.clone();
        many.faces.extend([vec![1, 2, 3, 0], vec![0, 3, 2, 1]]);
        assert!(shell(&many, 0.1).is_err());
        let mut flat = base;
        flat.vertices.fill([0.0; 3]);
        assert!(shell(&flat, 0.1).is_err());
    }
}
