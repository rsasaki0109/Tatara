//! Ephemeral, advisory presence for a local editor session. Identities are
//! display metadata, not authentication; no scene edits or undo entries here.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine::EngineError;

pub const TTL_MS: u64 = 30_000;
pub const MAX_PEERS: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    pub id: String,
    pub name: String,
}

impl Actor {
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.id.is_empty() || self.id.len() > 128 || self.id.chars().any(char::is_control) {
            return Err(EngineError::new(
                "actor id must be 1-128 bytes without control characters",
            ));
        }
        if self.name.trim().is_empty()
            || self.name.len() > 64
            || self.name.chars().any(char::is_control)
        {
            return Err(EngineError::new(
                "actor name must be 1-64 bytes without control characters",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    pub eye: [f64; 3],
    pub target: [f64; 3],
    pub fov: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ortho_height: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub up: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Presence {
    pub actor: Actor,
    /// Cursor in normalized viewport coordinates (0..1), or outside it.
    #[serde(default)]
    pub cursor: Option<[f64; 2]>,
    #[serde(default)]
    pub selection: Vec<u64>,
    #[serde(default)]
    pub camera: Option<Camera>,
    /// Advisory only: edits are still protected by expected_revision.
    #[serde(default)]
    pub editing: Vec<u64>,
}

impl Presence {
    fn validate(&self) -> Result<(), EngineError> {
        self.actor.validate()?;
        if self.selection.len() > 32 || self.editing.len() > 32 {
            return Err(EngineError::new("presence lists are limited to 32 objects"));
        }
        if self.cursor.is_some_and(|p| {
            p.into_iter()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(&v))
        }) {
            return Err(EngineError::new(
                "cursor must be within the viewport (0..1)",
            ));
        }
        if let Some(c) = &self.camera
            && (c
                .eye
                .iter()
                .chain(&c.target)
                .any(|v| !v.is_finite() || v.abs() > 1e6)
                || !c.fov.is_finite()
                || !(1.0..=179.0).contains(&c.fov)
                || c.eye == c.target
                || c.ortho_height
                    .is_some_and(|h| !h.is_finite() || !(0.001..=1e4).contains(&h))
                || c.aspect
                    .is_some_and(|a| !a.is_finite() || !(0.01..=100.0).contains(&a))
                || c.up.is_some_and(|u| {
                    let up = glam::DVec3::from(u);
                    let forward = glam::DVec3::from(c.target) - glam::DVec3::from(c.eye);
                    !up.is_finite()
                        || up.length() < 1e-9
                        || forward
                            .normalize_or_zero()
                            .cross(up.normalize_or_zero())
                            .length()
                            < 1e-9
                }))
        {
            return Err(EngineError::new(
                "camera needs finite, distinct eye/target positions and fov 1-179",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct Room {
    peers: BTreeMap<String, (Presence, u64)>,
    pub version: u64,
}

impl Room {
    pub fn expire(&mut self, now: u64) {
        let before = self.peers.len();
        self.peers
            .retain(|_, (_, seen)| now.saturating_sub(*seen) < TTL_MS);
        if before != self.peers.len() {
            self.version += 1;
        }
    }

    pub fn update(&mut self, presence: Presence, now: u64) -> Result<(), EngineError> {
        presence.validate()?;
        self.expire(now);
        if !self.peers.contains_key(&presence.actor.id) && self.peers.len() >= MAX_PEERS {
            return Err(EngineError::new("this session already has 64 participants"));
        }
        if self
            .peers
            .get(&presence.actor.id)
            .is_none_or(|(old, _)| *old != presence)
        {
            self.version += 1;
        }
        self.peers
            .insert(presence.actor.id.clone(), (presence, now));
        Ok(())
    }

    pub fn leave(&mut self, id: &str) {
        if self.peers.remove(id).is_some() {
            self.version += 1;
        }
    }

    pub fn peers(&self) -> Vec<&Presence> {
        self.peers.values().map(|(p, _)| p).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn peer(id: &str) -> Presence {
        Presence {
            actor: Actor {
                id: id.into(),
                name: "Alice".into(),
            },
            cursor: Some([0.2, 0.4]),
            selection: vec![1],
            camera: None,
            editing: vec![1],
        }
    }

    #[test]
    fn heartbeats_leave_and_expiry_do_not_hold_stale_locks() {
        let mut r = Room::default();
        r.update(peer("a"), 0).unwrap();
        let version = r.version;
        r.update(peer("a"), TTL_MS - 1).unwrap();
        assert_eq!(
            r.version, version,
            "heartbeats do not broadcast unchanged state"
        );
        r.update(peer("b"), 0).unwrap();
        r.expire(TTL_MS);
        assert_eq!(r.peers().len(), 1);
        assert_eq!(r.peers()[0].actor.id, "a");
        r.leave("a");
        assert!(r.peers().is_empty());
        r.leave("a");
        r.update(peer("a"), 2 * TTL_MS).unwrap();
        r.expire(3 * TTL_MS);
        assert!(r.peers().is_empty());
    }

    #[test]
    fn invalid_updates_are_atomic_and_the_room_is_bounded() {
        let mut r = Room::default();
        r.update(peer("a"), 0).unwrap();
        let mut bad = peer("a");
        bad.cursor = Some([f64::NAN, 0.5]);
        assert!(r.update(bad, 1).is_err());
        assert_eq!(r.peers()[0].cursor, Some([0.2, 0.4]));
        for i in 1..MAX_PEERS {
            r.update(peer(&i.to_string()), 1).unwrap();
        }
        assert!(r.update(peer("overflow"), 1).is_err());
        assert_eq!(r.peers().len(), MAX_PEERS);
        let mut bad = peer("a");
        bad.camera = Some(Camera {
            eye: [0.0; 3],
            target: [0.0; 3],
            fov: 36.0,
            ortho_height: None,
            up: None,
            aspect: None,
        });
        assert!(r.update(bad, 1).is_err());
    }

    #[test]
    fn presence_retains_orthographic_projection_and_roll_and_rejects_invalid_metadata() {
        let mut p = peer("ortho");
        p.camera = Some(Camera {
            eye: [0., 1., 4.],
            target: [0., 1., 0.],
            fov: 36.,
            ortho_height: Some(3.),
            up: Some([1., 0., 0.]),
            aspect: Some(2.),
        });
        assert!(p.validate().is_ok());
        let encoded = serde_json::to_value(&p).unwrap();
        assert_eq!(encoded["camera"]["ortho_height"], 3.);
        assert_eq!(serde_json::from_value::<Presence>(encoded).unwrap(), p);
        let mut bad = p.clone();
        bad.camera.as_mut().unwrap().ortho_height = Some(0.);
        assert!(bad.validate().is_err());
        bad = p.clone();
        bad.camera.as_mut().unwrap().up = Some([0., 0., 1.]);
        assert!(bad.validate().is_err());
        bad = p;
        bad.camera.as_mut().unwrap().aspect = Some(f64::INFINITY);
        assert!(bad.validate().is_err());
    }
}
