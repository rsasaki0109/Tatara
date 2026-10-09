//! Constraints: relations that stay true through every later edit, so the
//! scene keeps its intent instead of just its coordinates. An object (or a
//! group, such as a built table) can be kept **on** another, keeping its
//! place on it; kept the **mirror** image of another across a plane; or
//! kept **matching** another's material.
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
}

impl Rule {
    pub fn kind(&self) -> Kind {
        match self {
            Rule::On { .. } => Kind::On,
            Rule::Mirrors { .. } => Kind::Mirrors,
            Rule::Matches { .. } => Kind::Matches,
        }
    }

    fn other(&self) -> &Who {
        match self {
            Rule::On { support, .. } => support,
            Rule::Mirrors { of, .. } | Rule::Matches { of } => of,
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
        Request::Matches(r) => {
            let of = who(scene, &r)?;
            if overlaps(scene, &subject, &of) {
                return err("an object cannot match itself");
            }
            Rule::Matches { of }
        }
    };
    let kind = rule.kind();
    // Two objects mirror or match each other once, whichever way round.
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
}

/// Drop `subject`'s constraints (of one kind, or all), and those of others
/// that mirror or match it.
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
    if scene.constraints.is_empty() {
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
    if scene.constraints.is_empty() {
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
        for _ in 0..3 {
            for c in &constraints {
                let fresh = !before.constraints.contains(&c.id);
                let subject = touched(scene, &dirty, &c.subject);
                let other = touched(scene, &dirty, c.rule.other());
                let lead = match (fresh, subject, other) {
                    (false, false, false) => continue,
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
            assembly::settle_from_above(scene, &subject)
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
        if let Rule::On { offset, .. } = &c.rule
            && !offset.iter().all(|v| v.is_finite())
        {
            return err(format!("constraint {} has a bad offset", c.id));
        }
    }
    Ok(())
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
