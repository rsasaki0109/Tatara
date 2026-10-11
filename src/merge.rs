//! Deterministic collapse of a connected set of base-mesh vertices.
use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::{EngineError, Mesh};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeAt {
    #[default]
    Center,
    First,
    Last,
}

fn fail<T>(message: &str) -> Result<T, EngineError> {
    Err(EngineError::new(message))
}

fn edge(a: u32, b: u32) -> [u32; 2] {
    [a.min(b), a.max(b)]
}

fn edges(mesh: &Mesh) -> Result<BTreeMap<[u32; 2], Vec<bool>>, EngineError> {
    let mut result: BTreeMap<[u32; 2], Vec<bool>> = BTreeMap::new();
    for face in &mesh.faces {
        if face.len() < 3 || face.iter().any(|&v| v as usize >= mesh.vertices.len()) {
            return fail("mesh contains an invalid face");
        }
        for (&a, &b) in face.iter().zip(face.iter().cycle().skip(1)) {
            if a == b {
                return fail("mesh contains a collapsed edge");
            }
            result.entry(edge(a, b)).or_default().push(a < b);
        }
    }
    if result
        .values()
        .any(|uses| uses.len() > 2 || (uses.len() == 2 && uses[0] == uses[1]))
    {
        return fail("merge requires manifold edges with consistent winding");
    }
    Ok(result)
}

/// Returns the merged vertex's new index. A failure leaves the mesh unchanged.
/// Corner UVs retain their first surviving corner; marked seams are remapped.
pub fn vertices(mesh: &mut Mesh, selected: &[u32], at: MergeAt) -> Result<u32, EngineError> {
    let unique: BTreeSet<u32> = selected.iter().copied().collect();
    if unique.len() < 2 {
        return fail("select at least two distinct vertices");
    }
    if unique.iter().any(|&v| v as usize >= mesh.vertices.len()) {
        return fail("selected vertex does not exist");
    }
    let has_uvs = mesh.has_uvs();
    if !mesh.uvs.is_empty() && !has_uvs {
        return fail("mesh contains invalid corner UVs");
    }
    let original_edges = edges(mesh)?;
    let retained = *unique.first().unwrap();
    let mut adjacency: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for &[a, b] in original_edges.keys() {
        if unique.contains(&a) && unique.contains(&b) {
            adjacency.entry(a).or_default().push(b);
            adjacency.entry(b).or_default().push(a);
        }
    }
    let mut reached = BTreeSet::new();
    let mut pending = vec![retained];
    while let Some(v) = pending.pop() {
        if reached.insert(v)
            && let Some(neighbors) = adjacency.get(&v)
        {
            pending.extend(neighbors);
        }
    }
    if reached != unique {
        return fail("select a connected set of vertices");
    }
    let position = match at {
        MergeAt::First => mesh.vertices[selected[0] as usize],
        MergeAt::Last => mesh.vertices[*selected.last().unwrap() as usize],
        MergeAt::Center => {
            let mut p = [0.0; 3];
            for &v in &unique {
                for (axis, value) in p.iter_mut().enumerate() {
                    *value += mesh.vertices[v as usize][axis] / unique.len() as f64;
                }
            }
            p
        }
    };
    if position.iter().any(|v| !v.is_finite()) {
        return fail("merged position must be finite");
    }
    let mut candidate = Mesh::default();
    let mut mapping = vec![0; mesh.vertices.len()];
    for (i, &p) in mesh.vertices.iter().enumerate() {
        if unique.contains(&(i as u32)) && i as u32 != retained {
            continue;
        }
        mapping[i] = candidate.vertices.len() as u32;
        candidate
            .vertices
            .push(if i as u32 == retained { position } else { p });
    }
    let merged = mapping[retained as usize];
    for &v in &unique {
        mapping[v as usize] = merged;
    }
    let mut face_keys = BTreeSet::new();
    for (fi, face) in mesh.faces.iter().enumerate() {
        let mut rebuilt = Vec::new();
        let mut uv = Vec::new();
        for (corner, &old) in face.iter().enumerate() {
            let v = mapping[old as usize];
            if rebuilt.last() == Some(&v) {
                continue;
            }
            rebuilt.push(v);
            if has_uvs {
                uv.push(mesh.uvs[fi][corner]);
            }
        }
        if rebuilt.first() == rebuilt.last() {
            rebuilt.pop();
            uv.pop();
        }
        if rebuilt.len() < 3 {
            continue;
        }
        if rebuilt.iter().copied().collect::<BTreeSet<_>>().len() != rebuilt.len() {
            return fail("merge would create a pinched face");
        }
        let mut face_key = rebuilt.clone();
        face_key.sort_unstable();
        if !face_keys.insert(face_key) {
            return fail("merge would create duplicate faces");
        }
        let origin = candidate.vertices[rebuilt[0] as usize];
        let mut normal = [0.0; 3];
        let mut extent: f64 = 0.0;
        for (&a, &b) in rebuilt.iter().zip(rebuilt.iter().cycle().skip(1)) {
            let a = candidate.vertices[a as usize];
            let b = candidate.vertices[b as usize];
            let a = std::array::from_fn::<_, 3, _>(|k| a[k] - origin[k]);
            let b = std::array::from_fn::<_, 3, _>(|k| b[k] - origin[k]);
            for value in a {
                extent = extent.max(value.abs());
            }
            normal[0] += a[1] * b[2] - a[2] * b[1];
            normal[1] += a[2] * b[0] - a[0] * b[2];
            normal[2] += a[0] * b[1] - a[1] * b[0];
        }
        if normal.iter().any(|v| !v.is_finite())
            || normal.iter().map(|v| v.abs()).fold(0.0, f64::max) <= extent * extent * 1e-12
        {
            return fail("merge would create a zero-area face");
        }
        candidate.faces.push(rebuilt);
        if has_uvs {
            candidate.uvs.push(uv);
        }
    }
    if candidate.faces.is_empty() {
        return fail("merge would remove every face");
    }
    let candidate_edges = edges(&candidate)?;
    if original_edges.values().all(|uses| uses.len() == 2)
        && candidate_edges.values().any(|uses| uses.len() != 2)
    {
        return fail("merge would open a closed mesh");
    }
    // Faces at the merged point must form one fan, including on open surfaces.
    let mut incident = BTreeSet::new();
    let mut neighbors: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (i, face) in candidate.faces.iter().enumerate() {
        if let Some(k) = face.iter().position(|&v| v == merged) {
            incident.insert(i);
            for v in [
                face[(k + face.len() - 1) % face.len()],
                face[(k + 1) % face.len()],
            ] {
                neighbors.entry(v).or_default().push(i);
            }
        }
    }
    let Some(&first) = incident.first() else {
        return fail("merged vertex would have no surface");
    };
    let mut face_neighbors: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for faces in neighbors.values() {
        if let [a, b] = faces.as_slice() {
            face_neighbors.entry(*a).or_default().push(*b);
            face_neighbors.entry(*b).or_default().push(*a);
        }
    }
    let mut fan = BTreeSet::new();
    let mut pending = vec![first];
    while let Some(i) = pending.pop() {
        if fan.insert(i)
            && let Some(neighbors) = face_neighbors.get(&i)
        {
            pending.extend(neighbors);
        }
    }
    if fan != incident {
        return fail("merge would create disconnected faces at one vertex");
    }
    let mut seams = BTreeSet::new();
    for &[a, b] in &mesh.seams {
        let (Some(&a), Some(&b)) = (mapping.get(a as usize), mapping.get(b as usize)) else {
            return fail("mesh contains an invalid seam");
        };
        let pair = edge(a, b);
        if a != b && candidate_edges.contains_key(&pair) {
            seams.insert(pair);
        }
    }
    candidate.seams = seams.into_iter().collect();
    *mesh = candidate;
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Primitive, build_primitive};

    fn cube() -> Mesh {
        build_primitive(&Primitive::Cube { size: 2.0 }).unwrap()
    }

    #[test]
    fn top_face_becomes_a_closed_pyramid() {
        let mut mesh = cube();
        assert_eq!(
            vertices(&mut mesh, &[7, 6, 2, 3], MergeAt::Center).unwrap(),
            2
        );
        assert_eq!(mesh.vertices.len(), 5);
        assert_eq!(mesh.faces.len(), 5);
        assert_eq!(mesh.vertices[2], [0.0, 1.0, 0.0]);
        assert!(edges(&mesh).unwrap().values().all(|uses| uses.len() == 2));
        assert_eq!(mesh.faces.iter().filter(|f| f.len() == 3).count(), 4);
    }

    #[test]
    fn edge_collapse_preserves_closed_surface_and_selection_order() {
        for (at, expected) in [
            (MergeAt::First, [-1.0, -1.0, -1.0]),
            (MergeAt::Last, [1.0, -1.0, -1.0]),
            (MergeAt::Center, [0.0, -1.0, -1.0]),
        ] {
            let mut mesh = cube();
            let merged = vertices(&mut mesh, &[0, 1, 1], at).unwrap();
            assert_eq!(mesh.vertices[merged as usize], expected);
            assert_eq!(mesh.vertices.len(), 7);
            assert_eq!(mesh.faces.len(), 6);
            assert!(edges(&mesh).unwrap().values().all(|uses| uses.len() == 2));
        }
    }

    #[test]
    fn invalid_and_disconnected_selections_are_atomic() {
        for selected in [
            vec![],
            vec![0],
            vec![0, 0],
            vec![0, 8],
            vec![0, 6],
            (0..8).collect(),
        ] {
            let mut mesh = cube();
            let before = mesh.clone();
            assert!(vertices(&mut mesh, &selected, MergeAt::Center).is_err());
            assert_eq!(mesh, before);
        }
    }

    #[test]
    fn corner_uvs_and_seams_survive_with_their_remapped_faces() {
        let mut mesh = cube();
        mesh.uvs = mesh
            .faces
            .iter()
            .enumerate()
            .map(|(i, face)| {
                face.iter()
                    .enumerate()
                    .map(|(k, _)| [i as f64, k as f64])
                    .collect()
            })
            .collect();
        mesh.seams = vec![[0, 1], [1, 2], [0, 3], [1, 2]];
        vertices(&mut mesh, &[0, 1], MergeAt::Center).unwrap();
        assert!(mesh.has_uvs());
        assert_eq!(mesh.uvs[1], vec![[1.0, 0.0], [1.0, 2.0], [1.0, 3.0]]);
        assert_eq!(mesh.seams, vec![[0, 1], [0, 2]]);
    }

    #[test]
    fn collapsed_tetrahedron_and_disconnected_fans_are_atomic() {
        let tetrahedron = Mesh::new(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            vec![vec![0, 2, 1], vec![0, 1, 3], vec![1, 2, 3], vec![2, 0, 3]],
        );
        let disconnected = Mesh::new(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [-1.0, 1.0, 0.0],
                [-1.0, 0.0, 0.0],
                [2.0, 0.0, 0.0],
                [2.0, 1.0, 0.0],
            ],
            vec![vec![0, 1, 2], vec![0, 3, 4], vec![1, 5, 6]],
        );
        for mut mesh in [tetrahedron, disconnected] {
            let before = mesh.clone();
            assert!(vertices(&mut mesh, &[0, 1], MergeAt::Center).is_err());
            assert_eq!(mesh, before);
        }
    }

    #[test]
    fn pinched_faces_bad_winding_and_bad_uvs_are_atomic() {
        let base = Mesh::new(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            vec![vec![0, 1, 2, 3], vec![0, 2, 1]],
        );
        let mut bad_uv = cube();
        bad_uv.uvs = vec![vec![[0.0, 0.0]]];
        let mut bad_winding = cube();
        bad_winding.faces[0].reverse();
        for (mut mesh, selection) in [(base, [0, 2]), (bad_uv, [0, 1]), (bad_winding, [0, 1])] {
            let before = mesh.clone();
            assert!(vertices(&mut mesh, &selection, MergeAt::Center).is_err());
            assert_eq!(mesh, before);
        }
    }
}
