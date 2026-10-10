//! Bounded geometric face lookup for topology-sensitive command replay.
use crate::engine::{EngineError, Mesh, Vec3};
use glam::DVec3;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FaceRef {
    /// Legacy numeric face index, retained for existing callers.
    Index(usize),
    Geometry(FaceQuery),
}

fn default_dot() -> f64 {
    0.95
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FaceQuery {
    /// Desired outward normal in base-mesh local coordinates.
    pub normal: Vec3,
    /// Original face centroid in base-mesh local coordinates.
    pub centre: Vec3,
    /// Maximum centroid displacement, in local metres (0-10000).
    pub max_distance: f64,
    /// Minimum normalized normal dot product (0-1, default 0.95).
    #[serde(default = "default_dot")]
    pub min_dot: f64,
}

impl FaceRef {
    pub fn resolve(&self, mesh: &Mesh) -> Result<usize, EngineError> {
        match self {
            Self::Index(index) => Ok(*index),
            Self::Geometry(query) => query.resolve(mesh),
        }
    }
}

impl FaceQuery {
    fn resolve(&self, mesh: &Mesh) -> Result<usize, EngineError> {
        let bad = || {
            EngineError::new(
                "geometric face reference needs a nonzero finite normal, finite centre, max_distance 0-10000 and min_dot 0-1",
            )
        };
        let normal = DVec3::from(self.normal).try_normalize().ok_or_else(bad)?;
        let centre = DVec3::from(self.centre);
        if !normal.is_finite()
            || !centre.is_finite()
            || !self.max_distance.is_finite()
            || !(0.0..=10000.0).contains(&self.max_distance)
            || !self.min_dot.is_finite()
            || !(0.0..=1.0).contains(&self.min_dot)
        {
            return Err(bad());
        }
        let mut candidates = Vec::new();
        for (index, face) in mesh.faces.iter().enumerate() {
            if face.iter().any(|i| *i as usize >= mesh.vertices.len()) {
                return Err(EngineError::new(
                    "geometric face reference encountered an invalid mesh vertex index",
                ));
            }
            if face.len() < 3 {
                continue;
            }
            let n = DVec3::from(crate::engine::face_normal(mesh, face));
            let Some(n) = n.try_normalize() else {
                continue;
            };
            if !n.is_finite() || n.dot(normal) < self.min_dot {
                continue;
            }
            let point = face
                .iter()
                .map(|i| DVec3::from(mesh.vertices[*i as usize]))
                .sum::<DVec3>()
                / face.len() as f64;
            let distance = point.distance(centre);
            if distance.is_finite() && distance <= self.max_distance {
                candidates.push((index, distance));
            }
        }
        candidates.sort_by(|a, b| a.1.total_cmp(&b.1));
        let Some(&(index, distance)) = candidates.first() else {
            return Err(EngineError::new(
                "geometric face reference has no matching face; review the normal, centre and search distance",
            ));
        };
        if candidates
            .get(1)
            .is_some_and(|other| (other.1 - distance).abs() <= 1e-8 * distance.max(1.0))
        {
            return Err(EngineError::new(
                "geometric face reference is ambiguous; narrow the location or review the topology",
            ));
        }
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn square(z: f64) -> Mesh {
        Mesh::new(
            vec![[-1., -1., z], [1., -1., z], [1., 1., z], [-1., 1., z]],
            vec![vec![0, 1, 2, 3]],
        )
    }
    fn query() -> FaceRef {
        serde_json::from_value(json!({"normal":[0,0,1],"centre":[0,0,0],"max_distance":0.1}))
            .unwrap()
    }
    fn apply(
        ed: &mut crate::engine::Editor,
        commands: serde_json::Value,
    ) -> Result<crate::engine::ApplyResult, EngineError> {
        let batch: crate::engine::CommandBatch =
            serde_json::from_value(json!({"commands":commands})).unwrap();
        ed.apply(&batch)
    }
    fn cylinder(segments: u32) -> serde_json::Value {
        json!({"op":"add","name":"Cylinder","primitive":{"kind":"cylinder","segments":segments}})
    }
    fn cap(mesh: &Mesh) -> serde_json::Value {
        let face = mesh
            .faces
            .iter()
            .find(|face| {
                DVec3::from(crate::engine::face_normal(mesh, face))
                    .normalize_or_zero()
                    .y
                    > 0.99
            })
            .unwrap();
        let centre = face
            .iter()
            .map(|i| DVec3::from(mesh.vertices[*i as usize]))
            .sum::<DVec3>()
            / face.len() as f64;
        json!({"normal":[0,1,0],"centre":centre.to_array(),"max_distance":0.1})
    }
    #[test]
    fn cylinder_detail_revisions_reidentify_the_top_surface_and_preserve_undo() {
        let mut ed = crate::engine::Editor::new();
        apply(&mut ed, json!([cylinder(9)])).unwrap();
        let face = cap(&ed.scene().objects[0].mesh);
        apply(
            &mut ed,
            json!([{"op":"extrude","id":"Cylinder","face":face,"distance":0.4}]),
        )
        .unwrap();
        let before = ed.scene().clone();
        let commands = serde_json::from_value::<crate::engine::CommandBatch>(
            json!({"commands":[cylinder(12)]}),
        )
        .unwrap()
        .commands;
        let preview = ed.revise_preview(1, commands.clone()).unwrap();
        assert_eq!(*ed.scene(), before);
        ed.revise(1, commands).unwrap();
        assert_eq!(ed.scene().objects, preview.scene().objects);
        let mesh = &ed.scene().objects[0].mesh;
        assert!(
            (mesh
                .vertices
                .iter()
                .map(|p| p[1])
                .fold(f64::NEG_INFINITY, f64::max)
                - 0.9)
                .abs()
                < 1e-6
        );
        assert_ne!(mesh.faces.len(), before.objects[0].mesh.faces.len());
        ed.undo().unwrap();
        assert_eq!(ed.scene().objects, before.objects);
    }
    #[test]
    fn failed_geometric_face_batch_preserves_scene_history_and_allocation() {
        let mut ed = crate::engine::Editor::new();
        apply(&mut ed, json!([cylinder(9)])).unwrap();
        let before = ed.scene().clone();
        let steps = ed.history().0.len();
        let error=apply(&mut ed,json!([
            {"op":"add_camera","name":"Uncommitted"},
            {"op":"extrude","id":"Cylinder","face":{"normal":[0,1,0],"centre":[0,10,0],"max_distance":0.1},"distance":0.4}
        ])).unwrap_err();
        assert!(error.message.contains("no matching"));
        assert_eq!(*ed.scene(), before);
        assert_eq!(ed.history().0.len(), steps);
    }
    #[test]
    fn inset_accepts_geometric_faces_and_normalises_the_query_normal() {
        let mut ed = crate::engine::Editor::new();
        apply(&mut ed, json!([cylinder(9)])).unwrap();
        let mut face = cap(&ed.scene().objects[0].mesh);
        face["normal"] = json!([0, 10, 0]);
        let before = ed.scene().objects[0].mesh.faces.len();
        apply(
            &mut ed,
            json!([{"op":"inset","id":"Cylinder","face":face,"fraction":0.3}]),
        )
        .unwrap();
        assert!(ed.scene().objects[0].mesh.faces.len() > before);
    }

    #[test]
    fn reorders_are_resolved_by_geometry_while_numeric_indices_stay_literal() {
        let mut mesh = square(0.);
        mesh.faces.insert(0, vec![3, 2, 1, 0]);
        assert_eq!(query().resolve(&mesh).unwrap(), 1);
        let legacy: FaceRef = serde_json::from_value(json!(0)).unwrap();
        assert_eq!(legacy.resolve(&mesh).unwrap(), 0);
    }
    #[test]
    fn equal_candidates_are_refused_instead_of_silently_choosing() {
        let mut mesh = square(0.);
        mesh.faces.push(mesh.faces[0].clone());
        assert!(
            query()
                .resolve(&mesh)
                .unwrap_err()
                .message
                .contains("ambiguous")
        );
    }
    #[test]
    fn missing_reversed_degenerate_and_far_faces_are_refused() {
        assert!(query().resolve(&square(1.)).is_err());
        let mut mesh = square(0.);
        mesh.faces[0].reverse();
        assert!(query().resolve(&mesh).is_err());
        mesh.faces[0] = vec![0, 0, 0];
        assert!(query().resolve(&mesh).is_err());
        mesh.faces.clear();
        assert!(query().resolve(&mesh).is_err());
    }
    #[test]
    fn invalid_query_values_are_refused() {
        for value in [
            json!({"normal":[0,0,0],"centre":[0,0,0],"max_distance":1}),
            json!({"normal":[0,0,1],"centre":[0,0,0],"max_distance":-1}),
            json!({"normal":[0,0,1],"centre":[0,0,0],"max_distance":1,"min_dot":2}),
        ] {
            let q: FaceRef = serde_json::from_value(value).unwrap();
            assert!(q.resolve(&square(0.)).is_err());
        }
    }
}
