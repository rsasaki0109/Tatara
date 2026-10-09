//! Proposals: changes offered for review instead of applied. An agent (or
//! a person) proposes a command batch, or several alternative batches
//! (variants); the editor keeps each one applied to a copy of the current
//! scene, so it can be previewed exactly, and lists what it would add,
//! change and remove. Accepting applies the batch as one undo step (and
//! drops its sibling variants); rejecting drops it. Proposals follow the
//! scene: after every edit they are re-applied to it, and one that no
//! longer applies shows why.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::engine::{Command, Object, Scene};

/// Most proposals waiting at once.
pub const MAX_PENDING: usize = 12;
/// Most variants in one request.
pub const MAX_VARIANTS: usize = 6;
/// Decisions remembered for agents to read back.
pub const MAX_DECIDED: usize = 32;

/// A request to propose changes: one batch, or alternatives to choose from.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ProposalRequest {
    /// What the change does, in a few words.
    pub title: String,
    /// Why, or what to look at (optional).
    #[serde(default)]
    pub note: Option<String>,
    /// Who proposes it (e.g. the agent's name).
    #[serde(default)]
    pub author: Option<String>,
    /// The commands, for a single proposal.
    #[serde(default)]
    pub commands: Vec<Command>,
    /// Alternatives to choose between (accepting one drops the others).
    #[serde(default)]
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct Variant {
    pub title: String,
    #[serde(default)]
    pub note: Option<String>,
    pub commands: Vec<Command>,
}

/// A pending proposal.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub id: u64,
    pub title: String,
    pub note: Option<String>,
    pub author: String,
    /// Variants of one request share a group (the first variant's id).
    pub group: Option<u64>,
    pub commands: Vec<Command>,
    /// The current scene with the commands applied.
    pub scene: Scene,
    pub diff: Diff,
    /// Why the commands no longer apply to the current scene.
    pub conflict: Option<String>,
}

/// What a proposal changes, object by object.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Diff {
    pub added: Vec<Named>,
    pub removed: Vec<Named>,
    pub changed: Vec<Changed>,
    /// Scene-wide settings it changes: world, animation, images.
    pub scene: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Named {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Changed {
    pub id: u64,
    pub name: String,
    /// Which aspects: name, transform, material, mesh, modifiers,
    /// animation, rig, shading, group.
    pub parts: Vec<&'static str>,
}

/// How a proposal ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Accepted,
    Rejected,
    /// Another variant of its group was accepted.
    Superseded,
}

#[derive(Debug, Clone, Serialize)]
pub struct Decided {
    pub id: u64,
    pub title: String,
    pub author: String,
    pub outcome: Outcome,
    /// The scene revision when it was decided.
    pub revision: u64,
}

impl Proposal {
    /// The proposal as listed: everything but the scene and commands.
    pub fn summary(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "title": self.title,
            "note": self.note,
            "author": self.author,
            "group": self.group,
            "commands": self.commands.len(),
            "diff": self.diff,
            "conflict": self.conflict,
        })
    }
}

/// Compare two scenes object by object (matched by id).
pub fn diff(before: &Scene, after: &Scene) -> Diff {
    let mut d = Diff::default();
    let named = |o: &Object| Named {
        id: o.id,
        name: o.name.clone(),
    };
    for o in &after.objects {
        match before.objects.iter().find(|b| b.id == o.id) {
            None => d.added.push(named(o)),
            Some(b) => {
                let parts = changed_parts(b, o);
                if !parts.is_empty() {
                    d.changed.push(Changed {
                        id: o.id,
                        name: o.name.clone(),
                        parts,
                    });
                }
            }
        }
    }
    for b in &before.objects {
        if !after.objects.iter().any(|o| o.id == b.id) {
            d.removed.push(named(b));
        }
    }
    if before.world != after.world {
        d.scene.push("world");
    }
    if before.animation != after.animation {
        d.scene.push("animation");
    }
    // Image bytes are shared between snapshots, so this is cheap.
    if before.images != after.images {
        d.scene.push("images");
    }
    if before.constraints != after.constraints {
        d.scene.push("constraints");
    }
    if before.arrangements != after.arrangements {
        d.scene.push("arrangements");
    }
    d
}

fn changed_parts(a: &Object, b: &Object) -> Vec<&'static str> {
    let json = |v: &dyn erased::Json| v.value();
    let mut parts = Vec::new();
    if a.name != b.name {
        parts.push("name");
    }
    if a.transform != b.transform {
        parts.push("transform");
    }
    if a.material != b.material {
        parts.push("material");
    }
    if a.mesh != b.mesh {
        parts.push("mesh");
    }
    if json(&a.modifiers) != json(&b.modifiers) {
        parts.push("modifiers");
    }
    if json(&a.tracks) != json(&b.tracks) {
        parts.push("animation");
    }
    if json(&a.bones) != json(&b.bones) {
        parts.push("rig");
    }
    if a.smooth != b.smooth {
        parts.push("shading");
    }
    if a.group != b.group {
        parts.push("group");
    }
    parts
}

/// Compare serializable values without requiring `PartialEq` on them.
mod erased {
    pub trait Json {
        fn value(&self) -> serde_json::Value;
    }
    impl<T: serde::Serialize> Json for T {
        fn value(&self) -> serde_json::Value {
            serde_json::to_value(self).unwrap_or_default()
        }
    }
}

/// Check a request's text before anything is applied.
pub fn check_request(req: &ProposalRequest) -> Result<(), String> {
    let text = |label: &str, s: &str, max: usize| {
        if s.trim().is_empty() || s.chars().count() > max {
            Err(format!("{label} must be 1-{max} characters"))
        } else {
            Ok(())
        }
    };
    text("title", &req.title, 80)?;
    if let Some(note) = &req.note {
        text("note", note, 500)?;
    }
    if let Some(author) = &req.author {
        text("author", author, 40)?;
    }
    match (req.commands.is_empty(), req.variants.is_empty()) {
        (true, true) => return Err("propose commands or variants".into()),
        (false, false) => return Err("give commands or variants, not both".into()),
        _ => {}
    }
    if req.variants.len() > MAX_VARIANTS {
        return Err(format!("at most {MAX_VARIANTS} variants"));
    }
    for v in &req.variants {
        text("variant title", &v.title, 80)?;
        if let Some(note) = &v.note {
            text("variant note", note, 500)?;
        }
        if v.commands.is_empty() {
            return Err(format!("variant {:?} has no commands", v.title));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CommandBatch, Editor};
    use serde_json::json;

    fn apply(ed: &mut Editor, commands: Value) {
        let batch: CommandBatch = serde_json::from_value(json!({ "commands": commands })).unwrap();
        ed.apply(&batch).unwrap();
    }

    fn request(v: Value) -> ProposalRequest {
        serde_json::from_value(v).unwrap()
    }

    fn names(ed: &Editor) -> Vec<String> {
        ed.scene().objects.iter().map(|o| o.name.clone()).collect()
    }

    fn table() -> Editor {
        let mut ed = Editor::new();
        apply(
            &mut ed,
            json!([
                {"op": "add", "name": "Vase", "primitive": {"kind": "cylinder"}, "translation": [0, 0.5, 0]},
                {"op": "add", "name": "Cup", "primitive": {"kind": "cube"}, "translation": [1, 0.5, 0]}
            ]),
        );
        ed
    }

    #[test]
    fn a_proposal_changes_nothing_until_accepted() {
        let mut ed = table();
        let revision = ed.scene().revision;
        let ids = ed
            .propose(&request(json!({
                "title": "Glaze and a lamp",
                "author": "claude",
                "commands": [
                    {"op": "material", "id": "Vase", "color": "#2f4f8f"},
                    {"op": "add", "name": "Lamp", "primitive": {"kind": "sphere"}, "translation": [-1, 0.5, 0]},
                    {"op": "delete", "id": "Cup"}
                ]
            })))
            .unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ed.scene().revision, revision, "nothing applied yet");
        assert_eq!(names(&ed), ["Vase", "Cup"]);
        let p = &ed.proposals()[0];
        assert_eq!((p.author.as_str(), p.group), ("claude", None));
        assert_eq!(
            p.diff
                .added
                .iter()
                .map(|n| n.name.as_str())
                .collect::<Vec<_>>(),
            ["Lamp"]
        );
        assert_eq!(
            p.diff
                .removed
                .iter()
                .map(|n| n.name.as_str())
                .collect::<Vec<_>>(),
            ["Cup"]
        );
        assert_eq!(p.diff.changed.len(), 1);
        assert_eq!(
            (p.diff.changed[0].name.as_str(), &p.diff.changed[0].parts),
            ("Vase", &vec!["material"])
        );
        // The preview is the scene with the change applied.
        let preview = ed.preview(ids[0]).unwrap();
        assert_eq!(names(&preview), ["Vase", "Lamp"]);

        ed.accept(ids[0]).unwrap();
        assert_eq!(names(&ed), ["Vase", "Lamp"]);
        assert_eq!(ed.scene().revision, revision + 1, "one undo step");
        assert!(ed.proposals().is_empty());
        assert_eq!(ed.decided()[0].outcome, Outcome::Accepted);
        ed.undo();
        assert_eq!(names(&ed), ["Vase", "Cup"]);
    }

    #[test]
    fn variants_are_alternatives() {
        let mut ed = table();
        let colours = ["#8fb9a0", "#b5643c", "#2f4f8f"];
        let variants: Vec<Value> = colours
            .iter()
            .map(
                |c| json!({"title": c, "commands": [{"op": "material", "id": "Vase", "color": c}]}),
            )
            .collect();
        let ids = ed
            .propose(&request(json!({"title": "Glazes", "variants": variants})))
            .unwrap();
        assert_eq!(ids.len(), 3);
        assert!(ed.proposals().iter().all(|p| p.group == Some(ids[0])));
        assert_eq!(ed.proposals()[1].title, "Glazes · #b5643c");
        // Another, unrelated proposal stays when a variant is chosen.
        let other = ed
            .propose(&request(json!({"title": "Move the cup", "commands": [{"op": "transform", "id": "Cup", "translation": [2, 0.5, 0]}]})))
            .unwrap()[0];
        ed.accept(ids[1]).unwrap();
        assert_eq!(ed.scene().objects[0].material.color, "#b5643c");
        assert_eq!(
            ed.proposals().iter().map(|p| p.id).collect::<Vec<_>>(),
            [other]
        );
        let outcomes: Vec<(u64, Outcome)> =
            ed.decided().iter().map(|d| (d.id, d.outcome)).collect();
        assert_eq!(
            outcomes,
            [
                (ids[1], Outcome::Accepted),
                (ids[0], Outcome::Superseded),
                (ids[2], Outcome::Superseded)
            ]
        );
        ed.reject(other).unwrap();
        assert_eq!(ed.decided().last().unwrap().outcome, Outcome::Rejected);
        assert_eq!(ed.scene().objects[1].transform.translation, [1.0, 0.5, 0.0]);
    }

    #[test]
    fn proposals_follow_the_scene_and_report_conflicts() {
        let mut ed = table();
        let id = ed
            .propose(&request(json!({"title": "Blue vase", "commands": [{"op": "material", "id": "Vase", "color": "#2f4f8f"}]})))
            .unwrap()[0];
        // An edit elsewhere: the proposal is re-applied on top of it.
        apply(
            &mut ed,
            json!([{"op": "add", "name": "Plate", "primitive": {"kind": "cylinder"}}]),
        );
        let p = &ed.proposals()[0];
        assert_eq!(p.diff.changed.len(), 1);
        assert!(
            p.diff.added.is_empty(),
            "the plate is not part of the proposal"
        );
        assert!(
            ed.preview(id)
                .unwrap()
                .scene()
                .objects
                .iter()
                .any(|o| o.name == "Plate")
        );
        // The vase goes: the proposal no longer applies.
        apply(&mut ed, json!([{"op": "delete", "id": "Vase"}]));
        assert!(
            ed.proposals()[0]
                .conflict
                .as_deref()
                .unwrap()
                .contains("Vase")
        );
        assert!(ed.accept(id).is_err());
        // Undo brings the vase back, and the proposal applies again.
        ed.undo();
        assert!(ed.proposals()[0].conflict.is_none());
        ed.accept(id).unwrap();
        assert_eq!(ed.scene().objects[0].material.color, "#2f4f8f");
    }

    #[test]
    fn bad_proposals_are_refused_whole() {
        let mut ed = table();
        for bad in [
            json!({"title": "", "commands": [{"op": "delete", "id": "Cup"}]}),
            json!({"title": "Nothing"}),
            json!({"title": "Missing", "commands": [{"op": "delete", "id": "Nope"}]}),
            json!({"title": "Both", "commands": [{"op": "delete", "id": "Cup"}], "variants": [{"title": "a", "commands": [{"op": "delete", "id": "Cup"}]}]}),
            json!({"title": "One bad variant", "variants": [
                {"title": "fine", "commands": [{"op": "delete", "id": "Cup"}]},
                {"title": "broken", "commands": [{"op": "delete", "id": "Nope"}]}
            ]}),
        ] {
            assert!(ed.propose(&request(bad.clone())).is_err(), "{bad}");
        }
        assert!(ed.proposals().is_empty());
        for i in 0..MAX_PENDING {
            ed.propose(&request(
                json!({"title": format!("p{i}"), "commands": [{"op": "delete", "id": "Cup"}]}),
            ))
            .unwrap();
        }
        assert!(
            ed.propose(&request(
                json!({"title": "one too many", "commands": [{"op": "delete", "id": "Cup"}]})
            ))
            .is_err()
        );
        assert!(ed.reject(999).is_err() && ed.accept(999).is_err());
    }
}
