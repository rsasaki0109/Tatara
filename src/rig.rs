//! Rigs: bones that bend a mesh.
//!
//! A bone runs from its `head` (its pivot) to its `tail` in object space and
//! may hang from a parent bone. Posing a bone turns it about its head;
//! children follow. Each vertex of the displayed mesh is bound to up to four
//! nearby bones with automatic weights (closer bones pull harder) and moves
//! with their blend (linear blend skinning). Weights are computed from the
//! geometry rather than stored, so edits and modifiers never leave a rig
//! with stale weights.

use glam::{DMat4, DQuat, DVec3, EulerRot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::anim::{self, Property};
use crate::engine::{EngineError, Mesh, Object, Scene};

pub const MAX_BONES: usize = 64;
/// The most one bone turns in one `reach` iteration (radians).
pub const REACH_STEP: f64 = 0.08;
/// Influences per vertex.
pub const INFLUENCES: usize = 4;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Bone {
    /// Unique within the object.
    pub name: String,
    /// Where the bone starts (the point it turns about), object space.
    pub head: [f64; 3],
    /// Where the bone ends, object space.
    pub tail: [f64; 3],
    /// The bone this one hangs from (listed before it); it follows that
    /// bone's pose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Pose: Euler XYZ rotation about the head in radians, on the object's
    /// axes as carried along by the parent bones.
    #[serde(default)]
    pub rotation: [f64; 3],
}

/// One persistent IK relation, using stable object ids and a bone name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Target {
    pub rig: u64,
    pub target: u64,
    pub bone: String,
    pub chain: u32,
}

pub fn validate_targets(scene: &Scene) -> Result<(), EngineError> {
    if scene.ik_targets.len() > scene.objects.len() {
        return Err(EngineError::new("at most one IK target per rig"));
    }
    for (i, t) in scene.ik_targets.iter().enumerate() {
        let o = scene
            .objects
            .iter()
            .find(|o| o.id == t.rig)
            .ok_or_else(|| EngineError::new("IK target names a missing rig"))?;
        if t.target == t.rig || !scene.objects.iter().any(|o| o.id == t.target) {
            return Err(EngineError::new(
                "IK needs a different, existing target object",
            ));
        }
        if !o.bones.iter().any(|b| b.name == t.bone) {
            return Err(EngineError::new(format!(
                "{} has no IK bone {:?}; clear its target before removing that bone",
                o.name, t.bone
            )));
        }
        if !(1..=MAX_BONES as u32).contains(&t.chain) {
            return Err(EngineError::new(format!(
                "IK chain must contain 1-{MAX_BONES} bones"
            )));
        }
        if scene.ik_targets[..i].iter().any(|p| p.rig == t.rig) {
            return Err(EngineError::new("at most one IK target per rig"));
        }
    }
    Ok(())
}

/// Targets use world origins, independent of bone deformation: even mutually
/// targeted rigs have no rotational feedback. Object constraints solve first.
pub fn solve_targets(scene: &mut Scene) -> Result<(), EngineError> {
    validate_targets(scene)?;
    for t in &scene.ik_targets {
        let target = scene
            .objects
            .iter()
            .find(|o| o.id == t.target)
            .expect("validated")
            .transform
            .translation;
        let o = scene
            .objects
            .iter_mut()
            .find(|o| o.id == t.rig)
            .expect("validated");
        let end = o
            .bones
            .iter()
            .position(|b| b.name == t.bone)
            .expect("validated");
        let local = o
            .transform
            .matrix()
            .inverse()
            .transform_point3(DVec3::from(target));
        let rotations = reach(&o.bones, &rotations(o, None), end, t.chain as usize, local);
        for (bone, rotation) in o.bones.iter_mut().zip(rotations) {
            bone.rotation = rotation;
        }
    }
    Ok(())
}

/// The axis a bone chain runs along.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    X,
    Y,
    Z,
}

pub fn validate(bones: &[Bone]) -> Result<(), EngineError> {
    if bones.len() > MAX_BONES {
        return Err(EngineError::new(format!(
            "a rig holds at most {MAX_BONES} bones"
        )));
    }
    for (i, b) in bones.iter().enumerate() {
        if b.name.trim().is_empty() || b.name.len() > 64 {
            return Err(EngineError::new("bone names must be 1-64 characters"));
        }
        if bones[..i].iter().any(|o| o.name == b.name) {
            return Err(EngineError::new(format!(
                "two bones are named {:?}",
                b.name
            )));
        }
        let finite = b
            .head
            .iter()
            .chain(&b.tail)
            .chain(&b.rotation)
            .all(|v| v.is_finite() && v.abs() <= 1e6);
        if !finite {
            return Err(EngineError::new(format!(
                "bone {:?} needs finite numbers",
                b.name
            )));
        }
        if DVec3::from(b.head).distance(DVec3::from(b.tail)) < 1e-6 {
            return Err(EngineError::new(format!(
                "bone {:?} needs a tail apart from its head",
                b.name
            )));
        }
        if let Some(p) = &b.parent
            && !bones[..i].iter().any(|o| &o.name == p)
        {
            return Err(EngineError::new(format!(
                "bone {:?} hangs from {p:?}, which must be listed before it",
                b.name
            )));
        }
    }
    Ok(())
}

/// A chain of `count` bones through the middle of `mesh`, end to end along
/// `axis` (default: the mesh's longest side), each hanging from the last.
pub fn chain(mesh: &Mesh, count: u32, axis: Option<Axis>) -> Result<Vec<Bone>, EngineError> {
    if !(1..=MAX_BONES as u32).contains(&count) {
        return Err(EngineError::new(format!("a chain has 1-{MAX_BONES} bones")));
    }
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for v in &mesh.vertices {
        lo = lo.min(DVec3::from(*v));
        hi = hi.max(DVec3::from(*v));
    }
    if !lo.x.is_finite() {
        return Err(EngineError::new("the mesh has no vertices to rig"));
    }
    let size = hi - lo;
    let k = match axis {
        Some(Axis::X) => 0,
        Some(Axis::Y) => 1,
        Some(Axis::Z) => 2,
        None if size.x >= size.y && size.x >= size.z => 0,
        None if size.y >= size.z => 1,
        None => 2,
    };
    if size[k] < 1e-6 {
        return Err(EngineError::new("the mesh is flat along that axis"));
    }
    let centre = (lo + hi) * 0.5;
    let at = |t: f64| {
        let mut p = centre;
        p[k] = lo[k] + size[k] * t;
        p.to_array()
    };
    Ok((0..count)
        .map(|i| Bone {
            name: format!("Bone {}", i + 1),
            head: at(i as f64 / count as f64),
            tail: at((i + 1) as f64 / count as f64),
            parent: (i > 0).then(|| format!("Bone {i}")),
            rotation: [0.0; 3],
        })
        .collect())
}

/// Each bone's rotation at `frame` (keyed bones follow their tracks), or
/// the static pose without a frame.
pub fn rotations(o: &Object, frame: Option<f64>) -> Vec<[f64; 3]> {
    o.bones
        .iter()
        .map(|b| {
            let track = frame.and_then(|_| {
                o.tracks.iter().find(|t| {
                    t.property == Property::Bone
                        && t.bone.as_deref() == Some(b.name.as_str())
                        && !t.keys.is_empty()
                })
            });
            match (track, frame) {
                (Some(t), Some(f)) => {
                    let v = anim::sample_track(t, f);
                    [v[0], v[1], v[2]]
                }
                _ => b.rotation,
            }
        })
        .collect()
}

/// Each bone's posed transform (object space, rest to posed).
pub fn matrices(bones: &[Bone], rotations: &[[f64; 3]]) -> Vec<DMat4> {
    let mut out: Vec<DMat4> = Vec::with_capacity(bones.len());
    for (b, r) in bones.iter().zip(rotations) {
        let head = DVec3::from(b.head);
        let local = DMat4::from_translation(head)
            * DMat4::from_quat(DQuat::from_euler(EulerRot::XYZ, r[0], r[1], r[2]))
            * DMat4::from_translation(-head);
        let parent = b
            .parent
            .as_ref()
            .and_then(|p| bones.iter().position(|o| &o.name == p))
            .map_or(DMat4::IDENTITY, |i| out[i]);
        out.push(parent * local);
    }
    out
}

fn segment_distance(p: DVec3, a: DVec3, b: DVec3) -> f64 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared().max(1e-12)).clamp(0.0, 1.0);
    p.distance(a + ab * t)
}

/// Automatic weights: each vertex follows its nearest bones, a bone's pull
/// falling off with the square of its distance, so joints bend smoothly. Up to four
/// `(bone, weight)` pairs per vertex, weights summing to one (unused slots
/// have weight 0).
pub fn weights(mesh: &Mesh, bones: &[Bone]) -> Vec<[(u16, f32); INFLUENCES]> {
    let segments: Vec<(DVec3, DVec3)> = bones
        .iter()
        .map(|b| (DVec3::from(b.head), DVec3::from(b.tail)))
        .collect();
    mesh.vertices
        .iter()
        .map(|v| {
            let p = DVec3::from(*v);
            let mut near: Vec<(usize, f64)> = segments
                .iter()
                .enumerate()
                .map(|(i, (a, b))| (i, segment_distance(p, *a, *b)))
                .collect();
            near.sort_by(|a, b| a.1.total_cmp(&b.1));
            let closest = near.first().map_or(0.0, |n| n.1);
            let scale = closest.max(1e-4);
            let mut out = [(0u16, 0f32); INFLUENCES];
            let mut total = 0.0;
            for (slot, (i, d)) in out.iter_mut().zip(&near) {
                // Bones much further than the nearest one do not pull.
                if *d > closest * 3.0 + 1e-9 && *i != near[0].0 {
                    break;
                }
                let w = 1.0 / (d / scale).powi(2).max(1e-12);
                *slot = (*i as u16, w as f32);
                total += w;
            }
            for slot in &mut out {
                slot.1 = (slot.1 as f64 / total.max(1e-12)) as f32;
            }
            out
        })
        .collect()
}

/// `mesh` bent by the bones at `rotations` (linear blend skinning).
pub fn skin(mesh: &Mesh, bones: &[Bone], rotations: &[[f64; 3]]) -> Mesh {
    let m = matrices(bones, rotations);
    let w = weights(mesh, bones);
    let mut out = mesh.clone();
    for (v, influences) in out.vertices.iter_mut().zip(&w) {
        let p = DVec3::from(*v);
        let mut q = DVec3::ZERO;
        for (bone, weight) in influences {
            if *weight > 0.0 {
                q += m[*bone as usize].transform_point3(p) * *weight as f64;
            }
        }
        *v = q.to_array();
    }
    out
}

/// Inverse kinematics: turn bone `end` and up to `chain - 1` of its
/// ancestors (fewer at the root) so the tip of `end` reaches `target`
/// (object space), by cyclic coordinate descent: from the tip bone up,
/// each bone turns a little (at most `REACH_STEP`) to point the tip at the
/// target, repeated until it arrives. Small steps spread the bend along the
/// chain instead of curling the last bones. Returns the new rotations;
/// bones outside the chain keep theirs.
pub fn reach(
    bones: &[Bone],
    rotations: &[[f64; 3]],
    end: usize,
    chain: usize,
    target: DVec3,
) -> Vec<[f64; 3]> {
    let mut rot = rotations.to_vec();
    let mut joints = vec![end];
    while joints.len() < chain.max(1) {
        let last = &bones[*joints.last().expect("not empty")];
        match last
            .parent
            .as_ref()
            .and_then(|p| bones.iter().position(|b| &b.name == p))
        {
            Some(k) => joints.push(k),
            None => break,
        }
    }
    let tail = DVec3::from(bones[end].tail);
    let parent_of = |k: usize| {
        bones[k]
            .parent
            .as_ref()
            .and_then(|p| bones.iter().position(|b| &b.name == p))
    };
    for _ in 0..400 {
        let m = matrices(bones, &rot);
        if m[end].transform_point3(tail).distance(target) < 1e-5 {
            break;
        }
        for &j in &joints {
            let m = matrices(bones, &rot);
            let tip = m[end].transform_point3(tail);
            let pivot = m[j].transform_point3(DVec3::from(bones[j].head));
            let (a, b) = (tip - pivot, target - pivot);
            if a.length() < 1e-9 || b.length() < 1e-9 {
                continue;
            }
            let mut turn = DQuat::from_rotation_arc(a.normalize(), b.normalize());
            let angle = turn.angle_between(DQuat::IDENTITY);
            if angle > REACH_STEP {
                turn = DQuat::IDENTITY.slerp(turn, REACH_STEP / angle);
            }
            // The bone's frame in object space is its parents' turn times its own.
            let parent =
                parent_of(j).map_or(DQuat::IDENTITY, |k| DQuat::from_mat4(&m[k]).normalize());
            let [x, y, z] = rot[j];
            let own = DQuat::from_euler(EulerRot::XYZ, x, y, z);
            let next = (parent.inverse() * turn * parent * own).normalize();
            let (x, y, z) = next.to_euler(EulerRot::XYZ);
            rot[j] = [x, y, z];
        }
    }
    rot
}

/// Where the tip of bone `k` is with the bones at `rotations` (object space).
pub fn tip(bones: &[Bone], rotations: &[[f64; 3]], k: usize) -> DVec3 {
    matrices(bones, rotations)[k].transform_point3(DVec3::from(bones[k].tail))
}

/// Whether any bone is turned (otherwise skinning changes nothing).
pub fn posed(rotations: &[[f64; 3]]) -> bool {
    rotations.iter().flatten().any(|v| v.abs() > 1e-12)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::engine::{CommandBatch, Editor};
    use serde_json::{Value, json};

    fn apply(ed: &mut Editor, commands: Value) -> Result<(), EngineError> {
        let batch: CommandBatch = serde_json::from_value(json!({"commands":commands})).unwrap();
        ed.apply(&batch).map(|_| ())
    }

    fn target_editor() -> Editor {
        let mut ed = Editor::new();
        apply(&mut ed, json!([
            {"op":"add","name":"Arm","primitive":{"kind":"cylinder","height":2,"rings":8},"translation":[0,1,0]},
            {"op":"rig","id":"Arm","chain":4,"axis":"y"},
            {"op":"add","name":"Target","primitive":{"kind":"sphere"},"translation":[0.8,1.3,0.2]},
            {"op":"track_target","id":"Arm","bone":"Bone 4","target":"Target"}
        ])).unwrap();
        ed
    }

    fn tip_world(scene: &Scene) -> DVec3 {
        let o = &scene.objects[0];
        o.transform
            .matrix()
            .transform_point3(tip(&o.bones, &rotations(o, None), 3))
    }

    #[test]
    fn target_follows_world_origin_through_transforms_and_undo() {
        let mut ed = target_editor();
        assert!(tip_world(ed.scene()).distance(DVec3::new(0.8, 1.3, 0.2)) < 1e-3);
        let before = ed.scene().clone();
        apply(
            &mut ed,
            json!([
                {"op":"transform","id":"Arm","rotation":[0.2,0.4,0.1],"scale":[1.2,0.9,1.1]},
                {"op":"transform","id":"Target","translation":[-0.6,1.4,0.2]}
            ]),
        )
        .unwrap();
        assert!(tip_world(ed.scene()).distance(DVec3::new(-0.6, 1.4, 0.2)) < 1e-3);
        ed.undo().unwrap();
        assert_eq!(ed.scene().objects, before.objects);
        assert_eq!(ed.scene().ik_targets, before.ik_targets);
        ed.redo().unwrap();
        assert!(tip_world(ed.scene()).distance(DVec3::new(-0.6, 1.4, 0.2)) < 1e-3);
    }

    #[test]
    fn keyed_targets_override_bone_keys_without_editing_or_double_sampling() {
        let mut ed = target_editor();
        apply(&mut ed, json!([
            {"op":"set_keyframe","id":"Target","property":"translation","frame":1,"value":[0.8,1.3,0.2],"interpolation":"linear"},
            {"op":"set_keyframe","id":"Target","property":"translation","frame":3,"value":[-0.8,1.3,0.2]},
            {"op":"set_keyframe","id":"Arm","property":"bone","bone":"Bone 4","frame":1,"value":[0,0,0.5]},
            {"op":"set_keyframe","id":"Arm","property":"translation","frame":1,"value":[0,1,0],"interpolation":"linear"},
            {"op":"set_keyframe","id":"Arm","property":"translation","frame":3,"value":[0.2,1,0]}
        ])).unwrap();
        let rest = ed.scene().clone();
        let first = ed.scene_at(1.5).unwrap();
        for f in [3., 1., 1.5, 2.5] {
            let scene = ed.scene_at(f).unwrap();
            let target = DVec3::from(scene.objects[1].transform.translation);
            assert!(tip_world(&scene).distance(target) < 1e-3, "frame {f}");
            assert!(scene.objects.iter().all(|o| o.tracks.is_empty()));
        }
        assert_eq!(ed.scene_at(1.5).unwrap(), first);
        assert_eq!(*ed.scene(), rest);
        let o = &first.objects[0];
        let mesh = ed.posed(o, Some(1.5));
        assert_eq!(*mesh, skin(ed.evaluated(o), &o.bones, &rotations(o, None)));
        let context = crate::engine::context_at_checked(&ed, Some(1.5)).unwrap();
        assert_eq!(
            context["objects"][0]["pose"]["bones"],
            serde_json::to_value(&o.bones).unwrap()
        );
        let bounds = crate::engine::world_bounds(&o.transform, &mesh).unwrap();
        assert_eq!(
            context["objects"][0]["bounds"],
            serde_json::to_value(bounds).unwrap()
        );
    }

    #[test]
    fn invalid_targets_and_rig_changes_are_atomic_and_deletion_cleans_up() {
        let mut ed = target_editor();
        let rest = ed.scene().clone();
        for commands in [
            json!([{"op":"track_target","id":"Arm","bone":"Bone 4","target":"Arm"}]),
            json!([{"op":"track_target","id":"Arm","bone":"missing","target":"Target"}]),
            json!([{"op":"track_target","id":"Arm","bone":"Bone 4","target":"missing"}]),
            json!([{"op":"track_target","id":"Arm","bone":"Bone 4","target":"Target","chain":0}]),
            json!([{"op":"track_target","id":"Arm","bone":"Bone 4","target":"Target","chain":65}]),
            json!([{"op":"rig","id":"Arm","bones":[]}]),
        ] {
            assert!(apply(&mut ed, commands).is_err());
            assert_eq!(*ed.scene(), rest);
        }
        let mut invalid = rest.clone();
        invalid.ik_targets.push(invalid.ik_targets[0].clone());
        assert!(ed.load(invalid).is_err());
        assert_eq!(*ed.scene(), rest);
        apply(&mut ed, json!([{"op":"delete","id":"Target"}])).unwrap();
        assert!(ed.scene().ik_targets.is_empty());
        ed.undo().unwrap();
        assert_eq!(ed.scene().ik_targets, rest.ik_targets);
        apply(
            &mut ed,
            json!([{"op":"clear_target","id":"Arm"},{"op":"rig","id":"Arm","bones":[]}]),
        )
        .unwrap();
        assert!(ed.scene().objects[0].bones.is_empty());
    }

    #[test]
    fn target_relations_survive_proposal_reapplication_and_history_revision() {
        let mut ed = target_editor();
        apply(&mut ed, json!([{"op":"clear_target","id":"Arm"}])).unwrap();
        let rest = ed.scene().clone();
        let req=serde_json::from_value(json!({"title":"Follow target","commands":[{"op":"track_target","id":"Arm","bone":"Bone 4","target":"Target"}]})).unwrap();
        let id = ed.propose(&req).unwrap()[0];
        assert_eq!(*ed.scene(), rest);
        let preview = ed.preview(id).unwrap();
        assert_eq!(preview.scene().ik_targets.len(), 1);
        assert!(
            crate::proposal::diff(ed.scene(), preview.scene())
                .scene
                .contains(&"ik_targets")
        );
        apply(
            &mut ed,
            json!([{"op":"transform","id":"Target","translation":[-0.5,1.4,0.2]}]),
        )
        .unwrap();
        let preview = ed.preview(id).unwrap();
        assert!(tip_world(preview.scene()).distance(DVec3::new(-0.5, 1.4, 0.2)) < 1e-3);
        ed.accept(id).unwrap();
        assert_eq!(ed.scene().ik_targets.len(), 1);
        let commands = serde_json::from_value(
            json!([{ "op":"transform","id":"Target","translation":[0.5,1.4,0.2]}]),
        )
        .unwrap();
        // Step 3 moved the target; the later binding must replay against its new origin.
        let preview = ed.revise_preview(3, commands).unwrap();
        assert!(tip_world(preview.scene()).distance(DVec3::new(0.5, 1.4, 0.2)) < 1e-3);
        ed.undo().unwrap();
        assert!(ed.scene().ik_targets.is_empty());
    }

    #[test]
    fn unreachable_target_is_finite_and_unbinding_keeps_the_pose() {
        let mut ed = target_editor();
        apply(
            &mut ed,
            json!([{"op":"transform","id":"Target","translation":[100,20,3]}]),
        )
        .unwrap();
        assert!(
            ed.scene().objects[0]
                .bones
                .iter()
                .flat_map(|b| b.rotation)
                .all(f64::is_finite)
        );
        let bones = ed.scene().objects[0].bones.clone();
        apply(&mut ed,json!([{"op":"clear_target","id":"Arm"},{"op":"transform","id":"Target","translation":[-100,0,0]}])).unwrap();
        assert_eq!(ed.scene().objects[0].bones, bones);
        let mut reloaded = Editor::new();
        reloaded.load(ed.scene().clone()).unwrap();
        assert_eq!(reloaded.scene().objects, ed.scene().objects);
    }

    /// A tall box of rings along Y (0..2), so it can bend.
    fn column() -> Mesh {
        let mut vertices = Vec::new();
        let mut faces = Vec::new();
        let rings = 9;
        for r in 0..rings {
            let y = 2.0 * r as f64 / (rings - 1) as f64;
            for [x, z] in [[-0.1, -0.1], [0.1, -0.1], [0.1, 0.1], [-0.1, 0.1]] {
                vertices.push([x, y, z]);
            }
        }
        for r in 0..rings - 1 {
            for k in 0..4u32 {
                let (a, b) = (r * 4 + k, r * 4 + (k + 1) % 4);
                faces.push(vec![a, b, b + 4, a + 4]);
            }
        }
        Mesh {
            vertices,
            faces,
            uvs: Vec::new(),
            seams: Vec::new(),
        }
    }

    #[test]
    fn a_chain_runs_through_the_mesh_and_validates() {
        let bones = chain(&column(), 4, None).unwrap();
        assert_eq!(bones.len(), 4);
        assert_eq!(bones[0].head, [0.0, 0.0, 0.0]);
        assert_eq!(bones[3].tail, [0.0, 2.0, 0.0]);
        assert_eq!(bones[2].parent.as_deref(), Some("Bone 2"));
        validate(&bones).unwrap();
        let mut bad = bones.clone();
        bad[1].parent = Some("Bone 4".into());
        assert!(validate(&bad).is_err(), "parents come first");
        let mut bad = bones.clone();
        bad[2].name = "Bone 1".into();
        assert!(validate(&bad).is_err(), "names are unique");
        assert!(chain(&column(), 0, None).is_err());
    }

    #[test]
    fn weights_follow_the_nearest_bones_and_sum_to_one() {
        let mesh = column();
        let bones = chain(&mesh, 2, None).unwrap();
        let w = weights(&mesh, &bones);
        for influences in &w {
            let sum: f32 = influences.iter().map(|i| i.1).sum();
            assert!((sum - 1.0).abs() < 1e-5);
        }
        // The bottom ring follows the first bone, the top the second, and
        // the joint is shared evenly.
        assert!(w[0][0] == (0, 1.0) || w[0].iter().find(|i| i.0 == 0).unwrap().1 > 0.95);
        let top = w[mesh.vertices.len() - 1]
            .iter()
            .find(|i| i.0 == 1)
            .unwrap()
            .1;
        assert!(top > 0.95, "{top}");
        let joint = &w[4 * 4]; // ring 4 at y = 1
        let share = |b: u16| joint.iter().find(|i| i.0 == b).map_or(0.0, |i| i.1);
        assert!((share(0) - 0.5).abs() < 0.01 && (share(1) - 0.5).abs() < 0.01);
    }

    #[test]
    fn posing_bends_children_with_their_parents() {
        let mesh = column();
        let bones = chain(&mesh, 2, None).unwrap();
        // Turning the first bone 90 degrees about Z swings the whole column
        // over; the top (on the child bone) lands near (-2, 0).
        let bent = skin(
            &mesh,
            &bones,
            &[[0.0, 0.0, std::f64::consts::FRAC_PI_2], [0.0; 3]],
        );
        let tip = bent.vertices[bent.vertices.len() - 2]; // (0.1, 2, 0.1) at rest
        assert!(
            (tip[0] + 2.0).abs() < 1e-6 && (tip[1] - 0.1).abs() < 1e-6,
            "{tip:?}"
        );
        // Turning only the second bone leaves the bottom half alone.
        let bent = skin(&mesh, &bones, &[[0.0; 3], [0.0, 0.0, 1.0]]);
        assert_eq!(bent.vertices[0], mesh.vertices[0]);
        assert_ne!(
            bent.vertices[mesh.vertices.len() - 1],
            mesh.vertices[mesh.vertices.len() - 1]
        );
        assert!(!posed(&[[0.0; 3]]) && posed(&[[0.0, 0.1, 0.0]]));
    }

    #[test]
    fn reaching_bends_the_chain_until_the_tip_arrives() {
        let bones = chain(&column(), 4, None).unwrap();
        let rest = vec![[0.0; 3]; 4];
        // A point the 2-unit chain can reach, off to the side and lower.
        let target = DVec3::new(0.9, 1.2, 0.4);
        let solved = reach(&bones, &rest, 3, 4, target);
        assert!(tip(&bones, &solved, 3).distance(target) < 1e-3);
        // The base of the first bone does not move; a short chain only
        // turns the bones it is given.
        let short = reach(&bones, &rest, 3, 2, DVec3::new(0.3, 1.8, 0.0));
        assert_eq!(&short[..2], &rest[..2]);
        assert_ne!(short[2], rest[2]);
        // Out of reach: the chain points straight at the target.
        let far = DVec3::new(10.0, 0.0, 0.0);
        let stretched = reach(&bones, &rest, 3, 4, far);
        let t = tip(&bones, &stretched, 3);
        assert!(
            (t - DVec3::ZERO).normalize().dot(far.normalize()) > 0.999,
            "{t}"
        );
    }
}
