//! Transport-independent editor API. The native HTTP server and the
//! browser-only WebAssembly build both route `/api/...` requests through
//! [`handle`], so the two behave identically.

use serde_json::{Value, json};

use crate::engine::{self, CommandBatch, Editor, EngineError, Scene};
use crate::texture::{self, Look, Pattern, Texture};

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

    pub fn error(status: u16, message: impl Into<String>) -> Self {
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
    // Images go by reference; the viewport fetches them from /image.
    if let Some(images) = scene.get_mut("images") {
        *images = ed
            .scene()
            .images
            .iter()
            .map(|(name, i)| {
                let summary =
                    json!({ "mime": i.mime, "width": i.width, "height": i.height, "hash": i.hash() });
                (name.clone(), summary)
            })
            .collect::<serde_json::Map<_, _>>()
            .into();
    }
    if let Some(objects) = scene["objects"].as_array_mut() {
        for (value, o) in objects.iter_mut().zip(&ed.scene().objects) {
            if !o.modifiers.is_empty() {
                value["display"] = json!(ed.evaluated(o));
            }
            // Bone weights of each displayed vertex (four per vertex), so
            // the viewport bends the mesh exactly like the core.
            if !o.bones.is_empty() {
                let weights = crate::rig::weights(ed.evaluated(o), &o.bones);
                let joints: Vec<u16> = weights.iter().flatten().map(|(j, _)| *j).collect();
                let amounts: Vec<f64> = weights
                    .iter()
                    .flatten()
                    .map(|(_, w)| (*w as f64 * 1e4).round() / 1e4)
                    .collect();
                value["skin"] = json!({ "joints": joints, "weights": amounts });
            }
        }
    }
    json!({ "scene": scene, "history": history(ed), "ai": ai })
}

/// Parse a render request and gather the scene for it; `RenderJob::png`
/// does the (possibly long) rendering.
pub fn render_job(ed: &Editor, query: &str) -> Result<crate::render::RenderJob, Response> {
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
    let samples = match query_param(query, "samples").filter(|s| !s.is_empty()) {
        Some(s) => Some(
            s.parse()
                .map_err(|_| Response::error(400, "samples must be a number"))?,
        ),
        None => None,
    };
    Ok(crate::render::render_job(
        ed,
        &crate::render::RenderOptions {
            views,
            size,
            focus,
            frame,
            samples,
        },
    )?)
}

/// Finish a render job as a PNG response.
pub fn render_png(job: crate::render::RenderJob) -> Response {
    match job.png() {
        Ok(png) => Response {
            status: 200,
            content_type: "image/png",
            body: png,
            changed: None,
            disposition: None,
        },
        Err(e) => e.into(),
    }
}

/// One tile of a texture as PNG, so the viewport shows exactly what the
/// renderer and glTF export use: its colours, or with `kind=normal` the
/// normal map of its relief. Colours may omit the `#`.
fn texture(ed: &Editor, query: &str) -> Result<Response, Response> {
    let color = |key: &str, default: &str| -> Result<String, Response> {
        let c = query_param(query, key).unwrap_or_else(|| default.into());
        let c = if c.starts_with('#') {
            c
        } else {
            format!("#{c}")
        };
        Ok(engine::check_color(&c)?)
    };
    let pattern: Pattern =
        serde_json::from_value(json!(query_param(query, "pattern").unwrap_or_default()))
            .map_err(|_| Response::error(400, "unknown texture pattern"))?;
    let size = match query_param(query, "size") {
        Some(s) => s
            .parse::<u32>()
            .ok()
            .filter(|s| (8..=1024).contains(s))
            .ok_or_else(|| Response::error(400, "size must be 8-1024"))?,
        None => 256,
    };
    let relief = match query_param(query, "relief") {
        Some(r) => r
            .parse::<f64>()
            .ok()
            .filter(|r| (0.0..=1.0).contains(r))
            .ok_or_else(|| Response::error(400, "relief must be 0-1"))?,
        None => 0.0,
    };
    let image = match pattern {
        Pattern::Image => {
            let name = query_param(query, "image").unwrap_or_default();
            let asset = ed
                .scene()
                .images
                .get(&name)
                .ok_or_else(|| Response::error(404, format!("no image named {name:?}")))?;
            Some((name, asset.decode()?))
        }
        _ => None,
    };
    let t = Texture {
        image: image.as_ref().map(|(name, _)| name.clone()),
        ..Texture::new(pattern, &color("color2", "#3b2a22")?, 1.0).with_relief(relief)
    };
    let base = color("color", "#9aa0a6")?;
    let look = Look {
        base: &base,
        texture: &t,
        image: image.as_ref().map(|(_, px)| px),
        normal_map: None,
        baked: None,
    };
    let body = match query_param(query, "kind").as_deref() {
        Some("normal") => texture::bake_normal_png(&look, size),
        Some("color") | None => texture::bake_png(&base, &t, size),
        Some(_) => return Err(Response::error(400, "kind must be color or normal")),
    };
    Ok(Response {
        status: 200,
        content_type: "image/png",
        body,
        changed: None,
        disposition: None,
    })
}

/// A node graph's tiles for one object's material, as PNG: `kind` is
/// `color`, `orm` (roughness in G, metalness in B) or `normal` (relief).
fn node_tile(ed: &Editor, query: &str) -> Result<Response, Response> {
    let id = query_param(query, "object")
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| Response::error(400, "object must be an id"))?;
    let o = ed
        .scene()
        .objects
        .iter()
        .find(|o| o.id == id)
        .ok_or_else(|| Response::error(404, format!("no object with id {id}")))?;
    let size = match query_param(query, "size") {
        Some(s) => s
            .parse::<u32>()
            .ok()
            .filter(|s| (8..=1024).contains(s))
            .ok_or_else(|| Response::error(400, "size must be 8-1024"))?,
        None => 256,
    };
    let baked = crate::nodes::bake_material(&o.material, &ed.scene().images, size)
        .ok_or_else(|| Response::error(404, "the object's material has no node graph"))?;
    let t = o
        .material
        .texture
        .as_ref()
        .expect("a graph lives in a texture");
    let body = match query_param(query, "kind").as_deref() {
        Some("color") | None => texture::pixels_png(&baked.color),
        Some("orm") => texture::pixels_png(
            baked
                .orm
                .as_ref()
                .ok_or_else(|| Response::error(404, "the graph sets no roughness or metalness"))?,
        ),
        Some("normal") => texture::bake_normal_png(
            &Look {
                base: &o.material.color,
                texture: t,
                image: None,
                normal_map: None,
                baked: Some(&baked),
            },
            size,
        ),
        Some(_) => return Err(Response::error(400, "kind must be color, orm or normal")),
    };
    Ok(Response {
        status: 200,
        content_type: "image/png",
        body,
        changed: None,
        disposition: None,
    })
}

/// A scene image file as stored.
fn image(ed: &Editor, query: &str) -> Result<Response, Response> {
    let name = query_param(query, "name").unwrap_or_default();
    let asset = ed
        .scene()
        .images
        .get(&name)
        .ok_or_else(|| Response::error(404, format!("no image named {name:?}")))?;
    Ok(Response {
        status: 200,
        content_type: if asset.mime == "image/png" {
            "image/png"
        } else {
            "image/jpeg"
        },
        body: asset.data.to_vec(),
        changed: None,
        disposition: None,
    })
}

/// One path tracing pass: the scene is prepared under the editor lock,
/// then traced without it (`run`), so other requests are not held up.
pub struct PathJob {
    scene: std::sync::Arc<crate::pathtrace::Traced>,
    camera: crate::pathtrace::Camera,
    samples: u32,
}

impl PathJob {
    /// Add the samples to the camera's progressive image and answer with
    /// how many it now holds (u32, little-endian) followed by its
    /// denoised pixels as sRGB RGBA bytes, rows from the top.
    pub fn run(self) -> Response {
        let (total, pixels) = self.scene.pass(&self.camera, self.samples);
        let mut body = total.to_le_bytes().to_vec();
        body.extend(pixels);
        Response {
            status: 200,
            content_type: "application/octet-stream",
            body,
            changed: None,
            disposition: None,
        }
    }
}

/// A number from the query, or `default` (an error when there is none).
fn number(query: &str, key: &str, default: Option<f64>) -> Result<f64, Response> {
    match query_param(query, key).filter(|s| !s.is_empty()) {
        Some(v) => v
            .parse::<f64>()
            .ok()
            .filter(|x| x.is_finite())
            .ok_or_else(|| Response::error(400, format!("{key} must be a number"))),
        None => default.ok_or_else(|| Response::error(400, format!("{key} is required"))),
    }
}

/// An "x,y,z" point from the query.
fn point(query: &str, key: &str) -> Result<glam::DVec3, Response> {
    let raw = query_param(query, key).unwrap_or_default();
    let v: Vec<f64> = raw
        .split(',')
        .filter_map(|x| x.trim().parse().ok())
        .collect();
    match v[..] {
        [x, y, z] if v.iter().all(|c| c.is_finite()) => Ok(glam::DVec3::new(x, y, z)),
        _ => Err(Response::error(400, format!("{key} must be x,y,z"))),
    }
}

fn frame_param(query: &str) -> Result<Option<f64>, Response> {
    match query_param(query, "frame").filter(|s| !s.is_empty()) {
        Some(f) => Ok(Some(crate::anim::check_frame(
            f.parse()
                .map_err(|_| Response::error(400, "frame must be a number"))?,
        )?)),
        None => Ok(None),
    }
}

/// Image size `w` x `h` (8-`max`), `fov` (degrees), `aperture` (lens
/// radius in metres, 0-1) and `focus` (metres, 0 = the target's distance).
fn lens(query: &str, max: f64) -> Result<(usize, usize, f64, f64, f64), Response> {
    let (w, h) = (number(query, "w", None)?, number(query, "h", None)?);
    if !(8.0..=max).contains(&w) || !(8.0..=max).contains(&h) {
        return Err(Response::error(
            400,
            format!("w and h must be between 8 and {max}"),
        ));
    }
    let fov = number(query, "fov", Some(36.0))?;
    if !(1.0..=170.0).contains(&fov) {
        return Err(Response::error(
            400,
            "fov must be between 1 and 170 degrees",
        ));
    }
    let aperture = number(query, "aperture", Some(0.0))?;
    let focus = number(query, "focus", Some(0.0))?;
    if !(0.0..=1.0).contains(&aperture) || !(0.0..=1e4).contains(&focus) {
        return Err(Response::error(
            400,
            "aperture must be 0-1 m and focus 0-10000 m",
        ));
    }
    Ok((w as usize, h as usize, fov, aperture, focus))
}

/// A camera at `eye` looking at `target`, from the query.
fn camera(query: &str, max: f64) -> Result<crate::pathtrace::Camera, Response> {
    let (w, h, fov, aperture, focus) = lens(query, max)?;
    let (eye, target) = (point(query, "eye")?, point(query, "target")?);
    if eye.distance(target) < 1e-9 {
        return Err(Response::error(400, "eye and target must differ"));
    }
    Ok(crate::pathtrace::Camera {
        aperture,
        focus,
        ..crate::pathtrace::Camera::new(eye, target, fov, w, h)
    })
}

/// Parse a path tracing request: the camera (`w`, `h`, `eye`, `target`,
/// optional `fov`, `aperture`, `focus`), `samples` and `frame`. Requests for
/// the same camera and scene keep refining one image.
pub fn path_job(ed: &Editor, query: &str) -> Result<PathJob, Response> {
    let camera = camera(query, 2048.0)?;
    let samples = number(query, "samples", Some(1.0))?;
    if !(1.0..=64.0).contains(&samples) {
        return Err(Response::error(400, "samples must be between 1 and 64"));
    }
    Ok(PathJob {
        scene: crate::pathtrace::traced(ed, frame_param(query)?)?,
        camera,
        samples: samples as u32,
    })
}

/// A final render: the scene (one per frame for animations), the camera,
/// samples and background.
pub struct ImageJob {
    scenes: Vec<std::sync::Arc<crate::pathtrace::Traced>>,
    camera: crate::pathtrace::Camera,
    samples: u32,
    transparent: bool,
    fps: f64,
}

/// Parse a final render: the camera (`eye`/`target`, or a `view` preset
/// framing the scene), `w`, `h`, `fov`, `aperture`, `focus`, `samples`
/// (1-4096), `background` (`studio` or `transparent`) and `frame`, or
/// `frames=a-b` for an animated PNG.
pub fn image_job(ed: &Editor, query: &str) -> Result<ImageJob, Response> {
    let frames: Vec<Option<f64>> = match query_param(query, "frames").filter(|s| !s.is_empty()) {
        Some(range) => {
            let (a, b) = range
                .split_once('-')
                .and_then(|(a, b)| {
                    Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?))
                })
                .ok_or_else(|| Response::error(400, "frames must be start-end"))?;
            let (a, b) = (crate::anim::check_frame(a)?, crate::anim::check_frame(b)?);
            if b < a || b - a >= 240.0 {
                return Err(Response::error(
                    400,
                    "frames must run forward, at most 240 of them",
                ));
            }
            (0..=(b - a) as usize).map(|k| Some(a + k as f64)).collect()
        }
        None => vec![frame_param(query)?],
    };
    let samples = number(query, "samples", Some(64.0))?;
    if !(1.0..=4096.0).contains(&samples) {
        return Err(Response::error(400, "samples must be between 1 and 4096"));
    }
    let transparent = match query_param(query, "background").as_deref() {
        None | Some("") | Some("studio") => false,
        Some("transparent") => true,
        Some(_) => {
            return Err(Response::error(
                400,
                "background must be studio or transparent",
            ));
        }
    };
    let scenes = frames
        .iter()
        .map(|f| crate::pathtrace::traced_uncached(ed, *f))
        .collect::<Result<Vec<_>, _>>()?;
    let camera = if query_param(query, "eye").is_some() {
        camera(query, 4096.0)?
    } else {
        let (w, h, fov, aperture, focus) = lens(query, 4096.0)?;
        let view = crate::render::parse_views(
            &query_param(query, "view").unwrap_or_else(|| "iso".into()),
        )?;
        let [view] = &view[..] else {
            return Err(Response::error(400, "view names one view"));
        };
        let (eye, target) = scenes[0].frame_view(view, fov, w as f64 / h as f64);
        crate::pathtrace::Camera {
            aperture,
            focus,
            ..crate::pathtrace::Camera::new(eye, target, fov, w, h)
        }
    };
    let work = camera.width as f64 * camera.height as f64 * samples * scenes.len() as f64;
    if work > 4e9 {
        return Err(Response::error(
            400,
            "too much work: lower the size, samples or frames (w x h x samples x frames at most 4e9)",
        ));
    }
    Ok(ImageJob {
        scenes,
        camera,
        samples: samples as u32,
        transparent,
        fps: ed.scene().animation.fps,
    })
}

impl ImageJob {
    /// Render and answer with a PNG (an animated PNG for several frames).
    pub fn run(self) -> Response {
        let frames: Vec<Vec<u8>> = self
            .scenes
            .iter()
            .map(|t| t.still(&self.camera, self.samples, self.transparent))
            .collect();
        match crate::pathtrace::png(
            &frames,
            self.camera.width,
            self.camera.height,
            self.transparent,
            self.fps,
        ) {
            Ok(body) => Response {
                status: 200,
                content_type: "image/png",
                body,
                changed: None,
                disposition: None,
            },
            Err(e) => e.into(),
        }
    }
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
        ("GET", "/render") => render_job(ed, query).map(render_png),
        ("GET", "/pathtrace") => path_job(ed, query).map(PathJob::run),
        ("GET", "/render/image") => image_job(ed, query).map(ImageJob::run),
        ("GET", "/texture") => texture(ed, query),
        ("GET", "/image") => image(ed, query),
        ("GET", "/nodes") => node_tile(ed, query),
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
        let tex = handle(
            &mut ed,
            "GET",
            "/texture?pattern=brick&color=a4452c&color2=%23d8d0c4&size=32",
            &[],
            false,
        );
        assert_eq!((tex.status, tex.content_type), (200, "image/png"));
        assert_eq!(
            handle(&mut ed, "GET", "/texture?pattern=plaid", &[], false).status,
            400
        );
        let normal = handle(
            &mut ed,
            "GET",
            "/texture?pattern=tiles&relief=0.5&kind=normal&size=16",
            &[],
            false,
        );
        assert_eq!((normal.status, normal.content_type), (200, "image/png"));
        assert_eq!(
            handle(
                &mut ed,
                "GET",
                "/texture?pattern=tiles&relief=2",
                &[],
                false
            )
            .status,
            400
        );

        // Images are served as stored, and summarized in /state.
        use base64::Engine as _;
        let png = crate::image::tests::tiny_png();
        let data = base64::engine::general_purpose::STANDARD.encode(&png);
        let (status, _) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands": [{"op": "add_image", "name": "Logo", "data": data}]}),
        );
        assert_eq!(status, 200);
        let img = handle(&mut ed, "GET", "/image?name=Logo", &[], false);
        assert_eq!(
            (img.status, img.content_type, img.body),
            (200, "image/png", png)
        );
        assert_eq!(
            handle(&mut ed, "GET", "/image?name=Nope", &[], false).status,
            404
        );
        let (_, st) = call(&mut ed, "GET", "/state", Value::Null);
        assert_eq!(st["scene"]["images"]["Logo"]["width"], 2);
        assert!(st["scene"]["images"]["Logo"].get("data").is_none());
        let embossed = handle(
            &mut ed,
            "GET",
            "/texture?pattern=image&image=Logo&relief=1&kind=normal&size=16",
            &[],
            false,
        );
        assert_eq!(embossed.status, 200);
    }

    #[test]
    fn path_tracing_refines_one_image_per_camera() {
        let mut ed = Editor::new();
        call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands": [{"op": "add", "name": "Box", "primitive": {"kind": "cube"}, "translation": [0, 0.5, 0]}]}),
        );
        let view = "/pathtrace?w=24&h=16&eye=0,1.5,4&target=0,0.5,0&samples=1";
        let pass = |ed: &mut Editor, q: &str| {
            let r = handle(ed, "GET", q, &[], false);
            assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
            assert_eq!(r.body.len(), 4 + 24 * 16 * 4);
            let n = u32::from_le_bytes(r.body[..4].try_into().unwrap());
            (n, r.body[4..].to_vec())
        };
        assert_eq!(pass(&mut ed, view).0, 1);
        let (n, pixels) = pass(&mut ed, view);
        assert_eq!(n, 2, "the same camera keeps adding samples");
        // The box covers the middle; the sky is clear.
        assert_eq!(pixels[(8 * 24 + 12) * 4 + 3], 255);
        assert_eq!(pixels[3], 0);
        let moved = view.replace("eye=0,1.5,4", "eye=1,1.5,4");
        assert_eq!(pass(&mut ed, &moved).0, 1, "a new camera starts over");
        // An edit starts over too.
        call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands": [{"op": "material", "id": "Box", "color": "#ff0000"}]}),
        );
        assert_eq!(pass(&mut ed, &moved).0, 1);
        for bad in [
            "/pathtrace?w=24&h=16&eye=0,1,4&target=0,0,0&samples=0",
            "/pathtrace?w=4&h=16&eye=0,1,4&target=0,0,0",
            "/pathtrace?w=24&h=16&eye=0,1&target=0,0,0",
            "/pathtrace?w=24&h=16&eye=1,1,1&target=1,1,1",
        ] {
            assert_eq!(handle(&mut ed, "GET", bad, &[], false).status, 400, "{bad}");
        }
        // Agents can ask for a path-traced render too.
        let r = handle(&mut ed, "GET", "/render?size=64&samples=2", &[], false);
        assert_eq!((r.status, r.content_type), (200, "image/png"));
        assert_eq!(
            handle(&mut ed, "GET", "/render?size=64&samples=999", &[], false).status,
            422
        );
    }

    #[test]
    fn final_renders_are_pngs_and_animations_are_apngs() {
        let mut ed = Editor::new();
        call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands": [
                {"op": "add", "name": "Box", "primitive": {"kind": "cube"}, "translation": [0, 0.5, 0]},
                {"op": "set_keyframe", "id": "Box", "property": "translation", "frame": 1, "value": [0, 0.5, 0]},
                {"op": "set_keyframe", "id": "Box", "property": "translation", "frame": 3, "value": [1, 0.5, 0]}
            ]}),
        );
        let still = handle(
            &mut ed,
            "GET",
            "/render/image?w=32&h=18&samples=2&view=front&aperture=0.1",
            &[],
            false,
        );
        assert_eq!((still.status, still.content_type), (200, "image/png"));
        let decoder = png::Decoder::new(std::io::Cursor::new(still.body));
        let info = decoder.read_info().unwrap();
        assert_eq!((info.info().width, info.info().height), (32, 18));
        assert_eq!(
            info.info().color_type,
            png::ColorType::Rgb,
            "the studio backdrop is opaque"
        );
        let clear = handle(
            &mut ed,
            "GET",
            "/render/image?w=16&h=16&samples=1&background=transparent&eye=0,1,4&target=0,0.5,0",
            &[],
            false,
        );
        let decoder = png::Decoder::new(std::io::Cursor::new(clear.body));
        assert_eq!(
            decoder.read_info().unwrap().info().color_type,
            png::ColorType::Rgba
        );
        // A frame range is an animated PNG, one frame each.
        let anim = handle(
            &mut ed,
            "GET",
            "/render/image?w=16&h=16&samples=1&frames=1-3",
            &[],
            false,
        );
        let decoder = png::Decoder::new(std::io::Cursor::new(anim.body));
        let info = decoder.read_info().unwrap();
        assert_eq!(info.info().animation_control().unwrap().num_frames, 3);
        for bad in [
            "/render/image?w=16&h=16&samples=0",
            "/render/image?w=16&h=16&frames=5-1",
            "/render/image?w=16&h=16&background=pink",
            "/render/image?w=4000&h=4000&samples=4096",
            "/render/image?w=16&h=16&aperture=2",
        ] {
            assert_eq!(handle(&mut ed, "GET", bad, &[], false).status, 400, "{bad}");
        }
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
