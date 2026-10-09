//! Conservative optimistic collaboration. Reapply a stale edit only when every
//! object it reads (including group members and connected constraints) is still
//! identical to the retained context. Other edits remain in the current scene.

use std::collections::HashSet;

use crate::constraint::{self, Who};
use crate::engine::{self, Command, EngineError, ObjRef, Scene};
use crate::proposal;

fn target(command: &Command) -> Result<&ObjRef, EngineError> {
    match command {
        Command::CameraSettings { id, .. }
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

pub(crate) fn validate(
    base: &Scene,
    current: &Scene,
    commands: &[Command],
) -> Result<(), EngineError> {
    if commands.is_empty() {
        return Err(EngineError::new("batch has no commands"));
    }
    for command in commands {
        target(command)?;
    }
    if base.world != current.world
        || base.images != current.images
        || base.animation != current.animation
        || base.constraints != current.constraints
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
        if old.is_none() || old != now {
            let name = old.map(|o| o.name.as_str()).unwrap_or("unknown object");
            return Err(EngineError::new(format!(
                "object {name:?} (#{id}) was changed or removed by another edit"
            )));
        }
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
    fn unsupported_creation_global_changes_loaded_bases_and_unavailable_contexts_are_refused() {
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
                json!([{"op":"add","primitive":{"kind":"cube"}}]),
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
}
