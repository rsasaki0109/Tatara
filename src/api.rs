//! Transport-independent editor API. The native HTTP server and the
//! browser-only WebAssembly build both route `/api/...` requests through
//! [`handle`], so the two behave identically.

use serde_json::{Value, json};

use crate::engine::{self, CommandBatch, Editor, EngineError, Scene};

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// Set when the scene revision changed (for live-update notifications).
    pub changed: Option<u64>,
    /// Extra headers, such as a download file name.
    pub disposition: Option<&'static str>,
}

impl Response {
    fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: serde_json::to_vec(&value).expect("json serializes"),
            changed: None,
            disposition: None,
        }
    }

    fn error(status: u16, message: impl Into<String>) -> Self {
        Self::json(status, json!({ "error": message.into() }))
    }

    fn changed(mut self, revision: u64) -> Self {
        self.changed = Some(revision);
        self
    }
}

impl From<EngineError> for Response {
    fn from(e: EngineError) -> Self {
        let status = if e.stale { 409 } else { 422 };
        Response::json(status, json!({ "error": e.to_string(), "detail": e }))
    }
}

fn history(ed: &Editor) -> Value {
    json!({ "can_undo": ed.can_undo(), "can_redo": ed.can_redo() })
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Response> {
    serde_json::from_slice(body).map_err(|e| Response::error(400, format!("invalid request: {e}")))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(k) == key).then(|| percent_decode(v))
    })
}

/// The scene as the viewport needs it: objects with modifiers also carry
/// their evaluated `display` mesh.
pub fn state(ed: &Editor, ai: bool) -> Value {
    let mut scene = serde_json::to_value(ed.scene()).expect("scene serializes");
    if let Some(objects) = scene["objects"].as_array_mut() {
        for (value, o) in objects.iter_mut().zip(&ed.scene().objects) {
            if !o.modifiers.is_empty() {
                value["display"] = json!(ed.evaluated(o));
            }
        }
    }
    json!({ "scene": scene, "history": history(ed), "ai": ai })
}

fn render(ed: &Editor, query: &str) -> Result<Response, Response> {
    let views =
        crate::render::parse_views(&query_param(query, "views").unwrap_or_else(|| "iso".into()))?;
    let size = match query_param(query, "size") {
        Some(s) => s
            .parse()
            .map_err(|_| Response::error(400, "size must be a number"))?,
        None => 512,
    };
    let focus = match query_param(query, "object").filter(|s| !s.is_empty()) {
        None => None,
        Some(r) => {
            let found = r
                .parse::<u64>()
                .ok()
                .and_then(|id| ed.scene().objects.iter().find(|o| o.id == id))
                .or_else(|| ed.scene().objects.iter().find(|o| o.name == r));
            Some(
                found
                    .map(|o| o.id)
                    .ok_or_else(|| Response::error(404, format!("no object {r:?}")))?,
            )
        }
    };
    let frame = match query_param(query, "frame").filter(|s| !s.is_empty()) {
        Some(f) => {
            Some(crate::anim::check_frame(f.parse().map_err(|_| {
                Response::error(400, "frame must be a number")
            })?)?)
        }
        None => None,
    };
    let png = crate::render::render_png(
        ed,
        &crate::render::RenderOptions {
            views,
            size,
            focus,
            frame,
        },
    )?;
    Ok(Response {
        status: 200,
        content_type: "image/png",
        body: png,
        changed: None,
        disposition: None,
    })
}

/// Route one request. `path` may include a query string; it is relative to
/// `/api` (e.g. `/commands`). `ai` reports whether chat is configured.
pub fn handle(ed: &mut Editor, method: &str, path: &str, body: &[u8], ai: bool) -> Response {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let path = path.trim_end_matches('/');
    let result: Result<Response, Response> = (|| match (method, path) {
        ("GET", "/state") => Ok(Response::json(200, state(ed, ai))),
        ("GET", "/scene") => Ok(Response::json(200, json!(ed.scene()))),
        ("GET", "/context") => {
            let frame = match query_param(query, "frame").filter(|s| !s.is_empty()) {
                Some(f) => {
                    Some(crate::anim::check_frame(f.parse().map_err(|_| {
                        Response::error(400, "frame must be a number")
                    })?)?)
                }
                None => None,
            };
            Ok(Response::json(200, engine::context_at(ed, frame)))
        }
        ("GET", "/schema") => Ok(Response::json(200, engine::command_schema())),
        ("GET", "/inspect") => Ok(Response::json(200, crate::inspect::inspect(ed))),
        ("GET", "/ai") => Ok(Response::json(200, json!({ "enabled": ai }))),
        ("POST", "/commands") => {
            let batch: CommandBatch = parse(body)?;
            let r = ed.apply(&batch)?;
            Ok(Response::json(
                200,
                json!({ "revision": r.revision, "created": r.created, "history": history(ed) }),
            )
            .changed(r.revision))
        }
        ("PUT", "/scene") => {
            let scene: Scene = parse(body)?;
            let revision = ed.load(scene)?;
            Ok(
                Response::json(200, json!({ "revision": revision, "history": history(ed) }))
                    .changed(revision),
            )
        }
        ("POST", "/undo") | ("POST", "/redo") => {
            let rev = if path == "/undo" {
                ed.undo()
            } else {
                ed.redo()
            };
            let revision =
                rev.ok_or_else(|| Response::error(409, format!("nothing to {}", &path[1..])))?;
            Ok(
                Response::json(200, json!({ "revision": revision, "history": history(ed) }))
                    .changed(revision),
            )
        }
        ("POST", "/reset") => {
            ed.reset();
            let revision = ed.scene().revision;
            Ok(
                Response::json(200, json!({ "revision": revision, "history": history(ed) }))
                    .changed(revision),
            )
        }
        ("GET", "/export/obj") => Ok(Response {
            status: 200,
            content_type: "text/plain; charset=utf-8",
            body: engine::export_obj(ed).into_bytes(),
            changed: None,
            disposition: Some("attachment; filename=\"scene.obj\""),
        }),
        ("GET", "/export/glb") => Ok(Response {
            status: 200,
            content_type: "model/gltf-binary",
            body: crate::gltf::export_glb(ed),
            changed: None,
            disposition: Some("attachment; filename=\"scene.glb\""),
        }),
        ("POST", "/import") => {
            let commands = crate::gltf::import(body)?;
            let r = ed.apply(&CommandBatch {
                commands,
                expected_revision: None,
            })?;
            Ok(Response::json(
                200,
                json!({ "revision": r.revision, "created": r.created, "history": history(ed) }),
            )
            .changed(r.revision))
        }
        ("GET", "/render") => render(ed, query),
        _ => Err(Response::error(
            404,
            format!("no route for {method} /api{path}"),
        )),
    })();
    result.unwrap_or_else(|e| e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(ed: &mut Editor, method: &str, path: &str, body: Value) -> (u16, Value) {
        let body = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&body).unwrap()
        };
        let r = handle(ed, method, path, &body, false);
        let v = if r.content_type == "application/json" {
            serde_json::from_slice(&r.body).unwrap()
        } else {
            Value::Null
        };
        (r.status, v)
    }

    #[test]
    fn routes_mirror_the_http_api() {
        let mut ed = Editor::new();
        let (s, r) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands": [{"op": "add", "name": "Box", "primitive": {"kind": "cube"}}]}),
        );
        assert_eq!((s, &r["created"]), (200, &json!([1])));
        assert_eq!(
            call(
                &mut ed,
                "POST",
                "/commands",
                json!({"commands": [{"op": "delete", "id": 9}]})
            )
            .0,
            422
        );
        assert_eq!(
            call(
                &mut ed,
                "POST",
                "/commands",
                json!({"commands": [{"op": "clear"}], "expected_revision": 0})
            )
            .0,
            409
        );
        assert_eq!(call(&mut ed, "POST", "/commands", json!("nope")).0, 400);
        let (_, st) = call(&mut ed, "GET", "/state", Value::Null);
        assert_eq!(st["scene"]["objects"][0]["name"], "Box");
        assert_eq!(call(&mut ed, "POST", "/undo", Value::Null).0, 200);
        assert_eq!(call(&mut ed, "POST", "/undo", Value::Null).0, 409);
        assert_eq!(call(&mut ed, "POST", "/redo", Value::Null).0, 200);
        let png = handle(
            &mut ed,
            "GET",
            "/render?views=front%2Ctop&size=64&object=Box",
            &[],
            false,
        );
        assert_eq!((png.status, png.content_type), (200, "image/png"));
        assert_eq!(
            handle(&mut ed, "GET", "/render?object=Nope", &[], false).status,
            404
        );
        let glb = handle(&mut ed, "GET", "/export/glb", &[], false);
        let imported = handle(&mut ed, "POST", "/import", &glb.body, false);
        assert_eq!(imported.status, 200);
        assert_eq!(imported.changed, Some(ed.scene().revision));
        assert_eq!(call(&mut ed, "GET", "/nope", Value::Null).0, 404);
    }

    #[test]
    fn decodes_query_strings() {
        assert_eq!(
            query_param("views=front%2Ctop&size=64", "views").as_deref(),
            Some("front,top")
        );
        assert_eq!(
            query_param("object=My+Vase", "object").as_deref(),
            Some("My Vase")
        );
        assert_eq!(query_param("a=%zz", "a").as_deref(), Some("%zz"));
        assert_eq!(query_param("a=%", "a").as_deref(), Some("%"));
        assert_eq!(query_param("a=%E3%81%82", "a").as_deref(), Some("あ"));
        assert_eq!(query_param("a=あ%", "a").as_deref(), Some("あ%"));
    }
}
