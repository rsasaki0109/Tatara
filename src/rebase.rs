//! Conservative optimistic collaboration. Reapply a stale edit only when every
//! object it reads (including group members and connected constraints) is still
//! identical to the retained context, or only unrelated static properties changed. Other edits remain in the current scene.

use std::collections::HashSet;

use crate::constraint::{self, Who};
use crate::engine::{self, Command, EngineError, ObjRef, Scene};
use crate::proposal;

fn target(command: &Command) -> Result<&ObjRef, EngineError> {
    match command {
        Command::LightSettings { id, .. }
        | Command::CameraSettings { id, .. }
        | Command::Transform { id, .. }
        | Command::Material { id, .. }
        | Command::Rename { id, .. }
        | Command::Move { id, .. }
        | Command::Shade { id, .. }
        | Command::Delete { id }
        | Command::Extrude { id, .. }
        | Command::Subdivide { id, .. }
        | Command::Inset { id, .. }
        | Command::MoveVertices { id, .. }
        | Command::Bevel { id, .. }
        | Command::LoopCut { id, .. }
        | Command::Sculpt { id, .. }
        | Command::AddModifier { id, .. }
        | Command::SetModifier { id, .. }
        | Command::RemoveModifier { id, .. }
        | Command::ApplyModifiers { id }
        | Command::Rig { id, .. }
        | Command::Pose { id, .. }
        | Command::Unwrap { id, .. }
        | Command::TransformUvs { id, .. }
        | Command::MarkSeams { id, .. } => Ok(id),
        _ => Err(EngineError::new(
            "this command creates objects or reads scene-wide state and needs a fresh revision",
        )),
    }
}

fn ids(scene: &Scene, who: &Who) -> Vec<u64> {
    constraint::members(scene, who)
        .iter()
        .map(|&i| scene.objects[i].id)
        .collect()
}

fn members_unchanged(base: &Scene, current: &Scene, who: &Who) -> Result<Vec<u64>, EngineError> {
    let members = ids(base, who);
    if members.is_empty() || members != ids(current, who) {
        return Err(EngineError::new(format!("membership changed for {who:?}")));
    }
    Ok(members)
}

// Only explicit, independent static fields can relax object-level rejection.
// Unknown commands, structural changes and relation/animation dependencies stay strict.
fn independent_properties(
    scene: &Scene,
    original: &engine::Object,
    current: &engine::Object,
    commands: &[Command],
) -> bool {
    if original.group.is_some()
        || !original.tracks.is_empty()
        || !current.tracks.is_empty()
        || !original.bones.is_empty()
        || !current.bones.is_empty()
        || scene.ik_targets.iter().any(|target| {
            target.rig == original.id
                || target.target == original.id
                || target.pole == Some(original.id)
        })
        || scene.constraints.iter().any(|c| {
            ids(scene, &c.subject).contains(&original.id)
                || ids(scene, c.rule.other()).contains(&original.id)
        })
    {
        return false;
    }
    const TRANSLATION: u16 = 1;
    const ROTATION: u16 = 2;
    const SCALE: u16 = 4;
    const COLOR: u16 = 8;
    const ROUGHNESS: u16 = 16;
    const METALNESS: u16 = 32;
    const EMISSIVE: u16 = 64;
    const EMISSIVE_STRENGTH: u16 = 128;
    const OPACITY: u16 = 256;
    const TRANSMISSION: u16 = 512;
    const TEXTURE: u16 = 1024;
    const MATERIAL: u16 = COLOR
        | ROUGHNESS
        | METALNESS
        | EMISSIVE
        | EMISSIVE_STRENGTH
        | OPACITY
        | TRANSMISSION
        | TEXTURE;
    let flag = |present: bool, bit: u16| if present { bit } else { 0 };
    let mut protected = 0;
    for command in commands {
        let Ok(reference) = target(command) else {
            return false;
        };
        let Ok(who) = constraint::who(scene, reference) else {
            return false;
        };
        if who != Who::Id(original.id) {
            if ids(scene, &who).contains(&original.id) {
                return false;
            }
            continue;
        }
        let fields = match command {
            Command::Move {
                offset: Some(_),
                rotate_y: None,
                ..
            } => TRANSLATION,
            Command::Transform {
                translation,
                rotation,
                scale,
                ..
            } => {
                flag(translation.is_some(), TRANSLATION)
                    | flag(rotation.is_some(), ROTATION)
                    | flag(scale.is_some(), SCALE)
            }
            Command::Material {
                preset,
                color,
                roughness,
                metalness,
                emissive,
                emissive_strength,
                opacity,
                transmission,
                texture,
                ..
            } => {
                if preset.is_some() {
                    MATERIAL
                } else {
                    flag(color.is_some(), COLOR)
                        | flag(roughness.is_some(), ROUGHNESS)
                        | flag(metalness.is_some(), METALNESS)
                        | flag(emissive.is_some(), EMISSIVE)
                        | flag(emissive_strength.is_some(), EMISSIVE_STRENGTH)
                        | flag(opacity.is_some(), OPACITY)
                        | flag(transmission.is_some(), TRANSMISSION)
                        | flag(texture.is_some(), TEXTURE)
                }
            }
            _ => return false,
        };
        if fields == 0 {
            return false;
        }
        protected |= fields;
    }
    if protected == 0 {
        return false;
    }
    fn field<T: Clone + PartialEq>(claimed: bool, observed: &mut T, old: &T) -> bool {
        if claimed && *observed != *old {
            return false;
        }
        observed.clone_from(old);
        true
    }
    let mut observed = current.clone();
    // Normalize only known static fields after checking all claimed ones.
    // Full Object equality below keeps new or unknown structural fields strict.
    if !field(
        protected & TRANSLATION != 0,
        &mut observed.transform.translation,
        &original.transform.translation,
    ) || !field(
        protected & ROTATION != 0,
        &mut observed.transform.rotation,
        &original.transform.rotation,
    ) || !field(
        protected & SCALE != 0,
        &mut observed.transform.scale,
        &original.transform.scale,
    ) || !field(
        protected & COLOR != 0,
        &mut observed.material.color,
        &original.material.color,
    ) || !field(
        protected & ROUGHNESS != 0,
        &mut observed.material.roughness,
        &original.material.roughness,
    ) || !field(
        protected & METALNESS != 0,
        &mut observed.material.metalness,
        &original.material.metalness,
    ) || !field(
        protected & EMISSIVE != 0,
        &mut observed.material.emissive,
        &original.material.emissive,
    ) || !field(
        protected & EMISSIVE_STRENGTH != 0,
        &mut observed.material.emissive_strength,
        &original.material.emissive_strength,
    ) || !field(
        protected & OPACITY != 0,
        &mut observed.material.opacity,
        &original.material.opacity,
    ) || !field(
        protected & TRANSMISSION != 0,
        &mut observed.material.transmission,
        &original.material.transmission,
    ) || !field(
        protected & TEXTURE != 0,
        &mut observed.material.texture,
        &original.material.texture,
    ) {
        return false;
    }
    observed == *original
}

pub(crate) fn validate(
    base: &Scene,
    current: &Scene,
    commands: &[Command],
) -> Result<(), EngineError> {
    if commands.is_empty() {
        return Err(EngineError::new("batch has no commands"));
    }
    let creations = commands.iter().all(|command| {
        matches!(
            command,
            Command::Add { .. } | Command::AddCamera { .. } | Command::AddLight { .. }
        )
    });
    if !creations {
        for command in commands {
            target(command)?;
        }
    }
    if base.world != current.world
        || base.images != current.images
        || base.animation != current.animation
        || base.constraints != current.constraints
        || base.ik_targets != current.ik_targets
        || base.arrangements != current.arrangements
    {
        return Err(EngineError::new(
            "scene-wide settings or relation rules changed",
        ));
    }
    if !base.arrangements.is_empty() {
        return Err(EngineError::new(
            "maintained arrangements read scene-wide support geometry and need a fresh revision",
        ));
    }
    // This is the same validated clone/reapply path used to preview proposals.
    if creations {
        // Validate the old intent as well as the actual replay. IDs and generated
        // names belong to the current scene, never to the discarded candidate.
        let (candidate, old_ids) = engine::run(base, commands)?;
        let (replayed, new_ids) = engine::run(current, commands)?;
        for (scene, existing, created) in
            [(&candidate, base, &old_ids), (&replayed, current, &new_ids)]
        {
            let mut names = HashSet::new();
            for object in scene.objects.iter().filter(|o| created.contains(&o.id)) {
                if !names.insert(&object.name)
                    || existing
                        .objects
                        .iter()
                        .any(|o| o.name == object.name || o.group.as_ref() == Some(&object.name))
                {
                    return Err(EngineError::new(format!(
                        "creation name {:?} conflicts with an object or group",
                        object.name
                    )));
                }
            }
        }
        return Ok(());
    }
    let (candidate, _) = engine::run(base, commands)?;
    let mut dependencies = HashSet::new();
    for command in commands {
        let r = target(command)?;
        let original = constraint::who(base, r).ok();
        let who = original
            .clone()
            .map(Ok)
            .unwrap_or_else(|| constraint::who(&candidate, r))?;
        // A concurrent object with a group's or internally renamed object's name
        // must not redirect the replay to a different object.
        let now = constraint::who(current, r).ok();
        if original.is_some() && now != original || now.is_some() && now.as_ref() != Some(&who) {
            return Err(EngineError::new(format!(
                "the reference {r:?} now names a different object or group"
            )));
        }
        dependencies.extend(members_unchanged(base, current, &who)?);
    }
    let diff = proposal::diff(base, &candidate);
    dependencies.extend(diff.changed.iter().map(|o| o.id));
    dependencies.extend(diff.removed.iter().map(|o| o.id));
    // Relations create read dependencies even if the original command happened
    // not to move its partner. Walk the full connected component to catch chains.
    loop {
        let before = dependencies.len();
        for c in &base.constraints {
            let subject = ids(base, &c.subject);
            let other = ids(base, c.rule.other());
            if subject
                .iter()
                .chain(&other)
                .any(|id| dependencies.contains(id))
            {
                dependencies.extend(members_unchanged(base, current, &c.subject)?);
                dependencies.extend(members_unchanged(base, current, c.rule.other())?);
            }
        }
        if dependencies.len() == before {
            break;
        }
    }
    for id in dependencies {
        let old = base.objects.iter().find(|o| o.id == id);
        let now = current.objects.iter().find(|o| o.id == id);
        if old.is_some() && old == now {
            continue;
        }
        if let (Some(original), Some(current_object)) = (old, now)
            && independent_properties(base, original, current_object, commands)
        {
            continue;
        }
        let name = old.map(|o| o.name.as_str()).unwrap_or("unknown object");
        return Err(EngineError::new(format!(
            "object {name:?} (#{id}) was changed or removed by another edit"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ApplyResult, CommandBatch, Editor};
    use serde_json::{Value, json};

    fn apply(
        ed: &mut Editor,
        commands: Value,
        expected: Option<u64>,
        rebase: bool,
    ) -> Result<ApplyResult, EngineError> {
        let batch: CommandBatch = serde_json::from_value(json!({"commands":commands,"expected_revision":expected,"rebase":rebase,"actor":{"id":"bob","name":"Bob"},"source":"test"})).unwrap();
        ed.apply(&batch)
    }

    fn pair() -> Editor {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([
                {"op":"add","name":"A","primitive":{"kind":"cube"}},
                {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[3,0,0]}
            ]),
            None,
            false,
        )
        .unwrap();
        ed
    }

    #[test]
    fn independent_creations_allocate_current_ids_and_keep_one_undo() {
        let mut ed = pair();
        let first = apply(
            &mut ed,
            json!([{"op":"add","primitive":{"kind":"cube"}}]),
            Some(1),
            false,
        )
        .unwrap();
        let second = apply(
            &mut ed,
            json!([
                {"op":"add","primitive":{"kind":"cube"}},
                {"op":"add_camera","name":"Shot"},
                {"op":"add_light","name":"Key"}
            ]),
            Some(1),
            true,
        )
        .unwrap();
        assert_eq!(
            second.created,
            vec![
                first.created[0] + 1,
                first.created[0] + 2,
                first.created[0] + 3
            ]
        );
        assert_eq!(second.rebased_from, Some(1));
        assert_eq!(
            ed.history().0.last().unwrap().actor.as_ref().unwrap().name,
            "Bob"
        );
        let final_scene = ed.scene().clone();
        assert_eq!(final_scene.objects.len(), 6);
        ed.undo().unwrap();
        assert_eq!(ed.scene().objects.len(), 3);
        assert_eq!(ed.scene().objects.last().unwrap().id, first.created[0]);
        ed.redo().unwrap();
        assert_eq!(ed.scene().objects, final_scene.objects);
    }

    #[test]
    fn creation_name_collisions_and_invalid_old_intent_leave_everything_unchanged() {
        let mut ed = pair();
        apply(
            &mut ed,
            json!([{ "op":"build","template":"table","name":"Table"}]),
            None,
            false,
        )
        .unwrap();
        for commands in [
            json!([{"op":"add_camera","name":"Table"}]),
            json!([{"op":"add_light","name":"A"}]),
            json!([{"op":"add_camera","name":"Same"},{"op":"add_light","name":"Same"}]),
            json!([{"op":"add_camera"},{"op":"add_light","name":"Camera"}]),
            json!([{"op":"add_camera","lens":{"fov":0}}]),
        ] {
            let before = ed.scene().clone();
            let history = ed.history().0.len();
            assert!(apply(&mut ed, commands, Some(1), true).unwrap_err().stale);
            assert_eq!(*ed.scene(), before);
            assert_eq!(ed.history().0.len(), history);
        }
    }

    #[test]
    fn current_light_capacity_rejects_stale_creation_atomically() {
        let mut ed = pair();
        for i in 0..16 {
            apply(
                &mut ed,
                json!([{"op":"add_light","name":format!("Light {i}")}]),
                None,
                false,
            )
            .unwrap();
        }
        let before = ed.scene().clone();
        let error = apply(
            &mut ed,
            json!([{"op":"add_camera"},{"op":"add_light","name":"Extra"}]),
            Some(1),
            true,
        )
        .unwrap_err();
        assert!(error.stale);
        assert_eq!(*ed.scene(), before);
        assert_eq!(ed.history().0.len(), 17);
    }

    #[test]
    fn independent_edits_rebase_as_one_undo_with_attribution() {
        let mut ed = pair();
        apply(
            &mut ed,
            json!([{"op":"transform","id":"A","translation":[1,0,0]}]),
            Some(1),
            false,
        )
        .unwrap();
        let result = apply(
            &mut ed,
            json!([{"op":"material","id":"B","color":"#ff0000"}]),
            Some(1),
            true,
        )
        .unwrap();
        assert_eq!(result.rebased_from, Some(1));
        assert_eq!(result.revision, 3);
        assert_eq!(ed.scene().objects[0].transform.translation[0], 1.);
        assert_eq!(ed.scene().objects[1].material.color, "#ff0000");
        assert_eq!(ed.history().0[2].rebased_from, Some(1));
        assert_eq!(ed.history().0[2].actor.as_ref().unwrap().name, "Bob");
        ed.undo().unwrap();
        assert_eq!(ed.scene().objects[0].transform.translation[0], 1.);
        assert_ne!(ed.scene().objects[1].material.color, "#ff0000");
        ed.redo().unwrap();
        assert_eq!(ed.scene().objects[1].material.color, "#ff0000");
    }

    #[test]
    fn strict_is_default_and_same_target_conflicts_are_atomic_including_noops() {
        let mut ed = pair();
        apply(
            &mut ed,
            json!([{"op":"transform","id":"A","translation":[1,0,0]}]),
            None,
            false,
        )
        .unwrap();
        let before = ed.scene().clone();
        let strict = apply(
            &mut ed,
            json!([{"op":"material","id":"B","color":"#ffffff"}]),
            Some(1),
            false,
        )
        .unwrap_err();
        assert!(strict.stale);
        // This transform was a no-op in the context, but would overwrite Alice now.
        let conflict = apply(
            &mut ed,
            json!([
                {"op":"material","id":"B","color":"#ffffff"},
                {"op":"transform","id":"A","translation":[0,0,0]}
            ]),
            Some(1),
            true,
        )
        .unwrap_err();
        assert!(conflict.stale && conflict.message.contains("A"));
        assert_eq!(*ed.scene(), before);
        assert_eq!(ed.history().0.len(), 2);
    }

    #[test]
    fn connected_constraint_partners_are_read_dependencies_even_without_geometry_changes() {
        let mut ed = pair();
        apply(
            &mut ed,
            json!([{"op":"constrain","id":"B","distance":2,"from":"A"}]),
            None,
            false,
        )
        .unwrap();
        apply(
            &mut ed,
            json!([{"op":"shade","id":"B","smooth":true}]),
            None,
            false,
        )
        .unwrap();
        let error = apply(
            &mut ed,
            json!([{"op":"transform","id":"A","translation":[0,0,0]}]),
            Some(2),
            true,
        )
        .unwrap_err();
        assert!(error.stale && error.message.contains("B"));
    }

    #[test]
    fn groups_and_raw_name_shadowing_cannot_redirect_replayed_commands() {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([
                {"op":"build","template":"table","name":"Table"},
                {"op":"add","name":"Other","primitive":{"kind":"cube"},"translation":[3,0,0]}
            ]),
            None,
            false,
        )
        .unwrap();
        apply(
            &mut ed,
            json!([{"op":"transform","id":"Other","translation":[4,0,0]}]),
            None,
            false,
        )
        .unwrap();
        apply(
            &mut ed,
            json!([{"op":"move","id":"Table","offset":[1,0,0]}]),
            Some(1),
            true,
        )
        .unwrap();
        apply(
            &mut ed,
            json!([{"op":"add","name":"Table","primitive":{"kind":"cube"}}]),
            None,
            false,
        )
        .unwrap();
        let before = ed.scene().clone();
        let error = apply(
            &mut ed,
            json!([{"op":"move","id":"Table","offset":[1,0,0]}]),
            Some(3),
            true,
        )
        .unwrap_err();
        assert!(error.stale && error.message.contains("reference"));
        assert_eq!(*ed.scene(), before);
    }

    #[test]
    fn topology_changes_and_deleted_targets_are_conflicts_but_other_meshes_are_independent() {
        let mut ed = pair();
        apply(
            &mut ed,
            json!([{"op":"subdivide","id":"A","levels":1}]),
            None,
            false,
        )
        .unwrap();
        apply(
            &mut ed,
            json!([{"op":"extrude","id":"B","face":0,"distance":0.2}]),
            Some(1),
            true,
        )
        .unwrap();
        let before = ed.scene().clone();
        assert!(
            apply(
                &mut ed,
                json!([{"op":"extrude","id":"A","face":0,"distance":0.2}]),
                Some(1),
                true
            )
            .unwrap_err()
            .stale
        );
        assert_eq!(*ed.scene(), before);
        apply(&mut ed, json!([{"op":"delete","id":"A"}]), None, false).unwrap();
        assert!(
            apply(
                &mut ed,
                json!([{"op":"material","id":"A","color":"#ffffff"}]),
                Some(3),
                true
            )
            .unwrap_err()
            .stale
        );
    }

    #[test]
    fn unsupported_mixed_batches_global_changes_loaded_bases_and_unavailable_contexts_are_refused()
    {
        let mut ed = pair();
        apply(
            &mut ed,
            json!([{"op":"transform","id":"A","translation":[1,0,0]}]),
            None,
            false,
        )
        .unwrap();
        assert!(
            apply(
                &mut ed,
                json!([{"op":"add","primitive":{"kind":"cube"}},{"op":"material","id":"B","color":"#ffffff"}]),
                Some(1),
                true
            )
            .unwrap_err()
            .message
            .contains("fresh revision")
        );
        assert!(
            apply(
                &mut ed,
                json!([{"op":"material","id":"B","color":"#ffffff"}]),
                Some(99),
                true
            )
            .unwrap_err()
            .stale
        );
        let scene = ed.scene().clone();
        ed.load(scene).unwrap();
        assert!(
            apply(
                &mut ed,
                json!([{"op":"material","id":"B","color":"#ffffff"}]),
                Some(2),
                true
            )
            .unwrap_err()
            .message
            .contains("replaced")
        );
        ed.reset();
        assert!(
            apply(
                &mut ed,
                json!([{"op":"material","id":"B","color":"#ffffff"}]),
                Some(1),
                true
            )
            .unwrap_err()
            .message
            .contains("available")
        );
        let mut ed = pair();
        apply(&mut ed, json!([{"op":"world","sky":"sunset"}]), None, false).unwrap();
        assert!(
            apply(
                &mut ed,
                json!([{"op":"material","id":"B","color":"#ffffff"}]),
                Some(1),
                true
            )
            .unwrap_err()
            .message
            .contains("scene-wide")
        );
    }

    #[test]
    fn internally_renamed_targets_can_rebase_but_concurrent_name_collisions_cannot() {
        let mut ed = pair();
        apply(
            &mut ed,
            json!([{"op":"material","id":"B","color":"#ffffff"}]),
            None,
            false,
        )
        .unwrap();
        apply(
            &mut ed,
            json!([
                {"op":"rename","id":"A","name":"Fresh"},
                {"op":"transform","id":"Fresh","translation":[1,0,0]}
            ]),
            Some(1),
            true,
        )
        .unwrap();
        assert_eq!(ed.scene().objects[0].name, "Fresh");
        let mut ed = pair();
        apply(
            &mut ed,
            json!([{"op":"add","name":"Fresh","primitive":{"kind":"cube"}}]),
            None,
            false,
        )
        .unwrap();
        let before = ed.scene().clone();
        assert!(
            apply(
                &mut ed,
                json!([
                    {"op":"rename","id":"A","name":"Fresh"},
                    {"op":"transform","id":"Fresh","translation":[1,0,0]}
                ]),
                Some(1),
                true
            )
            .unwrap_err()
            .stale
        );
        assert_eq!(*ed.scene(), before);
    }

    #[test]
    fn maintained_layouts_require_fresh_geometry_context_but_current_edits_still_apply() {
        let mut ed = pair();
        apply(
            &mut ed,
            json!([{"op":"arrange","ids":["B"],"layout":"row","center":[3,0],"keep":true}]),
            None,
            false,
        )
        .unwrap();
        apply(
            &mut ed,
            json!([{"op":"transform","id":"A","translation":[1,0,0]}]),
            None,
            false,
        )
        .unwrap();
        let before = ed.scene().clone();
        let error = apply(
            &mut ed,
            json!([{"op":"material","id":"B","color":"#ffffff"}]),
            Some(2),
            true,
        )
        .unwrap_err();
        assert!(error.stale && error.message.contains("support geometry"));
        assert_eq!(*ed.scene(), before);
        let result = apply(
            &mut ed,
            json!([{"op":"material","id":"B","color":"#ffffff"}]),
            Some(3),
            true,
        )
        .unwrap();
        assert_eq!(result.rebased_from, None);
    }

    #[test]
    fn discarded_old_contexts_are_not_reconstructed_or_guessed() {
        let mut ed = pair();
        for _ in 0..205 {
            apply(
                &mut ed,
                json!([{"op":"material","id":"A","roughness":0.4}]),
                None,
                false,
            )
            .unwrap();
        }
        let before = ed.scene().clone();
        let error = apply(
            &mut ed,
            json!([{"op":"material","id":"B","color":"#ffffff"}]),
            Some(1),
            true,
        )
        .unwrap_err();
        assert!(error.stale && error.message.contains("snapshot is no longer available"));
        assert_eq!(*ed.scene(), before);
    }

    #[test]
    fn different_static_properties_on_one_object_rebase_in_both_orders() {
        let edits = [
            json!({"op":"transform","id":"A","translation":[1,2,3]}),
            json!({"op":"transform","id":"A","rotation":[0,0.2,0]}),
            json!({"op":"transform","id":"A","scale":[2,2,2]}),
            json!({"op":"material","id":"A","color":"#ff8800"}),
            json!({"op":"material","id":"A","roughness":0.15}),
            json!({"op":"material","id":"A","metalness":0.85}),
            json!({"op":"material","id":"A","emissive":"#ff8800"}),
            json!({"op":"material","id":"A","emissive_strength":3}),
            json!({"op":"material","id":"A","opacity":0.4}),
            json!({"op":"material","id":"A","transmission":0.7}),
            json!({"op":"material","id":"A","texture":{"pattern":"checker"}}),
        ];
        for (first_index, first) in edits.iter().enumerate() {
            for (second_index, second) in edits.iter().enumerate() {
                if first_index == second_index {
                    continue;
                }
                let mut editor = pair();
                apply(&mut editor, json!([first]), None, false).unwrap();
                let before = editor.scene().clone();
                let result = apply(&mut editor, json!([second]), Some(1), true).unwrap();
                assert_eq!(result.rebased_from, Some(1));
                let mut sequential = pair();
                apply(&mut sequential, json!([first, second]), None, false).unwrap();
                assert_eq!(
                    editor.scene().objects,
                    sequential.scene().objects,
                    "{first} then {second}"
                );
                assert_eq!(editor.undo(), Some(4));
                assert_eq!(editor.scene().objects, before.objects);
                assert_eq!(editor.history().0.len(), 2);
            }
        }
    }

    #[test]
    fn same_fields_presets_and_no_op_overwrites_remain_atomic_conflicts() {
        for requested in [
            json!({"op":"material","id":"A","color":"#ffffff"}),
            json!({"op":"material","id":"A","preset":"gold"}),
            json!({"op":"material","id":"A","color":engine::Material::default().color,"roughness":0.2}),
            json!({"op":"material","id":"A"}),
        ] {
            let mut editor = pair();
            apply(
                &mut editor,
                json!([{"op":"material","id":"A","color":"#ff8800"}]),
                None,
                false,
            )
            .unwrap();
            let before = editor.scene().clone();
            let recipe = editor.project();
            let error = apply(
                &mut editor,
                json!([{"op":"material","id":"B","roughness":0.3},requested]),
                Some(1),
                true,
            )
            .unwrap_err();
            assert!(error.stale && error.message.contains("A"));
            assert_eq!(*editor.scene(), before);
            assert_eq!(editor.project(), recipe);
        }
    }

    #[test]
    fn movement_and_a_material_preset_keep_both_edits_and_one_undo() {
        let mut editor = pair();
        apply(
            &mut editor,
            json!([{"op":"move","id":"A","offset":[2,0,0]}]),
            None,
            false,
        )
        .unwrap();
        let first = editor.scene().objects.clone();
        apply(
            &mut editor,
            json!([{"op":"material","id":"A","preset":"gold"}]),
            Some(1),
            true,
        )
        .unwrap();
        assert_eq!(
            editor.scene().objects[0].transform.translation,
            [2., 0., 0.]
        );
        assert_eq!(editor.scene().objects[0].material.metalness, 1.);
        editor.undo().unwrap();
        assert_eq!(editor.scene().objects, first);
        let mut editor = pair();
        apply(
            &mut editor,
            json!([{"op":"material","id":"A","color":"#ff8800"}]),
            None,
            false,
        )
        .unwrap();
        apply(
            &mut editor,
            json!([{"op":"move","id":"A","offset":[2,0,0]}]),
            Some(1),
            true,
        )
        .unwrap();
        assert_eq!(editor.scene().objects[0].material.color, "#ff8800");
        assert_eq!(
            editor.scene().objects[0].transform.translation,
            [2., 0., 0.]
        );
    }

    #[test]
    fn structural_and_unsupported_edits_keep_whole_object_conflict_detection() {
        for remote in [
            json!({"op":"subdivide","id":"A","levels":1}),
            json!({"op":"add_modifier","id":"A","modifier":{"type":"twist","angle":0.2}}),
            json!({"op":"rename","id":"A","name":"Renamed"}),
            json!({"op":"shade","id":"A","smooth":true}),
        ] {
            let mut editor = pair();
            apply(&mut editor, json!([remote]), None, false).unwrap();
            let before = editor.scene().clone();
            let error = apply(
                &mut editor,
                json!([{"op":"material","id":1,"color":"#ff8800"}]),
                Some(1),
                true,
            )
            .unwrap_err();
            assert!(error.stale);
            assert_eq!(*editor.scene(), before);
        }
        let mut editor = pair();
        apply(
            &mut editor,
            json!([{"op":"material","id":"A","color":"#ff8800"}]),
            None,
            false,
        )
        .unwrap();
        let before = editor.scene().clone();
        assert!(
            apply(
                &mut editor,
                json!([{"op":"move","id":"A","rotate_y":0.2}]),
                Some(1),
                true
            )
            .unwrap_err()
            .stale
        );
        assert_eq!(*editor.scene(), before);
    }

    #[test]
    fn rig_targets_poles_and_animation_keep_conservative_dependencies() {
        let mut editor = pair();
        apply(
            &mut editor,
            json!([
                {"op":"rig","id":"A","chain":2},
                {"op":"add","name":"Pole","primitive":{"kind":"cube"},"translation":[0,0,2]},
                {"op":"track_target","id":"A","bone":"Bone 2","target":"B","pole":"Pole","chain":2}
            ]),
            None,
            false,
        )
        .unwrap();
        for name in ["A", "B", "Pole"] {
            let scene = editor.scene().clone();
            let mut scoped = Editor::new();
            scoped.load(scene).unwrap();
            apply(
                &mut scoped,
                json!([{"op":"material","id":name,"color":"#ff8800"}]),
                None,
                false,
            )
            .unwrap();
            let before = scoped.scene().clone();
            assert!(
                apply(
                    &mut scoped,
                    json!([{"op":"move","id":name,"offset":[0.1,0,0]}]),
                    Some(1),
                    true
                )
                .unwrap_err()
                .stale
            );
            assert_eq!(*scoped.scene(), before);
        }
        let mut animated = pair();
        apply(&mut animated, json!([{"op":"set_keyframe","id":"A","property":"translation","frame":1,"value":[0,0,0]}]), None, false).unwrap();
        apply(
            &mut animated,
            json!([{"op":"move","id":"A","offset":[1,0,0]}]),
            None,
            false,
        )
        .unwrap();
        let before = animated.scene().clone();
        assert!(
            apply(
                &mut animated,
                json!([{"op":"material","id":"A","color":"#ff8800"}]),
                Some(2),
                true
            )
            .unwrap_err()
            .stale
        );
        assert_eq!(*animated.scene(), before);
        let mut linking = pair();
        apply(
            &mut linking,
            json!([{"op":"rig","id":"A","chain":2}]),
            None,
            false,
        )
        .unwrap();
        apply(
            &mut linking,
            json!([{"op":"track_target","id":"A","bone":"Bone 2","target":"B"}]),
            None,
            false,
        )
        .unwrap();
        let error = apply(
            &mut linking,
            json!([{"op":"material","id":"B","color":"#ff8800"}]),
            Some(2),
            true,
        )
        .unwrap_err();
        assert!(error.stale && error.message.contains("relation rules"));
    }

    #[test]
    fn every_claimed_static_field_rejects_a_concurrent_write() {
        for edit in [
            json!({"op":"transform","id":"A","translation":[1,0,0]}),
            json!({"op":"transform","id":"A","rotation":[0,0.2,0]}),
            json!({"op":"transform","id":"A","scale":[2,2,2]}),
            json!({"op":"material","id":"A","color":"#ff8800"}),
            json!({"op":"material","id":"A","roughness":0.15}),
            json!({"op":"material","id":"A","metalness":0.85}),
            json!({"op":"material","id":"A","emissive":"#ff8800"}),
            json!({"op":"material","id":"A","emissive_strength":3}),
            json!({"op":"material","id":"A","opacity":0.4}),
            json!({"op":"material","id":"A","transmission":0.7}),
            json!({"op":"material","id":"A","texture":{"pattern":"checker"}}),
        ] {
            let mut editor = pair();
            apply(&mut editor, json!([edit]), None, false).unwrap();
            let before = editor.scene().clone();
            assert!(
                apply(&mut editor, json!([edit]), Some(1), true)
                    .unwrap_err()
                    .stale
            );
            assert_eq!(*editor.scene(), before);
        }
    }

    #[test]
    fn grouped_parts_do_not_relax_whole_object_checks() {
        let mut editor = pair();
        let mut scene = editor.scene().clone();
        scene.objects[0].group = Some("Assembly".into());
        editor.load(scene).unwrap();
        let revision = editor.scene().revision;
        apply(
            &mut editor,
            json!([{"op":"material","id":"A","color":"#ff8800"}]),
            None,
            false,
        )
        .unwrap();
        let before = editor.scene().clone();
        assert!(
            apply(
                &mut editor,
                json!([{"op":"move","id":"A","offset":[1,0,0]}]),
                Some(revision),
                true
            )
            .unwrap_err()
            .stale
        );
        assert_eq!(*editor.scene(), before);
    }
}
