//! Constraints: relations that stay true through every later edit, so the
//! scene keeps its intent instead of just its coordinates. An object (or a
//! group, such as a built table) can be kept **on** another, keeping its
//! place on it; kept the **mirror** image of another across a plane; or
//! kept **matching** another's material; or kept a fixed **distance** from
//! another object's or group's evaluated world-bound centre; or keep centres
//! **aligned** along selected world axes while leaving the other axes free.
//!
//! Constraints are solved at the end of every command batch. Mirrors and
//! matches work both ways: whichever side the batch changed leads, so moving
//! either speaker of a mirrored pair moves the other. An object kept on a
//! support follows the support when it moves and keeps its new spot when
//! it is moved itself. Because solving is part of applying a batch, the
//! history replays and proposals preview with the same results.

use std::collections::HashMap;

use glam::{DQuat, DVec3, EulerRot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::assembly;
use crate::engine::{EngineError, Material, ObjRef, Scene, Transform};
use crate::inspect;

fn err<T>(message: impl Into<String>) -> Result<T, EngineError> {
    Err(EngineError::new(message))
}

/// What a constraint is about: one object (by id) or every part of a group.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Who {
    Id(u64),
    Group(String),
}

/// The plane a mirror reflects across: `x` = 0 (left and right) or `z` = 0
/// (front and back).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Plane {
    #[default]
    X,
    Z,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    On,
    Mirrors,
    Matches,
    Distance,
    Align,
}

/// World axes whose evaluated bound centres must coincide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Axes {
    X,
    Y,
    Z,
    Xy,
    Xz,
    Yz,
    Xyz,
}

impl Axes {
    fn mask(self) -> DVec3 {
        match self {
            Self::X => DVec3::X,
            Self::Y => DVec3::Y,
            Self::Z => DVec3::Z,
            Self::Xy => DVec3::new(1.0, 1.0, 0.0),
            Self::Xz => DVec3::new(1.0, 0.0, 1.0),
            Self::Yz => DVec3::new(0.0, 1.0, 1.0),
            Self::Xyz => DVec3::ONE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Rule {
    /// Rest on top of `support`, `offset` [x, z] metres from its middle.
    On { support: Who, offset: [f64; 2] },
    /// Be the mirror image of `of` across `axis` = 0.
    Mirrors { of: Who, axis: Plane },
    /// Share `of`'s material.
    Matches { of: Who },
    /// Keep evaluated world-bound centres this many metres apart.
    Distance { of: Who, distance: f64 },
    /// Keep evaluated centres equal on these axes; other axes stay free.
    Align { of: Who, axes: Axes },
}

impl Rule {
    pub fn kind(&self) -> Kind {
        match self {
            Rule::On { .. } => Kind::On,
            Rule::Mirrors { .. } => Kind::Mirrors,
            Rule::Matches { .. } => Kind::Matches,
            Rule::Distance { .. } => Kind::Distance,
            Rule::Align { .. } => Kind::Align,
        }
    }

    fn other(&self) -> &Who {
        match self {
            Rule::On { support, .. } => support,
            Rule::Mirrors { of, .. }
            | Rule::Matches { of }
            | Rule::Distance { of, .. }
            | Rule::Align { of, .. } => of,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Constraint {
    pub id: u64,
    pub subject: Who,
    #[serde(flatten)]
    pub rule: Rule,
}

/// The `Who` an object reference names: an object by name or id, else a
/// group by name.
pub fn who(scene: &Scene, r: &ObjRef) -> Result<Who, EngineError> {
    match r {
        ObjRef::Id(id) if scene.objects.iter().any(|o| o.id == *id) => Ok(Who::Id(*id)),
        ObjRef::Id(id) => err(format!("no object with id {id}")),
        ObjRef::Name(name) => {
            if let Some(o) = scene.objects.iter().find(|o| &o.name == name) {
                Ok(Who::Id(o.id))
            } else if scene
                .objects
                .iter()
                .any(|o| o.group.as_deref() == Some(name))
            {
                Ok(Who::Group(name.clone()))
            } else {
                err(format!("no object or group named {name:?}"))
            }
        }
    }
}

/// Indices of the objects `who` covers (empty when they are gone).
pub fn members(scene: &Scene, who: &Who) -> Vec<usize> {
    scene
        .objects
        .iter()
        .enumerate()
        .filter(|(_, o)| match who {
            Who::Id(id) => o.id == *id,
            Who::Group(g) => o.group.as_deref() == Some(g.as_str()),
        })
        .map(|(i, _)| i)
        .collect()
}

fn ids(scene: &Scene, idx: &[usize]) -> Vec<u64> {
    idx.iter().map(|&i| scene.objects[i].id).collect()
}

fn overlaps(scene: &Scene, a: &Who, b: &Who) -> bool {
    let b = members(scene, b);
    members(scene, a).iter().any(|i| b.contains(i))
}

/// Whether every object a constraint names still exists.
pub fn alive(scene: &Scene, c: &Constraint) -> bool {
    !members(scene, &c.subject).is_empty() && !members(scene, c.rule.other()).is_empty()
}

fn centre(scene: &Scene, idx: &[usize]) -> Result<DVec3, EngineError> {
    let solids = inspect::scene_solids(scene)?;
    let (lo, hi) = inspect::bounds(&solids, &ids(scene, idx))
        .ok_or_else(|| EngineError::new("nothing with faces to constrain"))?;
    Ok((lo + hi) * 0.5)
}

/// Add (or replace) a constraint of `subject`; it holds from now on.
pub fn add(scene: &mut Scene, subject: &ObjRef, rule: Request) -> Result<(), EngineError> {
    let subject = who(scene, subject)?;
    let rule = match rule {
        Request::On(r) => {
            let support = who(scene, &r)?;
            if overlaps(scene, &subject, &support) {
                return err("an object cannot rest on itself");
            }
            // Supports may rest on others, but not in a loop.
            let mut at = support.clone();
            for _ in 0..64 {
                let next = scene.constraints.iter().find_map(|c| match &c.rule {
                    Rule::On { support, .. } if c.subject == at => Some(support.clone()),
                    _ => None,
                });
                match next {
                    Some(n) if n == subject || overlaps(scene, &n, &subject) => {
                        return err("that would make objects rest on each other in a loop");
                    }
                    Some(n) => at = n,
                    None => break,
                }
            }
            let (s, p) = (members(scene, &subject), members(scene, &support));
            let d = centre(scene, &s)? - centre(scene, &p)?;
            Rule::On {
                support,
                offset: [d.x, d.z],
            }
        }
        Request::Mirrors(r, axis) => {
            let of = who(scene, &r)?;
            if overlaps(scene, &subject, &of) {
                return err("an object cannot mirror itself");
            }
            if members(scene, &subject).len() != members(scene, &of).len() {
                return err("mirrored groups need the same number of parts");
            }
            Rule::Mirrors { of, axis }
        }
        Request::Distance(r, distance) => {
            if !distance.is_finite() || distance < 0.0 {
                return err("distance must be finite and non-negative");
            }
            let of = who(scene, &r)?;
            if overlaps(scene, &subject, &of) {
                return err("a distance constraint needs disjoint objects or groups");
            }
            Rule::Distance { of, distance }
        }
        Request::Align(r, axes) => {
            let of = who(scene, &r)?;
            if overlaps(scene, &subject, &of) {
                return err("an alignment constraint needs disjoint objects or groups");
            }
            Rule::Align { of, axes }
        }
        Request::Matches(r) => {
            let of = who(scene, &r)?;
            if overlaps(scene, &subject, &of) {
                return err("an object cannot match itself");
            }
            Rule::Matches { of }
        }
    };
    let kind = rule.kind();
    // Symmetric rules bind a pair once, whichever way round.
    scene.constraints.retain(|c| {
        !(c.subject == subject && c.rule.kind() == kind)
            && !(kind != Kind::On
                && c.rule.kind() == kind
                && &c.subject == rule.other()
                && c.rule.other() == &subject)
    });
    let id = scene.constraints.iter().map(|c| c.id).max().unwrap_or(0) + 1;
    let c = Constraint { id, subject, rule };
    apply(scene, &c, Lead::Other)?;
    scene.constraints.push(c);
    Ok(())
}

/// What a `constrain` command asks for.
pub enum Request {
    On(ObjRef),
    Mirrors(ObjRef, Plane),
    Matches(ObjRef),
    Distance(ObjRef, f64),
    Align(ObjRef, Axes),
}

/// Drop `subject`'s constraints (of one kind, or all), and those of others
/// that bind it with a symmetric mirror, match, distance or alignment rule.
pub fn remove(scene: &mut Scene, subject: &ObjRef, kind: Option<Kind>) -> Result<(), EngineError> {
    let subject = who(scene, subject)?;
    let before = scene.constraints.len();
    scene.constraints.retain(|c| {
        let mine =
            c.subject == subject || (c.rule.kind() != Kind::On && c.rule.other() == &subject);
        !(mine && kind.is_none_or(|k| k == c.rule.kind()))
    });
    if scene.constraints.len() == before {
        return err("nothing to unconstrain");
    }
    Ok(())
}

/// What each constrained object looked like before a batch, to tell which
/// side of a constraint the batch changed.
#[derive(Default)]
pub struct Snapshot {
    looks: HashMap<u64, (Transform, Material, usize)>,
    constraints: Vec<u64>,
}

pub fn snapshot(scene: &Scene) -> Snapshot {
    if scene.constraints.is_empty() && scene.arrangements.is_empty() {
        return Snapshot::default();
    }
    Snapshot {
        looks: scene
            .objects
            .iter()
            .map(|o| {
                (
                    o.id,
                    (
                        o.transform.clone(),
                        o.material.clone(),
                        o.mesh.vertices.len(),
                    ),
                )
            })
            .collect(),
        constraints: scene.constraints.iter().map(|c| c.id).collect(),
    }
}

fn changed(before: &Snapshot, o: &crate::engine::Object) -> bool {
    before.looks.get(&o.id).is_none_or(|(t, m, n)| {
        *t != o.transform || *m != o.material || *n != o.mesh.vertices.len()
    })
}

/// Which side leads when a constraint is applied.
#[derive(Clone, Copy, PartialEq)]
enum Lead {
    /// The other object (the support, or the original being mirrored).
    Other,
    /// The constrained object itself (it was edited).
    Subject,
}

/// Make every constraint true again after a batch.
pub fn solve(scene: &mut Scene, before: &Snapshot) -> Result<(), EngineError> {
    if scene.constraints.is_empty() && scene.arrangements.is_empty() {
        return Ok(());
    }
    // Objects the batch changed; solving marks what it moves, so changes
    // spread along chains (a cup on a tray on a table, cups matching cups).
    let mut dirty: std::collections::HashSet<u64> = scene
        .objects
        .iter()
        .filter(|o| changed(before, o))
        .map(|o| o.id)
        .collect();
    let directly_changed = dirty.clone();
    let touched = |scene: &Scene, dirty: &std::collections::HashSet<u64>, who: &Who| {
        members(scene, who)
            .iter()
            .any(|&i| dirty.contains(&scene.objects[i].id))
    };
    let mut constraints = std::mem::take(&mut scene.constraints);
    let result = (|| {
        // An object kept on a support that the batch moved by hand keeps
        // its new spot on it.
        for c in constraints.iter_mut() {
            let fresh = !before.constraints.contains(&c.id);
            if let Rule::On { support, offset } = &mut c.rule
                && !fresh
                && touched(scene, &dirty, &c.subject)
            {
                let d = centre(scene, &members(scene, &c.subject))?
                    - centre(scene, &members(scene, support))?;
                *offset = [d.x, d.z];
            }
        }
        let spatial = |c: &Constraint| matches!(c.rule, Rule::Distance { .. } | Rule::Align { .. });
        let has_spatial = constraints.iter().any(spatial) || !scene.arrangements.is_empty();
        for pass in 0..if has_spatial { 32 } else { 3 } {
            for c in &constraints {
                let fresh = !before.constraints.contains(&c.id);
                let subject = touched(scene, &dirty, &c.subject);
                let other = touched(scene, &dirty, c.rule.other());
                let direct_subject = touched(scene, &directly_changed, &c.subject);
                let direct_other = touched(scene, &directly_changed, c.rule.other());
                let (subject, other) = if spatial(c) && (direct_subject || direct_other) {
                    (direct_subject, direct_other)
                } else {
                    (subject, other)
                };
                // Propagated spatial edits still need solving, even when
                // neither endpoint was directly edited by the command batch.
                let lead = match (fresh, subject, other) {
                    (false, false, false) if !spatial(c) => continue,
                    (false, false, false) => Lead::Other,
                    (false, true, false) => Lead::Subject,
                    _ => Lead::Other,
                };
                apply(scene, c, lead)?;
                // What was written to is changed now too.
                let written = match (&c.rule, lead) {
                    (Rule::On { .. }, _) | (_, Lead::Other) => &c.subject,
                    (_, Lead::Subject) => c.rule.other(),
                };
                for i in members(scene, written) {
                    dirty.insert(scene.objects[i].id);
                }
            }
            for arrangement in scene.arrangements.clone() {
                crate::layout::apply(scene, &arrangement)?;
                for item in &arrangement.items {
                    for i in members(scene, item) {
                        dirty.insert(scene.objects[i].id);
                    }
                }
            }
            if has_spatial && pass >= 2 && violations(scene, &constraints).is_empty() {
                break;
            }
        }
        if has_spatial && let Some(issue) = violations(scene, &constraints).first() {
            return err(issue["message"]
                .as_str()
                .unwrap_or("constraint cannot be satisfied"));
        }
        Ok(())
    })();
    scene.constraints = constraints;
    result
}

fn apply(scene: &mut Scene, c: &Constraint, lead: Lead) -> Result<(), EngineError> {
    let subject = members(scene, &c.subject);
    match &c.rule {
        Rule::On { support, offset } => {
            let support = members(scene, support);
            let target = centre(scene, &support)? + DVec3::new(offset[0], 0.0, offset[1]);
            let now = centre(scene, &subject)?;
            assembly::translate(
                scene,
                &subject,
                DVec3::new(target.x - now.x, 0.0, target.z - now.z),
            );
            // Rest on the support itself, not on whatever else sits on it
            // (things kept on one table must not climb onto each other).
            let (mine, base) = (ids(scene, &subject), ids(scene, &support));
            let solids = inspect::scene_solids(scene)?;
            let none = || EngineError::new("nothing with faces to constrain");
            let top = inspect::bounds(&solids, &base).ok_or_else(none)?.1.y;
            let low = inspect::bounds(&solids, &mine).ok_or_else(none)?.0.y;
            assembly::translate(scene, &subject, DVec3::Y * (top + 0.5 - low));
            let solids = inspect::scene_solids(scene)?;
            let dy = inspect::settle_onto(&solids, &mine, &base)?;
            assembly::translate(scene, &subject, DVec3::Y * dy);
            Ok(())
        }
        Rule::Mirrors { of, axis } => {
            let of = members(scene, of);
            let (from, to) = match lead {
                Lead::Other => (&of, &subject),
                Lead::Subject => (&subject, &of),
            };
            for (&a, &b) in from.iter().zip(to.iter()) {
                scene.objects[b].transform = mirror(&scene.objects[a].transform, *axis);
            }
            Ok(())
        }
        Rule::Distance { of, distance } => {
            let of = members(scene, of);
            let (fixed, moving) = match lead {
                Lead::Other => (&of, &subject),
                Lead::Subject => (&subject, &of),
            };
            let anchor = centre(scene, fixed)?;
            let current = centre(scene, moving)?;
            let direction = (current - anchor).try_normalize().unwrap_or(DVec3::X);
            let delta = anchor + direction * *distance - current;
            if !delta.is_finite() {
                return err("distance constraint exceeds finite scene coordinates");
            }
            assembly::translate(scene, moving, delta);
            Ok(())
        }
        Rule::Align { of, axes } => {
            let of = members(scene, of);
            let (fixed, moving) = match lead {
                Lead::Other => (&of, &subject),
                Lead::Subject => (&subject, &of),
            };
            let delta = (centre(scene, fixed)? - centre(scene, moving)?) * axes.mask();
            if !delta.is_finite() {
                return err("alignment constraint exceeds finite scene coordinates");
            }
            assembly::translate(scene, moving, delta);
            Ok(())
        }
        Rule::Matches { of } => {
            let of = members(scene, of);
            let (from, to) = match lead {
                Lead::Other => (&of, &subject),
                Lead::Subject => (&subject, &of),
            };
            for (k, &b) in to.iter().enumerate() {
                let a = from[k.min(from.len() - 1)];
                scene.objects[b].material = scene.objects[a].material.clone();
            }
            Ok(())
        }
    }
}

/// `t` reflected across the plane `axis` = 0.
pub fn mirror(t: &Transform, axis: Plane) -> Transform {
    let [x, y, z] = t.translation;
    let [rx, ry, rz] = t.rotation;
    let q = DQuat::from_euler(EulerRot::XYZ, rx, ry, rz);
    // Reflecting a rotation R by M gives M R M; as a quaternion that flips
    // the two components in the mirror plane.
    let (translation, q) = match axis {
        Plane::X => ([-x, y, z], DQuat::from_xyzw(q.x, -q.y, -q.z, q.w)),
        Plane::Z => ([x, y, -z], DQuat::from_xyzw(-q.x, -q.y, q.z, q.w)),
    };
    let (ex, ey, ez) = q.to_euler(EulerRot::XYZ);
    Transform {
        translation,
        rotation: [ex, ey, ez],
        scale: t.scale,
    }
}

/// Check a loaded scene's constraints.
pub fn validate(scene: &Scene) -> Result<(), EngineError> {
    for c in &scene.constraints {
        if !alive(scene, c) {
            return err(format!(
                "constraint {} names objects that do not exist",
                c.id
            ));
        }
        if let Rule::Distance { of, distance } = &c.rule
            && (!distance.is_finite() || *distance < 0.0 || overlaps(scene, &c.subject, of))
        {
            return err(format!(
                "constraint {} has an invalid distance or overlapping endpoints",
                c.id
            ));
        }
        if let Rule::On { offset, .. } = &c.rule
            && !offset.iter().all(|v| v.is_finite())
        {
            return err(format!("constraint {} has a bad offset", c.id));
        }
        if let Rule::Align { of, .. } = &c.rule
            && overlaps(scene, &c.subject, of)
        {
            return err(format!(
                "constraint {} has overlapping alignment endpoints",
                c.id
            ));
        }
    }
    Ok(())
}

/// Distance residuals for inspection, including imported unsatisfied scenes.
/// Other rule kinds retain their existing diagnostics.
pub fn distance_violations(scene: &Scene, constraints: &[Constraint]) -> Vec<serde_json::Value> {
    constraints.iter().filter_map(|c| {
        let Rule::Distance { of, distance } = &c.rule else { return None };
        let measured = centre(scene, &members(scene, &c.subject))
            .and_then(|a| centre(scene, &members(scene, of)).map(|b| (a - b).length()));
        match measured {
            Ok(actual) if actual.is_finite() && (actual - distance).abs() <= 1e-6 * distance.max(1.0) => None,
            Ok(actual) => Some(serde_json::json!({"kind":"constraint_violation", "constraint":c.id,
                "expected":distance, "actual":actual,
                "message":format!("distance constraint {} cannot be satisfied: expected {} m, measured {} m", c.id, distance, actual)})),
            Err(e) => Some(serde_json::json!({"kind":"constraint_violation", "constraint":c.id,
                "message":format!("distance constraint {} cannot be measured: {}", c.id, e)})),
        }
    }).collect()
}

/// Inspect all rules. Spatial batches must also preserve existing rules:
/// satisfying distance or alignment alone can break an earlier on/mirror rule.
pub fn violations(scene: &Scene, constraints: &[Constraint]) -> Vec<serde_json::Value> {
    let mut issues = distance_violations(scene, constraints);
    for c in constraints
        .iter()
        .filter(|c| c.rule.kind() != Kind::Distance)
    {
        let mut expected = scene.clone();
        let error = apply(&mut expected, c, Lead::Other).err();
        let differs = members(scene, &c.subject).iter().any(|&i| {
            let a = &scene.objects[i];
            let b = &expected.objects[i];
            let [ax, ay, az] = a.transform.rotation;
            let [bx, by, bz] = b.transform.rotation;
            let qa = DQuat::from_euler(EulerRot::XYZ, ax, ay, az);
            let qb = DQuat::from_euler(EulerRot::XYZ, bx, by, bz);
            a.material != b.material
                || !qa.is_finite()
                || !qb.is_finite()
                || !(qa.abs_diff_eq(qb, 1e-6) || qa.abs_diff_eq(-qb, 1e-6))
                || a.transform
                    .translation
                    .iter()
                    .chain(&a.transform.scale)
                    .zip(b.transform.translation.iter().chain(&b.transform.scale))
                    .any(|(x, y)| {
                        !x.is_finite()
                            || !y.is_finite()
                            || (x - y).abs() > 1e-6 * x.abs().max(y.abs()).max(1.0)
                    })
        });
        if error.is_some() || differs {
            issues.push(serde_json::json!({"kind":"constraint_violation", "constraint":c.id,
                "rule":c.rule.kind(), "message":format!("{:?} constraint {} cannot be satisfied{}", c.rule.kind(), c.id,
                    error.map(|e| format!(": {e}")).unwrap_or_default())}));
        }
    }
    issues.extend(crate::layout::violations(scene));
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CommandBatch, Editor};
    use serde_json::json;

    fn apply(ed: &mut Editor, commands: serde_json::Value) -> Result<(), EngineError> {
        let batch: CommandBatch = serde_json::from_value(json!({ "commands": commands })).unwrap();
        ed.apply(&batch).map(|_| ())
    }

    fn at(ed: &Editor, name: &str) -> [f64; 3] {
        ed.scene()
            .objects
            .iter()
            .find(|o| o.name == name)
            .unwrap()
            .transform
            .translation
    }

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6)
    }

    fn bottom(ed: &Editor, name: &str) -> f64 {
        let solids = inspect::scene_solids(ed.scene()).unwrap();
        let id = ed
            .scene()
            .objects
            .iter()
            .find(|o| o.name == name)
            .unwrap()
            .id;
        inspect::bounds(&solids, &[id]).unwrap().0.y
    }

    fn table_top(ed: &Editor) -> f64 {
        let solids = inspect::scene_solids(ed.scene()).unwrap();
        let ids: Vec<u64> = ed
            .scene()
            .objects
            .iter()
            .filter(|o| o.group.as_deref() == Some("Table"))
            .map(|o| o.id)
            .collect();
        inspect::bounds(&solids, &ids).unwrap().1.y
    }

    fn pair() -> Editor {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([
                {"op":"add","name":"A","primitive":{"kind":"cube"}},
                {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[3,0,0]},
                {"op":"constrain","id":"B","distance":2,"from":"A"}
            ]),
        )
        .unwrap();
        ed
    }

    #[test]
    fn alignment_selects_world_axes_and_either_endpoint_leads() {
        for (axes, mask) in [
            ("x", [true, false, false]),
            ("y", [false, true, false]),
            ("z", [false, false, true]),
            ("xy", [true, true, false]),
            ("xz", [true, false, true]),
            ("yz", [false, true, true]),
            ("xyz", [true, true, true]),
        ] {
            let mut ed = Editor::new();
            apply(
                &mut ed,
                json!([
                    {"op":"add","name":"A","primitive":{"kind":"cube"},"translation":[1,2,3]},
                    {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[4,5,6]},
                    {"op":"constrain","id":"B","align":axes,"from":"A"}
                ]),
            )
            .unwrap();
            let aligned = std::array::from_fn(|i| {
                if mask[i] {
                    [1., 2., 3.][i]
                } else {
                    [4., 5., 6.][i]
                }
            });
            assert!(close(at(&ed, "B"), aligned), "{axes}");
            apply(
                &mut ed,
                json!([{"op":"transform","id":"A","translation":[7,8,9]}]),
            )
            .unwrap();
            let moved = std::array::from_fn(|i| if mask[i] { [7., 8., 9.][i] } else { aligned[i] });
            assert!(close(at(&ed, "B"), moved));
            apply(
                &mut ed,
                json!([{"op":"transform","id":"B","translation":[10,11,12]}]),
            )
            .unwrap();
            let followed = std::array::from_fn(|i| {
                if mask[i] {
                    [10., 11., 12.][i]
                } else {
                    [7., 8., 9.][i]
                }
            });
            assert!(close(at(&ed, "A"), followed));
            ed.undo().unwrap();
            assert!(close(at(&ed, "A"), [7., 8., 9.]));
            assert!(close(at(&ed, "B"), moved));
            assert!(violations(ed.scene(), &ed.scene().constraints).is_empty());
        }
    }

    #[test]
    fn alignment_chains_propagate_and_both_edits_are_reference_led() {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([
                {"op":"add","name":"A","primitive":{"kind":"cube"}},
                {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[2,1,0]},
                {"op":"add","name":"C","primitive":{"kind":"cube"},"translation":[4,2,0]},
                {"op":"constrain","id":"B","align":"y","from":"A"},
                {"op":"constrain","id":"C","align":"y","from":"B"}
            ]),
        )
        .unwrap();
        apply(
            &mut ed,
            json!([{"op":"transform","id":"C","translation":[4,3,0]}]),
        )
        .unwrap();
        assert_eq!(at(&ed, "A"), [0., 3., 0.]);
        assert_eq!(at(&ed, "B"), [2., 3., 0.]);
        assert_eq!(at(&ed, "C"), [4., 3., 0.]);
        apply(
            &mut ed,
            json!([{"op":"unconstrain","id":"C","kind":"align"}]),
        )
        .unwrap();
        apply(
            &mut ed,
            json!([
                {"op":"transform","id":"B","translation":[2,8,0]},
                {"op":"transform","id":"A","translation":[0,5,0]}
            ]),
        )
        .unwrap();
        assert_eq!(at(&ed, "B"), [2., 5., 0.]);
        assert_eq!(at(&ed, "C"), [4., 3., 0.]);
        apply(
            &mut ed,
            json!([{"op":"constrain","id":"A","align":"xz","from":"B"}]),
        )
        .unwrap();
        assert_eq!(ed.scene().constraints.len(), 1); // Reversed pair replaces, not duplicates.
        apply(
            &mut ed,
            json!([{"op":"unconstrain","id":"B","kind":"align"}]),
        )
        .unwrap();
        assert!(ed.scene().constraints.is_empty());
    }

    #[test]
    fn alignment_uses_group_and_modifier_bounds_and_moves_groups_rigidly() {
        let mut ed = Editor::new();
        apply(&mut ed, json!([
            {"op":"add","name":"A","primitive":{"kind":"cube"}},
            {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[1,0,0]},
            {"op":"add","name":"C","primitive":{"kind":"cube"},"translation":[6,4,0]},
            {"op":"add_modifier","id":"B","modifier":{"type":"array","count":3,"offset":[2,0,0]}}
        ])).unwrap();
        let mut scene = ed.scene().clone();
        scene.objects[0].group = Some("Pair".into());
        scene.objects[1].group = Some("Pair".into());
        ed.load(scene).unwrap();
        apply(
            &mut ed,
            json!([{"op":"constrain","id":"Pair","align":"xy","from":"C"}]),
        )
        .unwrap();
        assert!(close(at(&ed, "A"), [3.5, 4., 0.])); // Pair bounds centre was x=2.5.
        assert!(close(at(&ed, "B"), [4.5, 4., 0.]));
        apply(&mut ed, json!([{"op":"move","id":"Pair","offset":[1,2,3]}])).unwrap();
        assert!(close(at(&ed, "C"), [7., 6., 0.]));
        assert!(close(at(&ed, "B"), [5.5, 6., 3.]));
        apply(&mut ed, json!([{"op":"delete","id":"C"}])).unwrap();
        assert!(ed.scene().constraints.is_empty());
    }

    #[test]
    fn alignment_rejects_overlap_ambiguity_and_conflicts_atomically_and_inspects_loaded_rules() {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([
                {"op":"add","name":"A","primitive":{"kind":"cube"},"translation":[1,0,0]},
                {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[-1,0,0]},
                {"op":"constrain","id":"B","mirrors":"A"}
            ]),
        )
        .unwrap();
        for command in [
            json!({"op":"constrain","id":"B","align":"x","from":"A"}),
            json!({"op":"constrain","id":"A","align":"y","from":"A"}),
            json!({"op":"constrain","id":"B","align":"y"}),
            json!({"op":"constrain","id":"B","align":"y","distance":1,"from":"A"}),
        ] {
            let before = serde_json::to_value(ed.scene()).unwrap();
            assert!(apply(&mut ed, json!([command])).is_err());
            assert_eq!(serde_json::to_value(ed.scene()).unwrap(), before);
        }
        apply(&mut ed, json!([{"op":"unconstrain","id":"B"}, {"op":"constrain","id":"B","align":"y","from":"A"}])).unwrap();
        let mut scene = ed.scene().clone();
        scene.objects[1].transform.translation[1] = 5.;
        ed.load(scene.clone()).unwrap();
        assert!(
            inspect::inspect(&ed)["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["kind"] == "constraint_violation" && i["rule"] == "align")
        );
        scene.constraints[0].subject = scene.constraints[0].rule.other().clone();
        assert!(validate(&scene).is_err());
        assert!(
            serde_json::from_value::<crate::engine::Command>(
                json!({"op":"constrain","id":"B","align":"yx","from":"A"})
            )
            .is_err()
        );
    }

    #[test]
    fn distance_cannot_silently_break_an_earlier_mirror_or_on_rule() {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([
                {"op":"add","name":"A","primitive":{"kind":"cube"}},
                {"op":"add","name":"B","primitive":{"kind":"cube"}},
                {"op":"constrain","id":"B","mirrors":"A"}
            ]),
        )
        .unwrap();
        let before = serde_json::to_value(ed.scene()).unwrap();
        let error = apply(
            &mut ed,
            json!([{"op":"constrain","id":"B","distance":2,"from":"A"}]),
        )
        .unwrap_err();
        assert!(error.to_string().contains("constraint"));
        assert_eq!(serde_json::to_value(ed.scene()).unwrap(), before);
        apply(
            &mut ed,
            json!([{"op":"unconstrain","id":"B"},{"op":"constrain","id":"B","on":"A"}]),
        )
        .unwrap();
        let before = serde_json::to_value(ed.scene()).unwrap();
        assert!(
            apply(
                &mut ed,
                json!([{"op":"constrain","id":"B","distance":0,"from":"A"}])
            )
            .is_err()
        );
        assert_eq!(serde_json::to_value(ed.scene()).unwrap(), before);
        let mut scene = ed.scene().clone();
        scene.objects[1].transform.translation[1] += 3.0;
        ed.load(scene).unwrap();
        assert!(
            inspect::inspect(&ed)["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["kind"] == "constraint_violation" && i["rule"] == "on")
        );
    }

    #[test]
    fn mixed_rules_compare_orientation_instead_of_euler_representations() {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([
                {"op":"add","name":"A","primitive":{"kind":"cube"},"translation":[-1,0,0]},
                {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[1,0,0]},
                {"op":"constrain","id":"B","mirrors":"A"},
                {"op":"constrain","id":"B","distance":2,"from":"A"}
            ]),
        )
        .unwrap();
        let mut scene = ed.scene().clone();
        scene.objects[1].transform.rotation = [std::f64::consts::TAU, 0.0, 0.0];
        ed.load(scene).unwrap();
        assert!(violations(ed.scene(), &ed.scene().constraints).is_empty());
        apply(
            &mut ed,
            json!([{"op":"transform","id":"B","rotation":[std::f64::consts::TAU,0,0]}]),
        )
        .unwrap();
        assert!(violations(ed.scene(), &ed.scene().constraints).is_empty());
    }

    #[test]
    fn distance_follows_either_side_and_undo_restores_the_whole_batch() {
        let mut ed = pair();
        assert!(close(at(&ed, "B"), [2.0, 0.0, 0.0]));
        apply(
            &mut ed,
            json!([{"op":"transform","id":"A","translation":[1,0,0]}]),
        )
        .unwrap();
        assert!(close(at(&ed, "A"), [1.0, 0.0, 0.0]));
        assert!(close(at(&ed, "B"), [3.0, 0.0, 0.0]));
        apply(
            &mut ed,
            json!([{"op":"transform","id":"B","translation":[5,0,0]}]),
        )
        .unwrap();
        assert!(close(at(&ed, "A"), [3.0, 0.0, 0.0]));
        ed.undo().unwrap();
        assert!(close(at(&ed, "A"), [1.0, 0.0, 0.0]));
        assert!(close(at(&ed, "B"), [3.0, 0.0, 0.0]));
    }

    #[test]
    fn distance_both_edited_uses_reference_and_coincident_centres_use_x() {
        let mut ed = pair();
        apply(
            &mut ed,
            json!([
                {"op":"transform","id":"A","translation":[4,0,0]},
                {"op":"transform","id":"B","translation":[4,0,0]}
            ]),
        )
        .unwrap();
        assert!(close(at(&ed, "A"), [4.0, 0.0, 0.0]));
        assert!(close(at(&ed, "B"), [6.0, 0.0, 0.0]));
        apply(
            &mut ed,
            json!([{"op":"constrain","id":"A","distance":0,"from":"B"}]),
        )
        .unwrap();
        assert_eq!(ed.scene().constraints.len(), 1);
        assert!(close(at(&ed, "A"), at(&ed, "B")));
        apply(
            &mut ed,
            json!([{"op":"unconstrain","id":"B","kind":"distance"}]),
        )
        .unwrap();
        assert!(ed.scene().constraints.is_empty());
    }

    #[test]
    fn distance_chains_propagate_from_either_end() {
        let mut ed = pair();
        apply(
            &mut ed,
            json!([
                {"op":"add","name":"C","primitive":{"kind":"cube"},"translation":[4,0,0]},
                {"op":"constrain","id":"C","distance":2,"from":"B"},
                {"op":"transform","id":"C","translation":[8,0,0]}
            ]),
        )
        .unwrap();
        // A newly added constraint is reference-led; edit C in a later batch.
        apply(
            &mut ed,
            json!([{"op":"transform","id":"C","translation":[10,0,0]}]),
        )
        .unwrap();
        assert!(close(at(&ed, "C"), [10.0, 0.0, 0.0]));
        assert!(distance_violations(ed.scene(), &ed.scene().constraints).is_empty());
    }

    #[test]
    fn distance_group_moves_rigidly_and_uses_evaluated_bounds() {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([
                {"op":"add","name":"A","primitive":{"kind":"cube"},"translation":[0,1,0]},
                {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[1,1,0]},

                {"op":"add","name":"C","primitive":{"kind":"cube"},"translation":[6,1,0]},
                {"op":"add_modifier","id":"B","modifier":{"type":"array","count":3,"offset":[2,0,0]}}
            ]),
        )
        .unwrap();
        let mut scene = ed.scene().clone();
        scene.objects[0].group = Some("Pair".into());
        scene.objects[1].group = Some("Pair".into());
        ed.load(scene).unwrap();
        apply(
            &mut ed,
            json!([{"op":"constrain","id":"Pair","distance":3,"from":"C"}]),
        )
        .unwrap();
        // The array extends the group's bounds: its centre is A.x + 2.5,
        // not the centre of the unmodified pair (A.x + 0.5).
        assert!((at(&ed, "A")[0] - 0.5).abs() < 1e-6);
        assert!((at(&ed, "B")[0] - at(&ed, "A")[0] - 1.0).abs() < 1e-6);
        assert!(distance_violations(ed.scene(), &ed.scene().constraints).is_empty());
        apply(
            &mut ed,
            json!([{"op":"transform","id":"C","translation":[7,2,1]}]),
        )
        .unwrap();
        assert!(distance_violations(ed.scene(), &ed.scene().constraints).is_empty());
        assert!((at(&ed, "B")[0] - at(&ed, "A")[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn invalid_and_incompatible_distance_batches_are_atomic() {
        let mut ed = pair();
        for bad in [
            json!({"op":"constrain","id":"B","distance":-1,"from":"A"}),
            json!({"op":"constrain","id":"B","distance":1,"from":"B"}),
            json!({"op":"constrain","id":"B","distance":1}),
            json!({"op":"constrain","id":"B","distance":1,"from":"A","on":"A"}),
        ] {
            let before = serde_json::to_value(ed.scene()).unwrap();
            assert!(
                apply(
                    &mut ed,
                    json!([{"op":"rename","id":"A","name":"Changed"},bad])
                )
                .is_err()
            );
            assert_eq!(serde_json::to_value(ed.scene()).unwrap(), before);
        }
        let before = serde_json::to_value(ed.scene()).unwrap();
        let err = apply(&mut ed, json!([{"op":"constrain","id":"B","mirrors":"A"}])).unwrap_err();
        assert!(err.to_string().contains("distance constraint"));
        assert_eq!(serde_json::to_value(ed.scene()).unwrap(), before);
    }

    #[test]
    fn inspection_reports_loaded_distance_residuals_and_validation_rejects_bad_parameters() {
        let mut ed = pair();
        let mut scene = ed.scene().clone();
        scene.objects[1].transform.translation = [8.0, 0.0, 0.0];
        ed.load(scene.clone()).unwrap();
        let report = inspect::inspect(&ed);
        assert!(
            report["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["kind"] == "constraint_violation"
                    && i["expected"] == 2.0
                    && i["actual"] == 8.0)
        );
        if let Rule::Distance { distance, .. } = &mut scene.constraints[0].rule {
            *distance = f64::NAN;
        }
        assert!(validate(&scene).is_err());
    }

    #[test]
    fn an_object_kept_on_a_support_follows_it_and_keeps_its_own_moves() {
        let mut ed = Editor::new();
        apply(&mut ed, json!([
            {"op": "build", "template": "table", "name": "Table"},
            {"op": "add", "name": "Vase", "primitive": {"kind": "cylinder", "radius": 0.08, "height": 0.3}, "translation": [0.2, 2, 0.1]},
            {"op": "place", "id": "Vase", "on": "Table", "at": [0.7, 0.5]},
            {"op": "constrain", "id": "Vase", "on": "Table"}
        ]))
        .unwrap();
        let start = at(&ed, "Vase");
        assert!((bottom(&ed, "Vase") - table_top(&ed)).abs() < 1e-6);
        // The table moves: the vase comes along, still on top.
        apply(
            &mut ed,
            json!([{"op": "move", "id": "Table", "offset": [1.5, 0, -0.5]}]),
        )
        .unwrap();
        assert!(
            close(at(&ed, "Vase"), [start[0] + 1.5, start[1], start[2] - 0.5]),
            "{:?}",
            at(&ed, "Vase")
        );
        // The vase is moved (and lifted): it keeps its new spot, back on the top.
        let moved = at(&ed, "Vase");
        apply(&mut ed, json!([{"op": "transform", "id": "Vase", "translation": [moved[0] - 0.3, 3.0, moved[2]]}])).unwrap();
        assert!((bottom(&ed, "Vase") - table_top(&ed)).abs() < 1e-6);
        assert!((at(&ed, "Vase")[0] - (moved[0] - 0.3)).abs() < 1e-6);
        // The table is rebuilt taller (through the history): still on top.
        let step = ed.history().0.len() - 2;
        let first = ed.history().0[0].commands.clone();
        let mut taller = serde_json::to_value(&first).unwrap();
        taller[0]["scale"] = json!(1.3);
        ed.revise(1, serde_json::from_value(taller).unwrap())
            .unwrap();
        assert!((bottom(&ed, "Vase") - table_top(&ed)).abs() < 1e-6);
        assert!(step > 0);
        // Deleting the table drops the constraint.
        apply(&mut ed, json!([{"op": "delete", "id": "Table"}])).unwrap();
        assert!(ed.scene().constraints.is_empty());
    }

    #[test]
    fn things_kept_on_one_support_do_not_climb_onto_each_other() {
        let mut ed = Editor::new();
        let mut commands = vec![json!({"op": "build", "template": "table", "name": "Table"})];
        for (i, x) in [0.0, 0.12, 0.24].iter().enumerate() {
            // Overlapping footprints, each meant to sit on the table.
            commands.push(json!({"op": "add", "name": format!("Cup {i}"), "primitive": {"kind": "cylinder", "radius": 0.1, "height": 0.12}, "translation": [x, 2.0, 0.0]}));
            commands.push(json!({"op": "constrain", "id": format!("Cup {i}"), "on": "Table"}));
        }
        apply(&mut ed, json!(commands)).unwrap();
        for _ in 0..3 {
            apply(
                &mut ed,
                json!([{"op": "move", "id": "Table", "offset": [0.2, 0, 0.1]}]),
            )
            .unwrap();
        }
        for i in 0..3 {
            let b = bottom(&ed, &format!("Cup {i}"));
            assert!(
                (b - table_top(&ed)).abs() < 1e-6,
                "cup {i} sits at {b}, the top is at {}",
                table_top(&ed)
            );
        }
    }

    #[test]
    fn mirrored_pairs_follow_whichever_side_moves() {
        let mut ed = Editor::new();
        apply(&mut ed, json!([
            {"op": "add", "name": "Left", "primitive": {"kind": "cube"}, "translation": [-1, 0.5, 0]},
            {"op": "add", "name": "Right", "primitive": {"kind": "cube"}, "translation": [3, 0.5, 2]},
            {"op": "constrain", "id": "Right", "mirrors": "Left"}
        ]))
        .unwrap();
        assert!(
            close(at(&ed, "Right"), [1.0, 0.5, 0.0]),
            "snaps to the mirror at once"
        );
        apply(&mut ed, json!([{"op": "transform", "id": "Left", "translation": [-1.5, 0.5, 0.3], "rotation": [0, 0.4, 0]}])).unwrap();
        let right = &ed.scene().objects[1].transform;
        assert!(close(right.translation, [1.5, 0.5, 0.3]));
        assert!(
            close(right.rotation, [0.0, -0.4, 0.0]),
            "{:?}",
            right.rotation
        );
        // Moving the mirrored one moves the original.
        apply(
            &mut ed,
            json!([{"op": "transform", "id": "Right", "translation": [2, 0.5, -1]}]),
        )
        .unwrap();
        assert!(close(at(&ed, "Left"), [-2.0, 0.5, -1.0]));
        // Front to back, too.
        apply(
            &mut ed,
            json!([{"op": "constrain", "id": "Right", "mirrors": "Left", "axis": "z"}]),
        )
        .unwrap();
        assert!(close(at(&ed, "Right"), [-2.0, 0.5, 1.0]));
        assert_eq!(
            ed.scene().constraints.len(),
            1,
            "a new mirror replaces the old"
        );
        let mirrored = mirror(
            &mirror(&ed.scene().objects[0].transform, Plane::X),
            Plane::X,
        );
        assert!(close(
            mirrored.rotation,
            ed.scene().objects[0].transform.rotation
        ));
    }

    #[test]
    fn matching_materials_spread_along_a_chain() {
        let mut ed = Editor::new();
        apply(&mut ed, json!([
            {"op": "add", "name": "Cup 1", "primitive": {"kind": "cylinder"}, "color": "#ffffff"},
            {"op": "add", "name": "Cup 2", "primitive": {"kind": "cylinder"}, "translation": [1, 0, 0]},
            {"op": "add", "name": "Cup 3", "primitive": {"kind": "cylinder"}, "translation": [2, 0, 0]},
            {"op": "constrain", "id": "Cup 2", "matches": "Cup 1"},
            {"op": "constrain", "id": "Cup 3", "matches": "Cup 1"}
        ]))
        .unwrap();
        // Glazing the middle cup glazes them all.
        apply(
            &mut ed,
            json!([{"op": "material", "id": "Cup 2", "preset": "jade"}]),
        )
        .unwrap();
        let colours: Vec<&str> = ed
            .scene()
            .objects
            .iter()
            .map(|o| o.material.color.as_str())
            .collect();
        assert!(colours.iter().all(|c| *c == colours[1]), "{colours:?}");
        assert_ne!(colours[0], "#ffffff");
        // Unconstrain cuts the link.
        apply(&mut ed, json!([{"op": "unconstrain", "id": "Cup 3"}, {"op": "material", "id": "Cup 1", "color": "#ff0000"}])).unwrap();
        assert_eq!(ed.scene().objects[1].material.color, "#ff0000");
        assert_ne!(ed.scene().objects[2].material.color, "#ff0000");
    }

    #[test]
    fn bad_constraints_are_refused() {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([
                {"op": "add", "name": "A", "primitive": {"kind": "cube"}},
                {"op": "add", "name": "B", "primitive": {"kind": "cube"}, "translation": [0, 2, 0]},
                {"op": "constrain", "id": "B", "on": "A"}
            ]),
        )
        .unwrap();
        for bad in [
            json!({"op": "constrain", "id": "A", "on": "B"}),
            json!({"op": "constrain", "id": "A", "on": "A"}),
            json!({"op": "constrain", "id": "A", "mirrors": "Nope"}),
            json!({"op": "constrain", "id": "A"}),
            json!({"op": "constrain", "id": "A", "on": "B", "matches": "B"}),
            json!({"op": "unconstrain", "id": "A", "kind": "mirrors"}),
        ] {
            assert!(apply(&mut ed, json!([bad.clone()])).is_err(), "{bad}");
        }
        assert_eq!(ed.scene().constraints.len(), 1);
        // Saved scenes keep constraints and are checked on load.
        let mut saved = serde_json::to_value(ed.scene()).unwrap();
        assert_eq!(saved["constraints"][0]["kind"], "on");
        saved["constraints"][0]["support"] = json!(999);
        assert!(ed.load(serde_json::from_value(saved).unwrap()).is_err());
    }
}
