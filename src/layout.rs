//! Arrangements whose intent survives later edits. References lead; item moves
//! snap back. The ordinary `arrange` implementation also drives maintenance,
//! proposals, inspection and history replay.

use std::collections::HashSet;

use glam::{DQuat, DVec3, EulerRot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::assembly::{self, Layout};
use crate::constraint::{self, Who};
use crate::engine::{EngineError, ObjRef, Scene};
use crate::inspect;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Arrangement {
    pub id: u64,
    pub items: Vec<Who>,
    pub layout: Layout,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub around: Option<Who>,
    /// A fixed XZ centre when no reference is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub center: Option<[f64; 2]>,
    pub spacing: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
}

fn reference(who: &Who) -> ObjRef {
    match who {
        Who::Id(id) => ObjRef::Id(*id),
        Who::Group(name) => ObjRef::Name(name.clone()),
    }
}

fn check(scene: &Scene, a: &Arrangement) -> Result<(), EngineError> {
    let bad = |message| EngineError::new(format!("arrangement {}: {message}", a.id));
    if a.items.is_empty() || a.items.len() > 200 {
        return Err(bad("needs 1 to 200 items"));
    }
    if a.around.is_some() == a.center.is_some() {
        return Err(bad("needs either a reference or a fixed centre"));
    }
    if !(a.spacing.is_finite() && (0.0..=100.0).contains(&a.spacing))
        || a.radius
            .is_some_and(|r| !r.is_finite() || !(0.0..=100.0).contains(&r))
        || a.center.is_some_and(|c| !c.iter().all(|v| v.is_finite()))
    {
        return Err(bad(
            "spacing/radius must be finite and between 0 and 100; centre must be finite",
        ));
    }
    let mut seen = HashSet::new();
    for who in &a.items {
        let members = constraint::members(scene, who);
        if members.is_empty() {
            return Err(bad("names a missing item"));
        }
        for i in members {
            if !seen.insert(scene.objects[i].id) {
                return Err(bad("items must be disjoint objects or groups"));
            }
        }
    }
    if let Some(around) = &a.around {
        let members = constraint::members(scene, around);
        if members.is_empty() || members.iter().any(|&i| seen.contains(&scene.objects[i].id)) {
            return Err(bad("reference must exist and be separate from its items"));
        }
    }
    Ok(())
}

pub fn add(
    scene: &mut Scene,
    items: &[ObjRef],
    layout: Layout,
    around: Option<&ObjRef>,
    center: Option<[f64; 2]>,
    spacing: f64,
    radius: Option<f64>,
) -> Result<(), EngineError> {
    if items.is_empty() || items.len() > 200 {
        return Err(EngineError::new("arrange needs 1 to 200 items"));
    }
    if around.is_some() && center.is_some() {
        return Err(EngineError::new(
            "a kept arrangement needs either around or center, not both",
        ));
    }
    let items = items
        .iter()
        .map(|r| constraint::who(scene, r))
        .collect::<Result<Vec<_>, _>>()?;
    let around = around.map(|r| constraint::who(scene, r)).transpose()?;
    let center = if around.is_none() && center.is_none() {
        let solids = inspect::scene_solids(scene)?;
        let mut middle = DVec3::ZERO;
        for item in &items {
            let ids = constraint::members(scene, item)
                .iter()
                .map(|&i| scene.objects[i].id)
                .collect::<Vec<_>>();
            let (lo, hi) = inspect::bounds(&solids, &ids)
                .ok_or_else(|| EngineError::new("nothing with faces to arrange"))?;
            middle += (lo + hi) * 0.5;
        }
        middle /= items.len() as f64;
        Some([middle.x, middle.z])
    } else {
        center
    };
    let set: HashSet<_> = items.iter().cloned().collect();
    let replacing = scene
        .arrangements
        .iter()
        .find(|a| a.items.iter().cloned().collect::<HashSet<_>>() == set)
        .map(|a| a.id);
    let id = match replacing {
        Some(id) => id,
        None => scene
            .arrangements
            .iter()
            .map(|a| a.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| EngineError::new("arrangement ids exhausted"))?,
    };
    let a = Arrangement {
        id,
        items,
        layout,
        around,
        center,
        spacing,
        radius,
    };
    check(scene, &a)?;
    if replacing.is_none() && scene.arrangements.len() >= 32 {
        return Err(EngineError::new("at most 32 maintained arrangements"));
    }
    // A member belongs to one maintained layout. Object/group aliases count too.
    let mine: HashSet<_> = a
        .items
        .iter()
        .flat_map(|w| constraint::members(scene, w))
        .collect();
    if scene
        .arrangements
        .iter()
        .filter(|b| Some(b.id) != replacing)
        .any(|b| {
            b.items
                .iter()
                .flat_map(|w| constraint::members(scene, w))
                .any(|i| mine.contains(&i))
        })
    {
        return Err(EngineError::new(
            "an item already belongs to another maintained arrangement; unarrange it first",
        ));
    }
    apply(scene, &a)?;
    scene.arrangements.retain(|b| Some(b.id) != replacing);
    scene.arrangements.push(a);
    Ok(())
}

pub fn apply(scene: &mut Scene, a: &Arrangement) -> Result<(), EngineError> {
    check(scene, a)?;
    let items: Vec<_> = a.items.iter().map(reference).collect();
    let around = a.around.as_ref().map(reference);
    assembly::arrange(
        scene,
        &items,
        a.layout,
        around.as_ref(),
        a.center,
        a.spacing,
        a.radius,
    )
}

pub fn remove(scene: &mut Scene, id: u64) -> Result<(), EngineError> {
    let before = scene.arrangements.len();
    scene.arrangements.retain(|a| a.id != id);
    if scene.arrangements.len() == before {
        return Err(EngineError::new(format!("no arrangement with id {id}")));
    }
    Ok(())
}

/// Removed members leave the layout; removing its reference drops the rule.
pub fn prune(scene: &mut Scene) {
    let mut arrangements = std::mem::take(&mut scene.arrangements);
    for a in &mut arrangements {
        a.items
            .retain(|w| !constraint::members(scene, w).is_empty());
    }
    arrangements.retain(|a| {
        !a.items.is_empty()
            && a.around
                .as_ref()
                .is_none_or(|w| !constraint::members(scene, w).is_empty())
    });
    scene.arrangements = arrangements;
}

pub fn validate(scene: &Scene) -> Result<(), EngineError> {
    if scene.arrangements.len() > 32 {
        return Err(EngineError::new("at most 32 maintained arrangements"));
    }
    let mut ids = HashSet::new();
    let mut members = HashSet::new();
    for a in &scene.arrangements {
        check(scene, a)?;
        if a.id == 0 || !ids.insert(a.id) {
            return Err(EngineError::new(
                "arrangement ids must be positive and unique",
            ));
        }
        for i in a.items.iter().flat_map(|w| constraint::members(scene, w)) {
            if !members.insert(i) {
                return Err(EngineError::new(
                    "an item belongs to multiple maintained arrangements",
                ));
            }
        }
    }
    Ok(())
}

pub fn violations(scene: &Scene) -> Vec<Value> {
    scene.arrangements.iter().filter_map(|a| {
        let mut expected = scene.clone();
        let error = apply(&mut expected, a).err();
        let differs = a.items.iter().flat_map(|w| constraint::members(scene, w)).any(|i| {
            let x = &scene.objects[i].transform;
            let y = &expected.objects[i].transform;
            let [rx, ry, rz] = x.rotation;
            let [sx, sy, sz] = y.rotation;
            let q = DQuat::from_euler(EulerRot::XYZ, rx, ry, rz);
            let r = DQuat::from_euler(EulerRot::XYZ, sx, sy, sz);
            !q.is_finite() || !r.is_finite() || !(q.abs_diff_eq(r, 1e-6) || q.abs_diff_eq(-r, 1e-6))
                || !DVec3::from(x.translation).abs_diff_eq(DVec3::from(y.translation), 1e-6)
        });
        (error.is_some() || differs).then(|| json!({"kind":"arrangement_violation", "arrangement":a.id, "layout":a.layout,
            "message":format!("arrangement {} cannot be satisfied{}", a.id, error.map(|e| format!(": {e}")).unwrap_or_default())}))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CommandBatch, Editor};

    fn run(ed: &mut Editor, commands: Value) -> Result<(), EngineError> {
        let batch: CommandBatch = serde_json::from_value(json!({"commands":commands})).unwrap();
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

    fn pair() -> Editor {
        let mut ed = Editor::new();
        run(
            &mut ed,
            json!([
                {"op":"add","name":"Anchor","primitive":{"kind":"cube"}},
                {"op":"add","name":"A","primitive":{"kind":"cube"},"translation":[-3,0,0]},
                {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[3,0,0]}
            ]),
        )
        .unwrap();
        ed
    }

    #[test]
    fn rows_follow_the_reference_and_item_moves_snap_back_as_one_undo() {
        let mut ed = pair();
        run(&mut ed, json!([{"op":"arrange","ids":["A","B"],"layout":"row","around":"Anchor","spacing":2,"keep":true}])).unwrap();
        assert_eq!(at(&ed, "A")[0], -1.5);
        assert_eq!(at(&ed, "B")[0], 1.5);
        run(
            &mut ed,
            json!([{"op":"transform","id":"Anchor","translation":[2,0,3]}]),
        )
        .unwrap();
        assert_eq!(at(&ed, "A"), [0.5, 0.5, 3.]);
        assert_eq!(at(&ed, "B"), [3.5, 0.5, 3.]);
        ed.undo().unwrap();
        assert_eq!(at(&ed, "Anchor"), [0., 0., 0.]);
        assert_eq!(at(&ed, "A"), [-1.5, 0.5, 0.]);
        run(
            &mut ed,
            json!([{"op":"transform","id":"A","translation":[-7,4,9]}]),
        )
        .unwrap();
        assert_eq!(at(&ed, "A"), [-1.5, 0.5, 0.]);
        assert!(violations(ed.scene()).is_empty());
    }

    #[test]
    fn grids_recompute_cells_from_evaluated_member_bounds() {
        let mut ed = pair();
        run(&mut ed, json!([
            {"op":"add","name":"C","primitive":{"kind":"cube"}},
            {"op":"add","name":"D","primitive":{"kind":"cube"}},
            {"op":"arrange","ids":["A","B","C","D"],"layout":"grid","center":[10,10],"spacing":0.5,"keep":true}
        ])).unwrap();
        assert_eq!(at(&ed, "A"), [9.25, 0.5, 9.25]);
        run(&mut ed, json!([{"op":"add_modifier","id":"A","modifier":{"type":"array","count":2,"offset":[2,0,0]}}])).unwrap();
        let solids = inspect::scene_solids(ed.scene()).unwrap();
        let id = ed
            .scene()
            .objects
            .iter()
            .find(|o| o.name == "A")
            .unwrap()
            .id;
        let (lo, hi) = inspect::bounds(&solids, &[id]).unwrap();
        assert!(((lo.x + hi.x) * 0.5 - 8.25).abs() < 1e-6);
        assert_eq!(at(&ed, "B")[0], 11.75);
        assert!(violations(ed.scene()).is_empty());
    }

    #[test]
    fn circles_replay_idempotently_face_the_centre_and_preserve_groups() {
        let mut ed = Editor::new();
        run(&mut ed, json!([
            {"op":"build","template":"table","name":"Table"},
            {"op":"build","template":"chair","name":"A"},
            {"op":"build","template":"chair","name":"B"},
            {"op":"build","template":"chair","name":"C"},
            {"op":"build","template":"chair","name":"D"},
            {"op":"arrange","ids":["A","B","C","D"],"layout":"circle","around":"Table","radius":2,"keep":true}
        ])).unwrap();
        let original = ed.scene().clone();
        let part = original
            .objects
            .iter()
            .find(|o| o.group.as_deref() == Some("A"))
            .unwrap()
            .id;
        for _ in 0..6 {
            run(
                &mut ed,
                json!([{"op":"material","id":part,"color":"#ffffff"}]),
            )
            .unwrap();
            for (a, b) in ed.scene().objects.iter().zip(&original.objects) {
                assert!(
                    DVec3::from(a.transform.translation)
                        .abs_diff_eq(DVec3::from(b.transform.translation), 1e-6)
                );
                assert!(
                    DVec3::from(a.transform.rotation)
                        .abs_diff_eq(DVec3::from(b.transform.rotation), 1e-6)
                );
            }
        }
        run(
            &mut ed,
            json!([{"op":"move","id":"Table","offset":[3,0,-1]}]),
        )
        .unwrap();
        for (a, b) in ed.scene().objects.iter().zip(&original.objects) {
            assert!(DVec3::from(a.transform.translation).abs_diff_eq(
                DVec3::from(b.transform.translation) + DVec3::new(3., 0., -1.),
                1e-6
            ));
        }
        assert!(violations(ed.scene()).is_empty());
    }

    #[test]
    fn automatic_circle_radius_tracks_reference_size_and_fixed_centres_stay_fixed() {
        let mut ed = pair();
        run(&mut ed, json!([{"op":"arrange","ids":["A","B"],"layout":"circle","around":"Anchor","spacing":1,"keep":true}])).unwrap();
        assert!((at(&ed, "A")[2] - 2.).abs() < 1e-6);
        run(
            &mut ed,
            json!([{"op":"transform","id":"Anchor","scale":[4,1,4]}]),
        )
        .unwrap();
        assert!((at(&ed, "A")[2] - 3.5).abs() < 1e-6);
        run(&mut ed, json!([{"op":"arrange","ids":["B","A"],"layout":"row","center":[10,5],"spacing":1,"keep":true}])).unwrap();
        assert_eq!(ed.scene().arrangements.len(), 1);
        assert_eq!(ed.scene().arrangements[0].id, 1);
        run(
            &mut ed,
            json!([{"op":"move","id":"Anchor","offset":[1,0,1]}]),
        )
        .unwrap();
        assert_eq!(at(&ed, "B"), [9., 0.5, 5.]);
    }

    #[test]
    fn removed_members_reflow_and_removed_references_drop_the_rule() {
        let mut ed = pair();
        run(
            &mut ed,
            json!([{"op":"arrange","ids":["A","B"],"layout":"row","center":[10,0],"keep":true}]),
        )
        .unwrap();
        run(&mut ed, json!([{"op":"delete","id":"A"}])).unwrap();
        assert_eq!(ed.scene().arrangements[0].items.len(), 1);
        assert_eq!(at(&ed, "B")[0], 10.);
        ed.undo().unwrap();
        assert_eq!(ed.scene().arrangements[0].items.len(), 2);
        run(
            &mut ed,
            json!([{"op":"arrange","ids":["A","B"],"layout":"row","around":"Anchor","keep":true}]),
        )
        .unwrap();
        run(&mut ed, json!([{"op":"delete","id":"Anchor"}])).unwrap();
        assert!(ed.scene().arrangements.is_empty());
        ed.undo().unwrap();
        run(&mut ed, json!([{"op":"unarrange","id":1}])).unwrap();
        run(
            &mut ed,
            json!([{"op":"transform","id":"B","translation":[8,1,2]}]),
        )
        .unwrap();
        assert_eq!(at(&ed, "B"), [8., 1., 2.]);
    }

    #[test]
    fn overlapping_members_ambiguous_centres_and_conflicting_rules_are_atomic() {
        let mut ed = pair();
        for command in [
            json!({"op":"arrange","ids":["A","A"],"layout":"row","keep":true}),
            json!({"op":"arrange","ids":["A","B"],"around":"A","layout":"row","keep":true}),
            json!({"op":"arrange","ids":["A","B"],"around":"Anchor","center":[0,0],"layout":"row","keep":true}),
            json!({"op":"arrange","ids":["A","B"],"layout":"grid","spacing":-1,"keep":true}),
        ] {
            let before = ed.scene().clone();
            assert!(run(&mut ed, json!([command])).is_err());
            assert_eq!(*ed.scene(), before);
        }
        run(
            &mut ed,
            json!([{"op":"constrain","id":"A","align":"x","from":"Anchor"}]),
        )
        .unwrap();
        let before = ed.scene().clone();
        assert!(run(&mut ed, json!([{"op":"arrange","ids":["A","B"],"around":"Anchor","layout":"row","spacing":1,"keep":true}])).is_err());
        assert_eq!(*ed.scene(), before);
    }

    #[test]
    fn scene_roundtrip_inspection_and_validation_preserve_maintained_intent() {
        let mut ed = pair();
        run(
            &mut ed,
            json!([{"op":"arrange","ids":["A","B"],"layout":"row","keep":true}]),
        )
        .unwrap();
        assert_eq!(ed.scene().arrangements[0].center, Some([0., 0.]));
        let mut scene: Scene =
            serde_json::from_value(serde_json::to_value(ed.scene()).unwrap()).unwrap();
        scene.objects[1].transform.translation[0] += 4.;
        ed.load(scene.clone()).unwrap();
        assert!(
            inspect::inspect(&ed)["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["kind"] == "arrangement_violation")
        );
        scene.arrangements[0].spacing = f64::NAN;
        assert!(validate(&scene).is_err());
        scene.arrangements[0].spacing = 1.;
        scene.arrangements[0]
            .items
            .push(Who::Id(scene.objects[1].id));
        assert!(validate(&scene).is_err());
    }

    #[test]
    fn one_layout_per_member_includes_group_aliases_and_loaded_ids_are_checked() {
        let mut ed = pair();
        run(
            &mut ed,
            json!([
                {"op":"add","name":"C","primitive":{"kind":"cube"},"translation":[20,0,0]},
                {"op":"arrange","ids":["A","B"],"layout":"row","center":[10,0],"keep":true}
            ]),
        )
        .unwrap();
        let before = ed.scene().clone();
        assert!(
            run(
                &mut ed,
                json!([{"op":"arrange","ids":["A","C"],"layout":"row","keep":true}])
            )
            .is_err()
        );
        assert_eq!(*ed.scene(), before);
        let mut scene = before;
        scene.objects[1].group = Some("Pair".into());
        scene.objects[2].group = Some("Pair".into());
        ed.load(scene.clone()).unwrap();
        assert!(
            run(
                &mut ed,
                json!([{"op":"arrange","ids":["Pair","A"],"layout":"row","keep":true}])
            )
            .is_err()
        );
        scene.arrangements.push(scene.arrangements[0].clone());
        assert!(validate(&scene).is_err());
        scene.arrangements.pop();
        scene.arrangements[0].id = 0;
        assert!(validate(&scene).is_err());
        scene.arrangements[0].id = u64::MAX;
        ed.load(scene).unwrap();
        assert!(
            run(
                &mut ed,
                json!([{"op":"arrange","ids":["C"],"layout":"row","center":[20,0],"keep":true}])
            )
            .unwrap_err()
            .to_string()
            .contains("ids exhausted")
        );
    }

    #[test]
    fn incompatible_reference_cycles_are_refused_without_partial_edits() {
        let mut ed = pair();
        run(
            &mut ed,
            json!([{"op":"arrange","ids":["A"],"layout":"row","around":"B","keep":true}]),
        )
        .unwrap();
        let before = ed.scene().clone();
        let error = run(&mut ed, json!([{"op":"arrange","ids":["B"],"layout":"circle","around":"A","radius":2,"keep":true}])).unwrap_err();
        assert!(error.to_string().contains("arrangement"));
        assert_eq!(*ed.scene(), before);
    }

    #[test]
    fn proposal_diffs_describe_intent_even_when_no_geometry_changes() {
        let mut ed = pair();
        run(
            &mut ed,
            json!([{"op":"arrange","ids":["A","B"],"layout":"row","center":[10,0],"spacing":2}]),
        )
        .unwrap();
        let request = |command| {
            serde_json::from_value::<crate::proposal::ProposalRequest>(
                json!({"title":"Keep intent","commands":[command]}),
            )
            .unwrap()
        };
        ed.propose(&request(json!({"op":"arrange","ids":["A","B"],"layout":"row","center":[10,0],"spacing":2,"keep":true}))).unwrap();
        assert!(ed.proposals()[0].diff.changed.is_empty());
        assert_eq!(ed.proposals()[0].diff.scene, ["arrangements"]);
        ed.propose(&request(
            json!({"op":"constrain","id":"B","align":"y","from":"A"}),
        ))
        .unwrap();
        assert!(ed.proposals()[1].diff.changed.is_empty());
        assert_eq!(ed.proposals()[1].diff.scene, ["constraints"]);
        assert!(ed.scene().arrangements.is_empty());
        assert!(ed.scene().constraints.is_empty());
    }
}
