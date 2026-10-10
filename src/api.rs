//! Transport-independent editor API. The native HTTP server and the
//! browser-only WebAssembly build both route `/api/...` requests through
//! [`handle`], so the two behave identically.

use serde_json::{Value, json};

use crate::engine::{self, CommandBatch, Editor, EngineError, Scene};
use crate::texture::{self, Look, Pattern, Texture};

/// Missing mode preserves legacy HTTP callers; the UI opts into review.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatMode {
    #[default]
    Apply,
    Proposal,
}

/// A provider reply follows the same validation and proposal/command paths as
/// other callers. Guard the context revision before either kind of mutation.
pub fn complete_chat(
    ed: &mut Editor,
    prompt: &str,
    batch: &CommandBatch,
    mode: ChatMode,
) -> Result<Value, EngineError> {
    ed.ensure_revision(batch.expected_revision)?;
    let mut out = match mode {
        ChatMode::Apply => {
            let result = ed.apply(batch)?;
            json!({"revision": result.revision, "created": result.created})
        }
        ChatMode::Proposal => {
            let ids = ed.propose(&crate::proposal::ProposalRequest {
                title: prompt.chars().take(80).collect(),
                note: Some(prompt.chars().take(500).collect()),
                author: Some("chat".into()),
                commands: batch.commands.clone(),
                variants: Vec::new(),
            })?;
            json!({"revision": ed.scene().revision, "created": [], "proposal_id": ids[0]})
        }
    };
    out["mode"] = json!(mode);
    out["commands"] = json!(batch.commands);
    out["history"] = history(ed);
    Ok(out)
}

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
    json!({ "scene": scene, "history": history(ed), "ai": ai, "proposals": proposals(ed) })
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
    let (content_type, body) = asset
        .portable()
        .map_err(|e| Response::error(422, e.message))?;
    Ok(Response {
        status: 200,
        content_type,
        body,
        changed: None,
        disposition: None,
    })
}

/// The world's environment for the viewport, at most `w` texels wide
/// (default 512): little-endian u32 width and height, the sun's direction
/// and the light it gives (three f32 each; zeros without a sun), then
/// linear RGBA f32 texels, bottom row first. A sun is taken out of the
/// map, for the viewport to cast its shadows with a light; strength and
/// rotation are left to the viewport. The studio sends its dome.
fn environment(ed: &Editor, query: &str) -> Result<Response, Response> {
    let w = number(query, "w", Some(512.0))?;
    if !(8.0..=2048.0).contains(&w) {
        return Err(Response::error(400, "w must be 8-2048"));
    }
    let scene = ed.scene();
    let map =
        crate::world::env_map(&scene.world, scene).map_err(|e| Response::error(422, e.message))?;
    let (map, sun) = match map {
        Some(map) => match map.sun() {
            Some((dir, power, rest)) => (
                std::sync::Arc::new(crate::world::EnvMap::new(map.width, map.height, rest)),
                Some((dir, power)),
            ),
            None => (map, None),
        },
        None => (
            std::sync::Arc::new(crate::world::studio_map(
                1024,
                crate::pathtrace::studio_dome,
            )),
            None,
        ),
    };
    let map = if map.width > w as usize {
        map.shrink(w as usize)
    } else {
        map
    };
    let mut body = Vec::with_capacity(32 + map.texels.len() * 16);
    body.extend((map.width as u32).to_le_bytes());
    body.extend((map.height as u32).to_le_bytes());
    let (dir, power) = sun.unwrap_or_default();
    for v in [dir.x, dir.y, dir.z, power.x, power.y, power.z] {
        body.extend((v as f32).to_le_bytes());
    }
    for row in map.texels.chunks(map.width).rev() {
        for c in row {
            for v in [c[0], c[1], c[2], 1.0] {
                body.extend(v.to_le_bytes());
            }
        }
    }
    Ok(Response {
        status: 200,
        content_type: "application/octet-stream",
        body,
        changed: None,
        disposition: None,
    })
}

/// The batches that made the scene (the last `limit`, default 100), each
/// numbered from 1. Image data is left out: those steps can't be revised.
pub fn history_steps(ed: &Editor, query: &str) -> Result<Value, Response> {
    let limit = number(query, "limit", Some(100.0))?;
    if !(1.0..=1000.0).contains(&limit) {
        return Err(Response::error(400, "limit must be 1-1000"));
    }
    let (steps, loaded) = ed.history();
    let skip = steps.len().saturating_sub(limit as usize);
    let listed: Vec<Value> = steps
        .iter()
        .enumerate()
        .skip(skip)
        .map(|(i, step)| {
            let mut commands = serde_json::to_value(&step.commands).expect("commands serialize");
            let mut editable = true;
            for c in commands.as_array_mut().into_iter().flatten() {
                if c["op"] == "add_image" {
                    let bytes = c["data"].as_str().map_or(0, str::len);
                    c["data"] = json!(format!("({bytes} characters of image data)"));
                    editable = false;
                }
            }
            let mut item = json!({ "step": i + 1, "source": step.source, "actor": step.actor, "commands": commands, "editable": editable });
            if let Some(revision) = step.rebased_from {
                item["rebased_from"] = json!(revision);
            }
            item
        })
        .collect();
    Ok(
        json!({ "revision": ed.scene().revision, "total": steps.len(), "from_loaded_scene": loaded, "steps": listed }),
    )
}

#[derive(serde::Deserialize)]
struct Revision {
    #[serde(default)]
    expected_revision: Option<u64>,
    step: usize,
    commands: Vec<crate::engine::Command>,
}

/// Pending proposals (without their scenes) and recent decisions.
pub fn proposals(ed: &Editor) -> Value {
    json!({
        "version": ed.proposals_version(),
        "pending": ed.proposals().iter().map(|p| p.summary()).collect::<Vec<_>>(),
        "decided": ed.decided(),
    })
}

fn proposal_id(query: &str) -> Result<u64, Response> {
    query_param(query, "id")
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| Response::error(400, "id must be a proposal number"))
}

fn preview(ed: &Editor, query: &str) -> Result<Editor, Response> {
    let id = proposal_id(query)?;
    ed.preview(id)
        .ok_or_else(|| Response::error(404, format!("no proposal {id}")))
}

/// A path-traced image of proposal `id`'s scene, like `/render/image`
/// (a thumbnail from the viewer's camera).
pub fn proposal_image_job(ed: &Editor, query: &str) -> Result<ImageJob, Response> {
    image_job(&preview(ed, query)?, query)
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

fn ortho_height(query: &str) -> Result<Option<f64>, Response> {
    if query_param(query, "ortho_height").is_none() {
        return Ok(None);
    }
    let height = number(query, "ortho_height", None)?;
    if !(0.001..=1e4).contains(&height) {
        return Err(Response::error(400, "ortho_height must be 0.001-10000 m"));
    }
    Ok(Some(height))
}

fn projection_query(camera: &mut crate::pathtrace::Camera, query: &str) -> Result<(), Response> {
    if let Some(height) = ortho_height(query)? {
        camera.ortho_height = Some(height);
    }
    if camera.ortho_height.is_some() && query_param(query, "fov").is_some() {
        return Err(Response::error(
            400,
            "set ortho_height to change an orthographic view",
        ));
    }
    Ok(())
}

/// A camera at `eye` looking at `target`, from the query.
fn query_camera(
    ed: &Editor,
    query: &str,
    max: f64,
    frame: Option<f64>,
) -> Result<crate::pathtrace::Camera, Response> {
    let (w, h, fov, aperture, focus) = lens(query, max)?;
    if let Some(id) = query_param(query, "camera") {
        if query_param(query, "eye").is_some() || query_param(query, "target").is_some() {
            return Err(Response::error(
                400,
                "give a scene camera or eye/target, not both",
            ));
        }
        if query_param(query, "up").is_some() {
            return Err(Response::error(
                400,
                "change the scene camera rotation to change its up vector",
            ));
        }
        let id = id
            .parse::<u64>()
            .map(engine::ObjRef::Id)
            .unwrap_or(engine::ObjRef::Name(id));
        let mut c = crate::camera::resolve(ed, &id, frame, w, h)?;
        if query_param(query, "fov").is_some() {
            c.fov = fov;
        }
        if query_param(query, "aperture").is_some() {
            c.aperture = aperture;
        }
        if query_param(query, "focus").is_some() {
            c.focus = focus;
        }
        projection_query(&mut c, query)?;
        return Ok(c);
    }
    let (eye, target) = (point(query, "eye")?, point(query, "target")?);
    if eye.distance(target) < 1e-9 {
        return Err(Response::error(400, "eye and target must differ"));
    }
    let up = if query_param(query, "up").is_some() {
        point(query, "up")?
    } else {
        glam::DVec3::Y
    };
    if up.length() < 1e-9
        || query_param(query, "up").is_some()
            && (target - eye).normalize().cross(up.normalize()).length() < 1e-6
    {
        return Err(Response::error(
            400,
            "up must be nonzero and not parallel to the view",
        ));
    }
    let mut c = crate::pathtrace::Camera {
        up,
        aperture,
        focus,
        ..crate::pathtrace::Camera::new(eye, target, fov, w, h)
    };
    projection_query(&mut c, query)?;
    Ok(c)
}

/// Parse a path tracing request: the camera (`w`, `h`, `eye`, `target`,
/// optional `fov`, `aperture`, `focus`), `samples` and `frame`. Requests for
/// the same camera and scene keep refining one image.
pub fn path_job(ed: &Editor, query: &str) -> Result<PathJob, Response> {
    let camera = query_camera(ed, query, 2048.0, frame_param(query)?)?;
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
    cameras: Vec<crate::pathtrace::Camera>,
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
    let mut camera =
        if query_param(query, "eye").is_some() || query_param(query, "camera").is_some() {
            query_camera(ed, query, 4096.0, frames[0])?
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
    projection_query(&mut camera, query)?;
    let work = camera.width as f64 * camera.height as f64 * samples * scenes.len() as f64;
    if work > 4e9 {
        return Err(Response::error(
            400,
            "too much work: lower the size, samples or frames (w x h x samples x frames at most 4e9)",
        ));
    }
    let cameras = frames
        .iter()
        .map(|f| {
            if query_param(query, "camera").is_some() {
                query_camera(ed, query, 4096.0, *f)
            } else {
                Ok(camera.clone())
            }
        })
        .collect::<Result<Vec<_>, Response>>()?;
    Ok(ImageJob {
        scenes,
        cameras,
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
            .zip(&self.cameras)
            .map(|(t, c)| t.still(c, self.samples, self.transparent))
            .collect();
        match crate::pathtrace::png(
            &frames,
            self.cameras[0].width,
            self.cameras[0].height,
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
        ("GET" | "POST", "/frame") => {
            let f = query_param(query, "frame")
                .ok_or_else(|| Response::error(400, "frame is required"))?;
            let f = f
                .parse()
                .map_err(|_| Response::error(400, "frame must be a number"))?;
            // Detached proposal/history previews evaluate their own scene, without
            // changing the shared editor or publishing a revision event.
            let mut preview = Editor::new();
            let (scene, revision) = if method == "POST" {
                let scene: Scene = parse(body)?;
                let revision = scene.revision;
                preview.load(scene)?;
                (preview.scene_at(f)?, revision)
            } else {
                (ed.scene_at(f)?, ed.scene().revision)
            };
            Ok(Response::json(
                200,
                json!({"revision": revision, "frame": f, "objects": scene.objects.iter().map(|o| json!({"id":o.id,"transform":o.transform,"material":o.material,"camera":o.camera,"light":o.light})).collect::<Vec<_>>()}),
            ))
        }
        ("GET", "/context") => {
            let frame = match query_param(query, "frame").filter(|s| !s.is_empty()) {
                Some(f) => {
                    Some(crate::anim::check_frame(f.parse().map_err(|_| {
                        Response::error(400, "frame must be a number")
                    })?)?)
                }
                None => None,
            };
            Ok(Response::json(200, engine::context_at_checked(ed, frame)?))
        }
        ("GET", "/schema") => Ok(Response::json(200, engine::command_schema())),
        ("GET", "/inspect") => Ok(Response::json(200, crate::inspect::inspect(ed))),
        ("GET", "/ai") => Ok(Response::json(200, json!({ "enabled": ai }))),
        ("POST", "/commands") => {
            let batch: CommandBatch = parse(body)?;
            let r = ed.apply(&batch)?;
            let mut out = json!(r);
            out["history"] = history(ed);
            Ok(Response::json(200, out).changed(r.revision))
        }
        ("GET", "/history") => history_steps(ed, query).map(|v| Response::json(200, v)),
        ("POST", "/history/revise") => {
            let r: Revision = parse(body)?;
            ed.ensure_revision(r.expected_revision)?;
            let revision = ed.revise(r.step, r.commands)?;
            Ok(
                Response::json(200, json!({ "revision": revision, "history": history(ed) }))
                    .changed(revision),
            )
        }
        ("POST", "/history/preview") => {
            let r: Revision = parse(body)?;
            let preview = ed.revise_preview(r.step, r.commands)?;
            Ok(Response::json(
                200,
                json!({ "scene": state(&preview, false)["scene"] }),
            ))
        }
        ("GET", "/proposals") => Ok(Response::json(200, proposals(ed))),
        ("POST", "/proposals") => {
            let req: crate::proposal::ProposalRequest = parse(body)?;
            let ids = ed.propose(&req)?;
            let mut out = proposals(ed);
            out["ids"] = json!(ids);
            Ok(Response::json(200, out))
        }
        ("GET", "/proposal") => {
            let p = preview(ed, query)?;
            let id = proposal_id(query)?;
            let summary = ed
                .proposals()
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.summary());
            Ok(Response::json(
                200,
                json!({ "proposal": summary, "scene": state(&p, false)["scene"] }),
            ))
        }
        ("GET", "/proposal/render") => proposal_image_job(ed, query).map(ImageJob::run),
        ("POST", "/proposal/accept") => {
            let r = ed.accept(proposal_id(query)?)?;
            Ok(Response::json(
                200,
                json!({ "revision": r.revision, "created": r.created, "history": history(ed) }),
            )
            .changed(r.revision))
        }
        ("POST", "/proposal/reject") => {
            ed.reject(proposal_id(query)?)?;
            Ok(Response::json(200, proposals(ed)))
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
                actor: None,
                commands,
                expected_revision: None,
                rebase: false,
                source: Some("import".into()),
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
        ("GET", "/environment") => environment(ed, query),
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
    #[test]
    fn frame_evaluation_is_read_only_and_rejects_conflicting_animation() {
        let mut ed = Editor::new();
        let (status, out) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[
                {"op":"add","name":"A","primitive":{"kind":"cube"}},
                {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[2,0,0]},
                {"op":"add","name":"Follower","primitive":{"kind":"cube"},"translation":[4,0,0]},
                {"op":"constrain","id":"Follower","align":"y","from":"A"},
                {"op":"set_keyframe","id":"A","property":"translation","frame":1,"value":[0,0,0],"interpolation":"linear"},
                {"op":"set_keyframe","id":"A","property":"translation","frame":3,"value":[0,2,0]}
            ]}),
        );
        assert_eq!(status, 200, "{out}");
        let rest = ed.scene().clone();
        let history = history(&ed);
        let (status, frame) = call(&mut ed, "GET", "/frame?frame=2", Value::Null);
        assert_eq!(status, 200, "{frame}");
        assert_eq!(
            frame["objects"][2]["transform"]["translation"],
            json!([4.0, 1.0, 0.0])
        );
        let (_, ctx) = call(&mut ed, "GET", "/context?frame=2", Value::Null);
        assert_eq!(
            ctx["objects"][2]["pose"]["transform"],
            frame["objects"][2]["transform"]
        );
        let mut detached = rest.clone();
        detached.objects[0].tracks[0].keys[1].value = vec![0., 6., 0.];
        let (status, preview) = call(&mut ed, "POST", "/frame?frame=2", json!(detached));
        assert_eq!(status, 200, "{preview}");
        assert_eq!(preview["objects"][2]["transform"]["translation"][1], 3.);
        assert_eq!(preview["revision"], rest.revision);
        assert_eq!(*ed.scene(), rest);
        assert_eq!(super::history(&ed), history);
        for query in [
            "/frame",
            "/frame?frame=no",
            "/frame?frame=-1",
            "/frame?frame=100001",
        ] {
            assert_ne!(call(&mut ed, "GET", query, Value::Null).0, 200);
        }
        assert_eq!(call(&mut ed,"POST","/commands",json!({"commands":[
            {"op":"constrain","id":"Follower","distance":2,"from":"B"},
            {"op":"set_keyframe","id":"A","property":"translation","frame":3,"value":[0,10,0]},
            {"op":"set_keyframe","id":"B","property":"translation","frame":1,"value":[2,0,0],"interpolation":"linear"},
            {"op":"set_keyframe","id":"B","property":"translation","frame":3,"value":[3,0,0]}
        ]})).0,200);
        let rest = ed.scene().clone();
        for path in [
            "/frame?frame=2",
            "/context?frame=2",
            "/render?frame=2&size=64",
            "/render/image?frame=2&w=8&h=8&samples=1",
        ] {
            let (status, out) = call(&mut ed, "GET", path, Value::Null);
            assert_eq!(status, 422, "{path}: {out}");
            assert!(
                out["error"].as_str().unwrap().contains("constraint"),
                "{path}: {out}"
            );
            assert_eq!(*ed.scene(), rest);
        }
    }

    #[test]
    fn light_proposals_rendering_and_scene_loading_share_validation() {
        let mut ed = Editor::new();
        assert_eq!(call(&mut ed,"POST","/commands",json!({"commands":[{"op":"add_light","name":"Key","translation":[0,2,3]},{"op":"add","primitive":{"kind":"cube"}}]})).0,200);
        let (_, p) = call(
            &mut ed,
            "POST",
            "/proposals",
            json!({"title":"Warm key","commands":[{"op":"light_settings","id":"Key","lamp":{"color":"#ff6633","intensity":25}}]}),
        );
        let id = p["ids"][0].as_u64().unwrap();
        assert_eq!(
            ed.scene().objects[0].light.as_ref().unwrap().color,
            "#ffffff"
        );
        assert_eq!(
            call(
                &mut ed,
                "POST",
                &format!("/proposal/accept?id={id}"),
                json!({})
            )
            .0,
            200
        );
        assert_eq!(
            ed.scene().objects[0].light.as_ref().unwrap().color,
            "#ff6633"
        );
        let rendered = handle(&mut ed, "GET", "/render?views=front&size=64", &[], false);
        assert_eq!(rendered.status, 200);
        assert!(rendered.body.starts_with(b"\x89PNG"));
        assert_eq!(call(&mut ed, "POST", "/undo", json!({})).0, 200);
        let (_, ctx) = call(&mut ed, "GET", "/context", Value::Null);
        assert_eq!(ctx["objects"][0]["light"]["intensity"], 10.);
        let mut loaded = serde_json::to_value(ed.scene()).unwrap();
        loaded["objects"][0]["light"]["intensity"] = json!(-1);
        assert_eq!(call(&mut ed, "PUT", "/scene", loaded).0, 422);
    }

    #[test]
    fn optical_animation_reaches_named_camera_render_and_validates_atomically() {
        let mut ed = Editor::new();
        let commands = json!([
            {"op":"add_camera","name":"Shot","translation":[0,1,4]},
            {"op":"add_light","name":"Key","translation":[0,2,2]},
            {"op":"add","primitive":{"kind":"cube"}},
            {"op":"set_keyframe","id":"Shot","property":"camera_fov","frame":1,"value":20,"interpolation":"linear"},
            {"op":"set_keyframe","id":"Shot","property":"camera_fov","frame":3,"value":60},
            {"op":"set_keyframe","id":"Shot","property":"camera_aperture","frame":1,"value":0.1},
            {"op":"set_keyframe","id":"Shot","property":"camera_focus","frame":1,"value":4},
            {"op":"set_keyframe","id":"Key","property":"light_intensity","frame":1,"value":0,"interpolation":"linear"},
            {"op":"set_keyframe","id":"Key","property":"light_intensity","frame":3,"value":20}
        ]);
        assert_eq!(
            call(&mut ed, "POST", "/commands", json!({"commands":commands})).0,
            200
        );
        let job = path_job(&ed, "camera=Shot&frame=2&w=8&h=8")
            .unwrap_or_else(|r| panic!("status {}", r.status));
        assert_eq!(
            (job.camera.fov, job.camera.aperture, job.camera.focus),
            (40., 0.1, 4.)
        );
        assert_eq!(
            path_job(&ed, "camera=Shot&frame=2&fov=75&w=8&h=8")
                .unwrap_or_else(|r| panic!("status {}", r.status))
                .camera
                .fov,
            75.
        );
        assert_eq!(
            path_job(&ed, "camera=Shot&w=8&h=8")
                .unwrap_or_else(|r| panic!("status {}", r.status))
                .camera
                .fov,
            36.
        );
        assert_eq!(
            image_job(&ed, "camera=Shot&w=8&h=8&samples=1&frames=1-3")
                .unwrap_or_else(|r| panic!("status {}", r.status))
                .run()
                .status,
            200
        );
        let before = ed.scene().clone();
        assert_eq!(call(&mut ed,"POST","/commands",json!({"commands":[{"op":"move","id":"Shot","offset":[1,0,0]},{"op":"set_keyframe","id":"Key","property":"camera_fov","frame":2,"value":40}]})).0,422);
        assert_eq!(*ed.scene(), before);
        assert_eq!(call(&mut ed, "POST", "/undo", json!({})).0, 200);
        assert!(ed.scene().objects.is_empty());
    }
    #[test]
    fn scene_camera_render_proposals_and_history_share_the_lens() {
        let mut ed = Editor::new();
        assert_eq!(call(&mut ed,"POST","/commands",json!({"commands":[{"op":"add_camera","name":"Shot","translation":[0,1,4]},{"op":"add","primitive":{"kind":"cube"}}]})).0,200);
        let camera = path_job(&ed, "camera=Shot&w=8&h=8")
            .unwrap_or_else(|r| panic!("unexpected response: {}", r.status))
            .camera;
        assert_eq!(camera.eye, glam::DVec3::new(0., 1., 4.));
        assert_eq!(camera.fov, 36.);
        assert_eq!(
            image_job(&ed, "camera=1&w=8&h=8&samples=1&frames=1-2")
                .unwrap_or_else(|r| panic!("unexpected response: {}", r.status))
                .run()
                .status,
            200
        );
        assert!(path_job(&ed, "camera=2&w=8&h=8").is_err());
        assert!(path_job(&ed, "w=8&h=8&camera=Shot&eye=0,0,4&target=0,0,0").is_err());
        assert!(path_job(&ed, "w=8&h=8&eye=0,0,4&target=0,0,0&up=0,0,0").is_err());
        // Legacy free-view top/bottom requests rely on Frame's stable basis
        // fallback. Only an explicitly parallel up vector is invalid.
        for eye in ["0,4,0", "0,-4,0"] {
            assert!(path_job(&ed, &format!("w=8&h=8&eye={eye}&target=0,0,0")).is_ok());
            assert!(path_job(&ed, &format!("w=8&h=8&eye={eye}&target=0,0,0&up=0,1,0")).is_err());
        }
        let (_, p) = call(
            &mut ed,
            "POST",
            "/proposals",
            json!({"title":"Wide shot","commands":[{"op":"camera_settings","id":"Shot","lens":{"fov":60,"focus":4}}]}),
        );
        let id = p["ids"][0].as_u64().unwrap();
        assert_eq!(ed.scene().objects[0].camera.as_ref().unwrap().fov, 36.);
        assert_eq!(
            call(
                &mut ed,
                "POST",
                &format!("/proposal/accept?id={id}"),
                json!({})
            )
            .0,
            200
        );
        assert_eq!(
            path_job(&ed, "camera=Shot&w=8&h=8")
                .unwrap_or_else(|r| panic!("unexpected response: {}", r.status))
                .camera
                .fov,
            60.
        );
        assert_eq!(call(&mut ed, "POST", "/undo", json!({})).0, 200);
        assert_eq!(
            path_job(&ed, "camera=Shot&w=8&h=8")
                .unwrap_or_else(|r| panic!("unexpected response: {}", r.status))
                .camera
                .fov,
            36.
        );
        let (_, context) = call(&mut ed, "GET", "/context", Value::Null);
        assert_eq!(context["objects"][0]["camera"]["focus"], 10.);
    }

    use super::*;

    #[test]
    fn geometric_faces_replay_through_history_and_reject_missing_matches_atomically() {
        let mut ed = Editor::new();
        let (_, out) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[{"op":"add","name":"Cylinder","primitive":{"kind":"cylinder","segments":9}}]}),
        );
        let old_revision = out["revision"].as_u64().unwrap();
        let face =
            json!({"normal":[0,1,0],"centre":[0.2943407405,0.5,0.1071312683],"max_distance":0.1});
        let (status, out) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[{"op":"extrude","id":"Cylinder","face":face,"distance":0.4}]}),
        );
        assert_eq!(status, 200, "{out}");
        let before = ed.scene().clone();
        let (status, out) = call(
            &mut ed,
            "POST",
            "/history/revise",
            json!({"step":1,"commands":[{"op":"add","name":"Cylinder","primitive":{"kind":"cylinder","segments":12}}]}),
        );
        assert_eq!(status, 200, "{out}");
        assert!(ed.scene().revision > old_revision);
        assert_ne!(
            ed.scene().objects[0].mesh.faces.len(),
            before.objects[0].mesh.faces.len()
        );
        assert!(
            (ed.scene().objects[0]
                .mesh
                .vertices
                .iter()
                .map(|p| p[1])
                .fold(f64::NEG_INFINITY, f64::max)
                - 0.9)
                .abs()
                < 1e-6
        );
        let current = ed.scene().clone();
        let (status, out) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[{"op":"extrude","id":"Cylinder","face":{"normal":[0,1,0],"centre":[0,10,0],"max_distance":0.1},"distance":0.4}]}),
        );
        assert_eq!(status, 422, "{out}");
        assert!(out["error"].as_str().unwrap().contains("no matching"));
        assert_eq!(*ed.scene(), current);
        call(&mut ed, "POST", "/undo", json!({}));
        assert_eq!(ed.scene().objects, before.objects);
    }

    #[test]
    fn orientation_proposals_preview_and_accept_share_the_atomic_command_path() {
        let mut ed = Editor::new();
        let (status, out) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[
                {"op":"add_camera","name":"Shot"},
                {"op":"add_light","name":"Key","lamp":{"kind":"sun"},"rotation":[0.3,0.1,0.2]},
                {"op":"constrain","id":"Key","orientation":"Shot"}
            ]}),
        );
        assert_eq!(status, 200, "{out}");
        let before = ed.scene().clone();
        let (status, out) = call(
            &mut ed,
            "POST",
            "/proposals",
            json!({"title":"Turn the shot and its light","commands":[{"op":"transform","id":"Shot","rotation":[0.8,0.5,0.1]}]}),
        );
        assert_eq!(status, 200, "{out}");
        let id = out["ids"][0].as_u64().unwrap();
        let (status, preview) = call(&mut ed, "GET", &format!("/proposal?id={id}"), Value::Null);
        assert_eq!(status, 200, "{preview}");
        assert_eq!(*ed.scene(), before);
        assert_ne!(
            preview["scene"]["objects"][1]["transform"]["rotation"],
            json!(before.objects[1].transform.rotation)
        );
        let (status, out) = call(
            &mut ed,
            "POST",
            &format!("/proposal/accept?id={id}"),
            json!({}),
        );
        assert_eq!(status, 200, "{out}");
        assert_eq!(json!(ed.scene().objects), preview["scene"]["objects"]);
        call(&mut ed, "POST", "/undo", json!({}));
        assert_eq!(ed.scene().objects, before.objects);
        let before = ed.scene().clone();
        let (status, out) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[{"op":"constrain","id":"Shot","orientation":"Shot"}]}),
        );
        assert_eq!(status, 422, "{out}");
        assert_eq!(*ed.scene(), before);
    }

    #[test]
    fn stale_creations_return_actual_ids_and_conflicts_are_409() {
        let mut ed = Editor::new();
        call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[{"op":"add","name":"Base","primitive":{"kind":"cube"}}]}),
        );
        call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[{"op":"add_camera","name":"Alice"}]}),
        );
        let (status, out) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"expected_revision":1,"rebase":true,"actor":{"id":"bob","name":"Bob"},"commands":[{"op":"add_light","name":"Bob"}]}),
        );
        assert_eq!(status, 200, "{out}");
        assert_eq!(out["created"], json!([3]));
        assert_eq!(out["rebased_from"], 1);
        let before = ed.scene().clone();
        let (status, out) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"expected_revision":1,"rebase":true,"commands":[{"op":"add_camera","name":"Alice"}]}),
        );
        assert_eq!(status, 409, "{out}");
        assert!(out["error"].as_str().unwrap().contains("name"));
        assert_eq!(*ed.scene(), before);
        call(&mut ed, "POST", "/undo", json!({}));
        assert_eq!(ed.scene().objects.len(), 2);
    }

    #[test]
    fn rebased_commands_expose_context_and_history_but_keep_strict_conflicts() {
        let mut ed = Editor::new();
        assert_eq!(
            call(
                &mut ed,
                "POST",
                "/commands",
                json!({"commands":[
                    {"op":"add","name":"A","primitive":{"kind":"cube"}},
                    {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[3,0,0]}
                ]})
            )
            .0,
            200
        );
        assert_eq!(call(&mut ed, "POST", "/commands", json!({"expected_revision":1,"actor":{"id":"alice","name":"Alice"},"commands":[{"op":"transform","id":"A","translation":[1,0,0]}]})).0, 200);
        let (status, out) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"expected_revision":1,"rebase":true,"actor":{"id":"bob","name":"Bob"},"commands":[{"op":"material","id":"B","color":"#ff0000"}]}),
        );
        assert_eq!(status, 200, "{out}");
        assert_eq!(out["rebased_from"], 1);
        let (_, history) = call(&mut ed, "GET", "/history", Value::Null);
        assert_eq!(history["steps"][2]["rebased_from"], 1);
        assert_eq!(history["steps"][2]["actor"]["name"], "Bob");
        let before = ed.scene().clone();
        for rebase in [false, true] {
            let (status, out) = call(
                &mut ed,
                "POST",
                "/commands",
                json!({"expected_revision":1,"rebase":rebase,"commands":[{"op":"transform","id":"A","translation":[2,0,0]}]}),
            );
            assert_eq!(status, 409, "{out}");
            assert_eq!(out["detail"]["stale"], true);
            assert_eq!(*ed.scene(), before);
        }
        assert_eq!(call(&mut ed, "POST", "/undo", json!({})).0, 200);
        assert_eq!(ed.scene().objects[0].transform.translation[0], 1.);
        assert_ne!(ed.scene().objects[1].material.color, "#ff0000");
    }

    #[test]
    fn maintained_layout_proposals_undo_history_and_atomic_errors_share_one_path() {
        let mut ed = Editor::new();
        let (status, _) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[
                {"op":"add","name":"Anchor","primitive":{"kind":"cube"}},
                {"op":"add","name":"A","primitive":{"kind":"cube"},"translation":[-3,0,0]},
                {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[3,0,0]}
            ]}),
        );
        assert_eq!(status, 200);
        let command = json!({"op":"arrange","ids":["A","B"],"layout":"row","around":"Anchor","spacing":2,"keep":true});
        let (status, out) = call(
            &mut ed,
            "POST",
            "/proposals",
            json!({"title":"Keep the row", "commands":[command]}),
        );
        assert_eq!(status, 200, "{out}");
        let id = out["ids"][0].as_u64().unwrap();
        let (_, preview) = call(&mut ed, "GET", &format!("/proposal?id={id}"), Value::Null);
        assert_eq!(preview["scene"]["arrangements"][0]["layout"], "row");
        assert!(
            preview["proposal"]["diff"]["scene"]
                .as_array()
                .unwrap()
                .contains(&json!("arrangements"))
        );
        assert!(ed.scene().arrangements.is_empty());
        assert_eq!(
            call(
                &mut ed,
                "POST",
                &format!("/proposal/accept?id={id}"),
                json!({})
            )
            .0,
            200
        );
        assert_eq!(
            call(
                &mut ed,
                "POST",
                "/commands",
                json!({"commands":[{"op":"move","id":"Anchor","offset":[2,0,3]}]})
            )
            .0,
            200
        );
        assert_eq!(ed.scene().objects[1].transform.translation, [0.5, 0.5, 3.]);
        assert_eq!(call(&mut ed, "POST", "/undo", json!({})).0, 200);
        assert_eq!(ed.scene().objects[1].transform.translation, [-1.5, 0.5, 0.]);
        assert_eq!(call(&mut ed, "POST", "/redo", json!({})).0, 200);
        let mut revised = command;
        revised["spacing"] = json!(4);
        let (status, out) = call(
            &mut ed,
            "POST",
            "/history/revise",
            json!({"step":2,"commands":[revised]}),
        );
        assert_eq!(status, 200, "{out}");
        assert_eq!(ed.scene().objects[1].transform.translation, [-0.5, 0.5, 3.]);
        let (_, report) = call(&mut ed, "GET", "/inspect", Value::Null);
        assert!(
            !report["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["kind"] == "arrangement_violation")
        );
        let before = ed.scene().clone();
        let (status, _) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[{"op":"material","id":"A","color":"#ffffff"},{"op":"arrange","ids":["A","A"],"layout":"row","keep":true}]}),
        );
        assert_eq!(status, 422);
        assert_eq!(*ed.scene(), before);
        assert_eq!(
            call(
                &mut ed,
                "POST",
                "/commands",
                json!({"commands":[{"op":"unarrange","id":1}]})
            )
            .0,
            200
        );
        assert!(ed.scene().arrangements.is_empty());
    }

    #[test]
    fn alignment_commands_preview_undo_revise_and_reject_atomically() {
        let mut ed = Editor::new();
        let setup = json!([
            {"op":"add","name":"A","primitive":{"kind":"cube"},"translation":[-2,1,0]},
            {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[2,3,4]},
            {"op":"constrain","id":"B","align":"y","from":"A"}
        ]);
        let (status, _) = call(&mut ed, "POST", "/commands", json!({"commands":setup}));
        assert_eq!(status, 200);
        assert_eq!(ed.scene().objects[1].transform.translation, [2., 1., 4.]);
        let original = serde_json::to_value(ed.scene()).unwrap();
        let (status, out) = call(
            &mut ed,
            "POST",
            "/proposals",
            json!({"title":"Lift B", "commands":[{"op":"transform","id":"B","translation":[2,5,4]}]}),
        );
        assert_eq!(status, 200, "{out}");
        let id = out["ids"][0].as_u64().unwrap();
        let (_, preview) = call(&mut ed, "GET", &format!("/proposal?id={id}"), Value::Null);
        assert_eq!(
            preview["scene"]["objects"][0]["transform"]["translation"],
            json!([-2.0, 5.0, 0.0])
        );
        assert_eq!(serde_json::to_value(ed.scene()).unwrap(), original);
        assert_eq!(
            call(
                &mut ed,
                "POST",
                &format!("/proposal/accept?id={id}"),
                json!({})
            )
            .0,
            200
        );
        assert_eq!(ed.scene().objects[0].transform.translation[1], 5.);
        assert_eq!(call(&mut ed, "POST", "/undo", json!({})).0, 200);
        assert_eq!(ed.scene().objects[0].transform.translation[1], 1.);
        assert_eq!(call(&mut ed, "POST", "/redo", json!({})).0, 200);
        let mut revised = setup;
        revised[2]["align"] = json!("xz");
        let (status, out) = call(
            &mut ed,
            "POST",
            "/history/revise",
            json!({"step":1,"commands":revised}),
        );
        assert_eq!(status, 200, "{out}");
        assert_eq!(ed.scene().objects[0].transform.translation, [2., 1., 4.]);
        assert_eq!(ed.scene().objects[1].transform.translation, [2., 5., 4.]);
        let (_, report) = call(&mut ed, "GET", "/inspect", Value::Null);
        assert!(
            !report["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["kind"] == "constraint_violation")
        );
        let before = serde_json::to_value(ed.scene()).unwrap();
        let (status, _) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[{"op":"material","id":"A","color":"#ffffff"},{"op":"constrain","id":"B","align":"bad","from":"A"}]}),
        );
        assert_eq!(status, 400);
        assert_eq!(serde_json::to_value(ed.scene()).unwrap(), before);
        let (status, _) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[{"op":"constrain","id":"A","align":"y","from":"A"}]}),
        );
        assert_eq!(status, 422);
        assert_eq!(serde_json::to_value(ed.scene()).unwrap(), before);
    }

    #[test]
    fn distance_commands_preview_revise_inspect_and_reject_atomically() {
        let mut ed = Editor::new();
        let (status, _) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[
                {"op":"add","name":"A","primitive":{"kind":"cube"}},
                {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[3,0,0]},
                {"op":"constrain","id":"B","distance":2,"from":"A"}
            ]}),
        );
        assert_eq!(status, 200);
        let (status, out) = call(
            &mut ed,
            "POST",
            "/proposals",
            json!({"title":"Move A","commands":[{"op":"transform","id":"A","translation":[1,0,0]}]}),
        );
        assert_eq!(status, 200, "{out}");
        let id = out["ids"][0].as_u64().unwrap();
        let (status, preview) = call(&mut ed, "GET", &format!("/proposal?id={id}"), Value::Null);
        assert_eq!(status, 200);
        assert_eq!(
            preview["scene"]["objects"][1]["transform"]["translation"][0],
            3.0
        );
        assert_eq!(ed.scene().objects[1].transform.translation[0], 2.0);
        let (status, _) = call(
            &mut ed,
            "POST",
            &format!("/proposal/accept?id={id}"),
            json!({}),
        );
        assert_eq!(status, 200);
        let step = 1;
        let (status, out) = call(
            &mut ed,
            "POST",
            "/history/revise",
            json!({"step":step,"commands":[
                {"op":"add","name":"A","primitive":{"kind":"cube"}},
                {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[3,0,0]},
                {"op":"constrain","id":"B","distance":4,"from":"A"}
            ]}),
        );
        assert_eq!(status, 200, "{out}");
        assert_eq!(ed.scene().objects[1].transform.translation[0], 5.0);
        let (status, report) = call(&mut ed, "GET", "/inspect", Value::Null);
        assert_eq!(status, 200);
        assert!(
            !report["issues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["kind"] == "constraint_violation")
        );
        let before = serde_json::to_value(ed.scene()).unwrap();
        let (status, _) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands":[{"op":"constrain","id":"B","distance":-1,"from":"A"}]}),
        );
        assert_eq!(status, 422);
        assert_eq!(serde_json::to_value(ed.scene()).unwrap(), before);
    }

    #[test]
    fn chat_review_validates_without_mutating_scene_or_undo() {
        let mut ed = Editor::new();
        let mut batch: CommandBatch = serde_json::from_value(json!({"expected_revision":0,"source":"chat","commands":[{"op":"add","primitive":{"kind":"cube"}}]})).unwrap();
        let prompt = "提案".repeat(100);
        let reply = complete_chat(&mut ed, &prompt, &batch, ChatMode::Proposal).unwrap();
        assert_eq!(reply["mode"], "proposal");
        assert_eq!(reply["revision"], 0);
        assert_eq!(reply["created"], json!([]));
        assert!(ed.scene().objects.is_empty());
        assert!(!ed.can_undo());
        assert_eq!(ed.proposals()[0].title.chars().count(), 80);
        let id = reply["proposal_id"].as_u64().unwrap();
        let (status, _) = call(
            &mut ed,
            "POST",
            &format!("/proposal/accept?id={id}"),
            json!({}),
        );
        assert_eq!(status, 200);
        assert_eq!(ed.scene().objects.len(), 1);
        assert!(ed.can_undo());
        let version = ed.proposals_version();
        assert!(complete_chat(&mut ed, "stale", &batch, ChatMode::Proposal).is_err());
        assert_eq!(ed.proposals_version(), version);
        batch.expected_revision = Some(ed.scene().revision);
        batch.commands = serde_json::from_value(json!([{"op":"delete","id":999}])).unwrap();
        assert!(complete_chat(&mut ed, "invalid", &batch, ChatMode::Proposal).is_err());
        assert_eq!(ed.proposals_version(), version);
        assert_eq!(ed.scene().objects.len(), 1);
        let (status, _) = call(&mut ed, "POST", "/undo", json!({}));
        assert_eq!(status, 200);
        assert!(ed.scene().objects.is_empty());
    }

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
    fn the_history_is_listed_previewed_and_revised_over_http() {
        let mut ed = Editor::new();
        call(
            &mut ed,
            "POST",
            "/commands",
            json!({"source": "UI", "commands": [{"op": "add", "name": "Post", "primitive": {"kind": "cylinder", "height": 1}}]}),
        );
        call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands": [{"op": "add_image", "name": "Logo", "data": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, crate::image::tests::tiny_png())}]}),
        );
        call(
            &mut ed,
            "POST",
            "/commands",
            json!({"source": "agent", "commands": [{"op": "add", "name": "Cap", "primitive": {"kind": "sphere", "radius": 0.2}}, {"op": "place", "id": "Cap", "on": "Post"}]}),
        );
        let (s, h) = call(&mut ed, "GET", "/history", Value::Null);
        assert_eq!(s, 200);
        assert_eq!(
            (h["total"].as_u64(), h["from_loaded_scene"].as_bool()),
            (Some(3), Some(false))
        );
        assert_eq!(h["steps"][0]["source"], "UI");
        assert_eq!(h["steps"][1]["source"], "API");
        assert_eq!(h["steps"][1]["editable"], false);
        assert!(
            h["steps"][1]["commands"][0]["data"]
                .as_str()
                .unwrap()
                .contains("image data")
        );
        assert_eq!(h["steps"][2]["step"], 3);
        let (_, last) = call(&mut ed, "GET", "/history?limit=1", Value::Null);
        assert_eq!(last["steps"].as_array().unwrap().len(), 1);

        let taller = json!({"step": 1, "commands": [{"op": "add", "name": "Post", "primitive": {"kind": "cylinder", "height": 2}}]});
        let cap_y = |ed: &Editor| ed.scene().objects[1].transform.translation[1];
        let before = cap_y(&ed);
        let (s, p) = call(&mut ed, "POST", "/history/preview", taller.clone());
        assert_eq!(s, 200);
        assert!(
            p["scene"]["objects"][1]["transform"]["translation"][1]
                .as_f64()
                .unwrap()
                > before + 0.4
        );
        assert_eq!(cap_y(&ed), before, "a preview changes nothing");
        let r = handle(
            &mut ed,
            "POST",
            "/history/revise",
            &serde_json::to_vec(&taller).unwrap(),
            false,
        );
        assert_eq!((r.status, r.changed), (200, Some(ed.scene().revision)));
        assert!(
            cap_y(&ed) > before + 0.4,
            "the cap is placed on the taller post again"
        );
        let bad = json!({"step": 1, "commands": [{"op": "add", "name": "Pole", "primitive": {"kind": "cylinder"}}]});
        let (s, e) = call(&mut ed, "POST", "/history/revise", bad);
        assert_eq!(s, 422);
        assert!(e["error"].as_str().unwrap().contains("step 3"), "{e}");
    }

    #[test]
    fn proposals_are_previewed_rendered_and_decided_over_http() {
        let mut ed = Editor::new();
        call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands": [{"op": "add", "name": "Vase", "primitive": {"kind": "cylinder"}}]}),
        );
        let (s, r) = call(
            &mut ed,
            "POST",
            "/proposals",
            json!({"title": "Options", "variants": [
                {"title": "Blue", "commands": [{"op": "material", "id": "Vase", "color": "#2f4f8f"}]},
                {"title": "With a cup", "commands": [{"op": "add", "name": "Cup", "primitive": {"kind": "cube"}, "translation": [1, 0.5, 0]}]}
            ]}),
        );
        assert_eq!(s, 200, "{r}");
        assert_eq!(r["ids"], json!([1, 2]));
        assert_eq!(r["pending"][1]["diff"]["added"][0]["name"], "Cup");
        let (_, st) = call(&mut ed, "GET", "/state", Value::Null);
        assert_eq!(st["proposals"]["pending"].as_array().unwrap().len(), 2);
        let (s, p) = call(&mut ed, "GET", "/proposal?id=2", Value::Null);
        assert_eq!(s, 200);
        assert_eq!(p["scene"]["objects"].as_array().unwrap().len(), 2);
        assert_eq!(p["proposal"]["title"], "Options · With a cup");
        let png = handle(
            &mut ed,
            "GET",
            "/proposal/render?id=2&w=32&h=24&samples=2&eye=2,2,4&target=0,0.5,0",
            &[],
            false,
        );
        assert_eq!((png.status, png.content_type), (200, "image/png"));
        assert_eq!(call(&mut ed, "GET", "/proposal?id=9", Value::Null).0, 404);
        let accepted = handle(&mut ed, "POST", "/proposal/accept?id=2", &[], false);
        assert_eq!(accepted.status, 200);
        assert_eq!(accepted.changed, Some(ed.scene().revision));
        assert_eq!(ed.scene().objects.len(), 2);
        let (_, list) = call(&mut ed, "GET", "/proposals", Value::Null);
        assert!(list["pending"].as_array().unwrap().is_empty());
        assert_eq!(list["decided"][0]["outcome"], "accepted");
        assert_eq!(list["decided"][1]["outcome"], "superseded");
        assert_eq!(
            call(&mut ed, "POST", "/proposal/reject?id=1", Value::Null).0,
            422
        );
    }

    #[test]
    fn the_environment_comes_with_its_sun_split_off() {
        let mut ed = Editor::new();
        let header = |r: &Response| -> Vec<f32> {
            let n = |i: usize| u32::from_le_bytes(r.body[i * 4..i * 4 + 4].try_into().unwrap());
            let f = |i: usize| f32::from_le_bytes(r.body[i * 4..i * 4 + 4].try_into().unwrap());
            let (w, h) = (n(0), n(1));
            assert_eq!(r.body.len(), 32 + (w * h * 16) as usize);
            vec![w as f32, h as f32, f(2), f(3), f(4), f(5), f(6), f(7)]
        };
        // The studio sends its dome, without a sun.
        let r = handle(&mut ed, "GET", "/environment?w=64", &[], false);
        assert_eq!(r.status, 200);
        let h = header(&r);
        assert_eq!((h[0], h[1]), (64.0, 32.0));
        assert!(h[5..].iter().all(|v| *v == 0.0));
        call(
            &mut ed,
            "POST",
            "/commands",
            json!({"commands": [{"op": "world", "sky": "daylight"}]}),
        );
        let h = header(&handle(&mut ed, "GET", "/environment?w=128", &[], false));
        assert_eq!((h[0], h[1]), (128.0, 64.0));
        assert!(h[3] > 0.5 && h[5] > 0.5, "the sun is high: {h:?}");
        assert_eq!(
            handle(&mut ed, "GET", "/environment?w=4", &[], false).status,
            400
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
    #[test]
    fn participant_attribution_survives_replay_and_stale_edits_are_atomic() {
        let mut ed = Editor::new();
        let actor = json!({"id":"alice", "name":"Alice"});
        let commands = json!([{"op":"add", "primitive":{"kind":"cube"}}]);
        let (status, _) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"actor":actor,"commands":commands,"expected_revision":0}),
        );
        assert_eq!(status, 200);
        let (status, error) = call(
            &mut ed,
            "POST",
            "/commands",
            json!({"actor":{"id":"bob","name":"Bob"},"commands":[{"op":"clear"}],"expected_revision":0}),
        );
        assert_eq!(status, 409);
        assert_eq!(error["detail"]["stale"], true);
        assert_eq!(ed.scene().objects.len(), 1);
        assert_eq!(
            call(
                &mut ed,
                "POST",
                "/history/revise",
                json!({"step":1,"commands":commands})
            )
            .0,
            200
        );
        let (stale, _) = call(
            &mut ed,
            "POST",
            "/history/revise",
            json!({"step":1,"commands":commands,"expected_revision":1}),
        );
        assert_eq!(stale, 409);
        ed.undo().unwrap();
        ed.redo().unwrap();
        let (_, history) = call(&mut ed, "GET", "/history", Value::Null);
        assert_eq!(history["steps"][0]["actor"], actor);
        let before = ed.scene().revision;
        assert_eq!(
            call(
                &mut ed,
                "POST",
                "/commands",
                json!({"actor":{"id":"bad","name":""},"commands":[{"op":"clear"}]})
            )
            .0,
            422
        );
        assert_eq!(ed.scene().revision, before);
    }
    #[test]
    fn orthographic_render_queries_sample_height_and_reject_ineffective_fov_overrides() {
        let mut ed = Editor::new();
        assert_eq!(call(&mut ed,"POST","/commands",json!({"commands":[
            {"op":"add_camera","name":"Ortho","translation":[0,1,4],"lens":{"ortho_height":4}},
            {"op":"set_keyframe","id":"Ortho","property":"camera_height","frame":1,"value":2,"interpolation":"linear"},
            {"op":"set_keyframe","id":"Ortho","property":"camera_height","frame":3,"value":6},
            {"op":"add","primitive":{"kind":"cube"}}
        ]})).0,200);
        assert_eq!(
            path_job(&ed, "camera=Ortho&frame=2&w=16&h=8")
                .unwrap_or_else(|r| panic!("{}", r.status))
                .camera
                .ortho_height,
            Some(4.)
        );
        assert_eq!(
            path_job(&ed, "camera=Ortho&ortho_height=3&w=16&h=8")
                .unwrap_or_else(|r| panic!("{}", r.status))
                .camera
                .ortho_height,
            Some(3.)
        );
        assert!(path_job(&ed, "camera=Ortho&fov=40&w=16&h=8").is_err());
        assert_eq!(
            path_job(&ed, "eye=0,1,4&target=0,1,0&ortho_height=2&w=16&h=8")
                .unwrap_or_else(|r| panic!("{}", r.status))
                .camera
                .ortho_height,
            Some(2.)
        );
        for h in ["0", "-1", "NaN", "10001"] {
            assert!(
                path_job(
                    &ed,
                    &format!("eye=0,1,4&target=0,1,0&ortho_height={h}&w=16&h=8")
                )
                .is_err()
            );
        }
        assert_eq!(
            image_job(&ed, "camera=Ortho&frames=1-3&w=8&h=8&samples=1")
                .unwrap_or_else(|r| panic!("{}", r.status))
                .run()
                .status,
            200
        );
        assert_eq!(
            image_job(&ed, "view=front&ortho_height=2&w=8&h=8&samples=1")
                .unwrap_or_else(|r| panic!("{}", r.status))
                .run()
                .status,
            200
        );
    }
    #[test]
    fn spot_api_validates_settings_renders_and_undoes_one_typed_batch() {
        let mut ed = Editor::new();
        assert_eq!(call(&mut ed,"POST","/commands",json!({"commands":[
            {"op":"add_light","name":"Spot","translation":[0,2,3],"rotation":[-0.4,0,0],"lamp":{"kind":"spot","inner_cone":0.2,"outer_cone":0.8,"range":8,"intensity":80}},
            {"op":"add","primitive":{"kind":"cube"}}]})).0,200);
        let before = ed.scene().clone();
        assert_eq!(call(&mut ed,"POST","/commands",json!({"commands":[{"op":"move","id":"Spot","offset":[1,0,0]},{"op":"light_settings","id":"Spot","lamp":{"kind":"spot","inner_cone":0.8,"outer_cone":0.8}}]})).0,422);
        assert_eq!(*ed.scene(), before);
        let image = image_job(&ed, "view=front&w=16&h=16&samples=2")
            .unwrap_or_else(|r| panic!("{}", r.status))
            .run();
        assert_eq!(image.status, 200);
        assert_eq!(call(&mut ed,"POST","/commands",json!({"commands":[{"op":"light_settings","id":"Spot","lamp":{"kind":"spot","inner_cone":0.3,"outer_cone":0.9,"range":10,"intensity":20}}]})).0,200);
        assert_eq!(call(&mut ed, "POST", "/undo", json!({})).0, 200);
        let mut restored = before;
        restored.revision += 2; // The edit and Undo each advance the API revision.
        assert_eq!(*ed.scene(), restored);
    }
}
