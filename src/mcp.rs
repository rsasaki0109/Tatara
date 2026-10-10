//! Stdio MCP bridge. It forwards tool calls to the running editor, so an
//! agent edits the same scene that is open in the browser.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::engine;

const PROTOCOL_VERSION: &str = "2025-06-18";

pub async fn run(base_url: String) -> anyhow::Result<()> {
    let http = reqwest::Client::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&http, &base_url, msg).await,
            Err(e) => Some(json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": -32700, "message": format!("parse error: {e}") }
            })),
        };
        if let Some(reply) = reply {
            stdout.write_all(format!("{reply}\n").as_bytes()).await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

pub fn tools() -> Value {
    let mut batch_schema = engine::command_schema();
    if let Some(obj) = batch_schema.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
    }
    let mut propose_schema =
        serde_json::to_value(schemars::schema_for!(crate::proposal::ProposalRequest))
            .expect("schema serializes");
    if let Some(obj) = propose_schema.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
    }
    let presence_schema =
        serde_json::to_value(schemars::schema_for!(crate::collaboration::Presence))
            .expect("schema serializes");
    json!([
        { "name": "get_presence", "description": "Read participants in this local shared session: their selections, cursors, camera and advisory editing locks. Presence expires after 30 seconds without a heartbeat.", "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false } },
        { "name": "update_presence", "description": "Join or heartbeat a shared session with actor {id,name}, optional selection, cursor (0..1), camera {eye,target,fov, optional up/aspect/ortho_height} and editing object IDs. Heartbeat every 5-10 seconds; editing is advisory, not an exclusive lock. Pass the same actor to apply_commands to attribute edits, and expected_revision to prevent lost updates.", "inputSchema": presence_schema },
        { "name": "leave_presence", "description": "Leave the session and release your advisory editing locks.", "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"], "additionalProperties": false } },
        {
            "name": "get_scene",
            "description": "Read the shared Tatara scene: object IDs, names, transforms, materials, world bounds, counts and revision. Set include_mesh to also get vertices and polygon indices.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "include_mesh": { "type": "boolean", "default": false },
                    "frame": { "type": "number", "description": "Report every object's animation and constraint-solved pose at this frame, including unkeyed followers" }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "apply_commands",
            "description": "Apply an atomic batch of modeling commands to the shared scene (one undo step). Pass expected_revision from get_scene to guard the context. With rebase: true, supported existing-object edits are reapplied if every object/group and connected constraint dependency is unchanged; conflicts still fail with a recovery reason. Creation-only batches of add, add_camera and add_light can also rebase, returning current IDs in created and rejecting ambiguous object/group names. Mixed creation/edit batches, other creation commands, scene-wide commands and maintained layouts need a fresh context. Key camera_fov, camera_aperture and camera_focus on cameras, light_color and light_intensity on lights; get_scene with frame and render_image sample optics. Light colour keys accept #rrggbb or sRGB triples. Units are meters, Y up, rotations in radians. Lay scenes out by relation instead of coordinates: `build` furniture (table, chair, lamp, mug, plant, shelf), `place` it on or beside something, `arrange` items in a row, grid or circle (facing the centre); `keep: true` maintains the layout after later edits, following `around` or keeping a fixed `center`, and `unarrange` removes it by id from scene `arrangements`. Members snap back to the layout; deleted members leave it and a deleted reference drops it. Cut, merge or intersect shapes with `boolean`. Rig a mesh with `rig` (`chain`: that many bones end to end through it, or explicit `bones` with head, tail and parent), turn bones with `pose` or bend a chain so a bone's tip reaches a world point with `reach` (inverse kinematics; add `frame` to key it), and animate them with `set_keyframe` property `bone` plus the `bone` name; the mesh bends with automatic weights and exports as a glTF skin (give cylinders `rings` so they can bend). Sculpt with `sculpt` brush strokes (draw, inflate, smooth, flatten, grab) on a `quadsphere`; give `detail` (an edge length) to add faces only where the brush goes (dynamic topology), so a coarse ball can take fine features. Materials take a preset (glass, gold, chrome, neon, jade, wood, marble, brick, tiles, ...) plus colour, roughness, metalness, emissive, opacity, transmission and a `texture`: a pattern (wood, marble, brick, tiles, checker, stripes; color2; scale = metres per tile) or a scene image (`add_image` with base64 PNG/JPEG, then pattern \"image\" with `image` and `fit: true` to cover each side once), plus `relief` (0-1 bumps from the pattern) or a `normal_map` image. Pattern \"nodes\" takes a `graph`: `nodes` (noise, voronoi, pattern, gradient, image, mix, math, ramp; inputs are numbers, `#rrggbb` or `{\"node\": id}`) and an `output` with color, roughness, metalness and height. Keep intent with `constrain`: `on` keeps an object or group resting on another (it follows the support; moving it keeps its new spot), `mirrors` keeps it the mirror image of another across X = 0 (or `axis: \"z\"`; either side leads), `matches` keeps its material the same as another's; `distance` with `from` keeps evaluated world-bound centres that many metres apart (either edited side leads; both edited makes `from` lead); `align` with `from` keeps evaluated centres equal on selected world axes (x, y, z, xy, xz, yz, xyz), leaving other axes free, with the same lead policy; `orientation` with an individual object reference captures their current relative rotation (also cameras/lights); turning either endpoint leads, or the referenced endpoint leads if both change. Positions and scales stay free; groups are rejected. `unconstrain` drops them. `inspect_scene` reports unsatisfied constraints and maintained arrangements. Spot lights use `lamp.kind: \"spot\"`, `inner_cone` and `outer_cone` half-angles in radians (0 <= inner < outer <= pi/2), optional positive `range`, and local -Z direction; ordinary point/spot lights accept range too. Light the scene with `world`: `sky` studio (default), daylight, sunset, overcast or night, or an `image` (an HDRI panorama added with `add_image`; Radiance .hdr keeps real light levels), with `strength`, `rotation` (degrees) and `background` (show it behind the scene in path-traced renders). Extrude and inset accept a numeric face index or a bounded local query {normal, centre, max_distance, min_dot}. Queries normalize the normal, choose the closest qualifying polygon and reject missing or ambiguous matches, including during history replay. Faces are box-projected by default; to map an image exactly, `unwrap` the mesh (method smart, cube, cylinder, or seams after `mark_seams` cuts edges) and move islands with `transform_uvs` (offset, rotate, scale about each island centre). Imported meshes keep their UVs.",
            "inputSchema": batch_schema
        },
        {
            "name": "undo",
            "description": "Undo the last change in the shared scene.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "redo",
            "description": "Redo the last undone change in the shared scene.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "import_gltf",
            "description": "Import a local .glb (or .gltf with embedded buffers) file into the shared scene as one undoable step. Triangles are welded and merged back into quads.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Path to a .glb or .gltf file" } },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "export_gltf",
            "description": "Write the shared scene, with modifiers applied, to a local binary glTF (.glb) file.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string", "description": "Destination path ending in .glb" } },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "render_image",
            "description": "Render a finished, path-traced image of the shared scene to a local PNG: realistic light, glass, reflections, soft shadows and glow, denoised, over the studio backdrop or transparent. Frame it with a scene `camera` (ID or exact name, using its stored lens and animated transform), a `view` (like render_view; default iso) or an explicit `eye` and `target`; give the lens an `aperture` (radius in metres, e.g. 0.05) for depth of field, focused at `focus` metres (default: the target's distance). With `frames` [start, end] it writes an animated PNG of that range. Slow at high sizes and samples.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Destination path ending in .png" },
                    "camera": { "oneOf": [{ "type": "integer", "minimum": 1 }, { "type": "string" }], "description": "Scene camera ID or exact name; samples its transform per frame and uses its lens" },
                    "view": { "type": "string", "description": "front, back, left, right, top, bottom, iso or \"azimuth:elevation\"; ignored with eye/target" },
                    "eye": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": "Camera position (world, metres)" },
                    "target": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": "Point the camera looks at" },
                    "up": {"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3,"description":"Free-view camera up vector, for roll"},
                    "size": { "type": "array", "items": { "type": "integer", "minimum": 8, "maximum": 4096 }, "minItems": 2, "maxItems": 2, "description": "[width, height] in pixels, default [1280, 720]" },
                    "ortho_height": {"type":"number","minimum":0.001,"maximum":10000,"description":"Orthographic vertical view height in metres; omit fov"},
                    "fov": { "type": "number", "minimum": 1, "maximum": 170, "description": "Vertical field of view in degrees (default 36)" },
                    "samples": { "type": "integer", "minimum": 1, "maximum": 4096, "description": "Samples per pixel (default 128)" },
                    "aperture": { "type": "number", "minimum": 0, "maximum": 1, "description": "Lens radius in metres for depth of field (0: everything sharp)" },
                    "focus": { "type": "number", "minimum": 0, "description": "Focus distance in metres (default: distance to target)" },
                    "frame": { "type": "number", "description": "Sample animation and solve persistent constraints at this frame" },
                    "frames": { "type": "array", "items": { "type": "number" }, "minItems": 2, "maxItems": 2, "description": "[start, end]: an animated PNG of that range (at most 240 frames)" },
                    "background": { "type": "string", "enum": ["studio", "transparent"], "description": "Default studio" }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "render_view",
            "description": "Look at the shared scene: returns a PNG rendered from one or more labelled camera views (tiled two per row), with shadows, outlines, see-through glass, glowing emissive surfaces and a 1 m ground grid; pass `samples` to path trace it instead. Use it after editing to check proportions, placement and intersections.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "views": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Up to 6 of: front, back, left, right, top, bottom, iso, or \"azimuth:elevation\" in degrees (azimuth 0 looks from +Z). Default [\"iso\"].",
                        "maxItems": 6
                    },
                    "size": { "type": "integer", "minimum": 64, "maximum": 1024, "default": 512, "description": "Pixel size of each view" },
                    "object": { "type": "string", "description": "Frame one object (name or numeric id) instead of the whole scene" },
                    "frame": { "type": "number", "description": "Render the animation and constraint-solved scene at this frame" },
                    "samples": { "type": "integer", "minimum": 1, "maximum": 256, "description": "Path trace with this many samples per pixel for realistic light: soft shadows, reflections, refraction through glass and light cast by glowing surfaces. Slower; 16-64 is a good preview." }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "propose_changes",
            "description": "Offer changes for the person to review instead of applying them: they see exactly what would be added, changed and removed, preview it in the viewport and accept or reject it. Give `commands` (the same commands as apply_commands) for one proposal, or `variants` (each with a title and commands) to let them pick one of several options; accepting one drops the others. Use this for big or taste-dependent changes (layouts, looks, deletions) and when asked for options. Returns the proposal ids; call list_proposals later to see what was decided.",
            "inputSchema": propose_schema
        },
        {
            "name": "get_history",
            "description": "Read how the scene was made: the batches applied so far, oldest first, each numbered (`step`) with its source (UI, agent, chat, proposal, ...), actor, commands and optional `rebased_from` for an edit combined with newer changes. Use it to find the step that set a value you want to change, then revise_step it.",
            "inputSchema": {
                "type": "object",
                "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "Only the last this many steps (default 100)" } },
                "additionalProperties": false
            }
        },
        {
            "name": "revise_step",
            "description": "Change an earlier step and replay everything after it, like editing a parametric history: e.g. rebuild the table taller and anything later placed on it ends up on the new top. Give the step number from get_history and its new `commands` (the whole batch). One undo step; refused, with the step that breaks, if a later step no longer applies.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "step": { "type": "integer", "minimum": 1 },
                    "expected_revision": { "type": "integer", "minimum": 0, "description": "Reject when the scene changed since reading history" },
                    "commands": { "type": "array", "items": { "type": "object" }, "description": "The step's new commands (same format as apply_commands)" }
                },
                "required": ["step", "commands"],
                "additionalProperties": false
            }
        },
        {
            "name": "list_proposals",
            "description": "List proposals waiting for review (with what each would change, and a conflict if the scene moved on so it no longer applies) and recent decisions: accepted, rejected, or superseded by another variant.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "inspect_scene",
            "description": "Measure the shared scene and list problems a picture can hide: objects that intersect (with depth), float above what is beneath them (with gap) or sink below the floor (y = 0), plus every object's world bounds, size and what it rests on. Call it after building; fix floating or sunk objects with the `drop` command.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }
    ])
}

async fn handle(http: &reqwest::Client, base: &str, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    // Notifications carry no id and get no reply.
    let id = id?;
    let result = match method {
        "initialize" => {
            let version = msg["params"]["protocolVersion"]
                .as_str()
                .unwrap_or(PROTOCOL_VERSION);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "tatara", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Tatara is a 3D editor. Call get_scene first, then apply_commands. Edits appear live in the user's browser and are undoable. For big or taste-dependent changes, or when asked for options, use propose_changes so the person can preview and accept them."
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
            Ok(call_tool(http, base, name, args).await)
        }
        _ => Err(json!({ "code": -32601, "message": format!("method not found: {method}") })),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    })
}

fn has_extension(path: &str, allowed: &[&str]) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| allowed.iter().any(|a| e.eq_ignore_ascii_case(a)))
}

async fn file_tool(http: &reqwest::Client, base: &str, name: &str, args: &Value) -> Value {
    let Some(path) = args["path"].as_str().filter(|p| !p.is_empty()) else {
        return tool_result("path is required".into(), true);
    };
    let unreachable = |e: reqwest::Error| {
        tool_result(
            format!("Tatara editor is not reachable at {base} ({e})."),
            true,
        )
    };
    if name == "import_gltf" {
        if !has_extension(path, &["glb", "gltf"]) {
            return tool_result("path must end in .glb or .gltf".into(), true);
        }
        let bytes = match tokio::fs::read(path).await {
            Ok(b) => b,
            Err(e) => return tool_result(format!("cannot read {path}: {e}"), true),
        };
        return match http
            .post(format!("{base}/api/import"))
            .body(bytes)
            .send()
            .await
        {
            Ok(resp) => {
                let ok = resp.status().is_success();
                tool_result(resp.text().await.unwrap_or_default(), !ok)
            }
            Err(e) => unreachable(e),
        };
    }
    if !has_extension(path, &["glb"]) {
        return tool_result("path must end in .glb".into(), true);
    }
    let bytes = match http.get(format!("{base}/api/export/glb")).send().await {
        Ok(resp) => match resp.bytes().await {
            Ok(b) => b,
            Err(e) => return unreachable(e),
        },
        Err(e) => return unreachable(e),
    };
    match tokio::fs::write(path, &bytes).await {
        Ok(()) => tool_result(format!("wrote {} bytes to {path}", bytes.len()), false),
        Err(e) => tool_result(format!("cannot write {path}: {e}"), true),
    }
}

async fn call_tool(http: &reqwest::Client, base: &str, name: &str, args: Value) -> Value {
    if name == "render_view" {
        return render_tool(http, base, &args).await;
    }
    if name == "render_image" {
        return image_tool(http, base, &args).await;
    }
    if name == "import_gltf" || name == "export_gltf" {
        return file_tool(http, base, name, &args).await;
    }
    let request = match name {
        "get_presence" => http.get(format!("{base}/api/presence")),
        "update_presence" => http.post(format!("{base}/api/presence")).json(&args),
        "leave_presence" => http.delete(format!("{base}/api/presence")).json(&args),
        "get_scene" if args["include_mesh"].as_bool() == Some(true) => {
            http.get(format!("{base}/api/scene"))
        }
        "get_scene" => match args["frame"].as_f64() {
            Some(f) => http
                .get(format!("{base}/api/context"))
                .query(&[("frame", f.to_string())]),
            None => http.get(format!("{base}/api/context")),
        },
        "apply_commands" => {
            let mut args = args;
            if args.get("source").is_none() {
                args["source"] = json!("agent");
            }
            http.post(format!("{base}/api/commands")).json(&args)
        }
        "get_history" => match args["limit"].as_u64() {
            Some(n) => http
                .get(format!("{base}/api/history"))
                .query(&[("limit", n.to_string())]),
            None => http.get(format!("{base}/api/history")),
        },
        "revise_step" => http.post(format!("{base}/api/history/revise")).json(&args),
        "propose_changes" => {
            let mut args = args;
            if args.get("author").is_none() {
                args["author"] = json!("agent");
            }
            http.post(format!("{base}/api/proposals")).json(&args)
        }
        "list_proposals" => http.get(format!("{base}/api/proposals")),
        "inspect_scene" => http.get(format!("{base}/api/inspect")),
        "undo" => http.post(format!("{base}/api/undo")),
        "redo" => http.post(format!("{base}/api/redo")),
        _ => return tool_result(format!("unknown tool: {name}"), true),
    };
    match request.send().await {
        Ok(resp) => {
            let ok = resp.status().is_success();
            let text = resp.text().await.unwrap_or_default();
            let pretty = serde_json::from_str::<Value>(&text)
                .map(|v| serde_json::to_string_pretty(&v).unwrap_or(text.clone()))
                .unwrap_or(text);
            tool_result(pretty, !ok)
        }
        Err(e) => tool_result(
            format!(
                "Tatara editor is not reachable at {base} ({e}). Start it with `cargo run --release`, or set TATARA_URL."
            ),
            true,
        ),
    }
}

/// `render_image`: ask the editor for a final render and write it to disk.
async fn image_tool(http: &reqwest::Client, base: &str, args: &Value) -> Value {
    let Some(path) = args["path"].as_str().filter(|p| !p.is_empty()) else {
        return tool_result("path is required".into(), true);
    };
    if !has_extension(path, &["png"]) {
        return tool_result("path must end in .png".into(), true);
    }
    let triple = |v: &Value| -> Option<String> {
        let a = v.as_array()?;
        if a.len() != 3 {
            return None;
        }
        let n: Vec<f64> = a.iter().map(Value::as_f64).collect::<Option<Vec<_>>>()?;
        (n.len() == 3).then(|| format!("{},{},{}", n[0], n[1], n[2]))
    };
    let size = args["size"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_u64).collect::<Vec<_>>());
    let (w, h) = match size.as_deref() {
        Some([w, h]) => (*w, *h),
        None => (1280, 720),
        _ => return tool_result("size must be [width, height]".into(), true),
    };
    let mut query = vec![
        ("w", w.to_string()),
        ("h", h.to_string()),
        (
            "samples",
            args["samples"].as_u64().unwrap_or(128).to_string(),
        ),
    ];
    if args.get("camera").is_some() && (args.get("eye").is_some() || args.get("target").is_some()) {
        return tool_result("give a scene camera or eye/target, not both".into(), true);
    }
    match (triple(&args["eye"]), triple(&args["target"])) {
        (Some(eye), Some(target)) => {
            query.push(("eye", eye));
            query.push(("target", target));
        }
        (None, None) if args.get("camera").is_some() => {
            let id = match &args["camera"] {
                Value::String(n) => n.clone(),
                v if v.as_u64().is_some() => v.as_u64().unwrap().to_string(),
                _ => return tool_result("camera must be an ID or exact name".into(), true),
            };
            query.push(("camera", id));
        }
        (None, None) => {
            query.push(("view", args["view"].as_str().unwrap_or("iso").to_string()));
        }
        _ => return tool_result("give both eye and target, or neither".into(), true),
    }
    if args.get("up").is_some() {
        if args.get("camera").is_some() {
            return tool_result(
                "change the scene camera rotation to change its up vector".into(),
                true,
            );
        }
        let Some(up) = triple(&args["up"]) else {
            return tool_result("up must be [x,y,z]".into(), true);
        };
        query.push(("up", up));
    }
    for key in ["fov", "aperture", "focus", "ortho_height", "frame"] {
        if let Some(v) = args[key].as_f64() {
            query.push((key, v.to_string()));
        }
    }
    if let Some([a, b]) = args["frames"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect::<Vec<_>>())
        .as_deref()
    {
        query.push(("frames", format!("{a}-{b}")));
    }
    if let Some(b) = args["background"].as_str() {
        query.push(("background", b.to_string()));
    }
    let resp = match http
        .get(format!("{base}/api/render/image"))
        .query(&query)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return tool_result(
                format!("Tatara editor is not reachable at {base} ({e})."),
                true,
            );
        }
    };
    let ok = resp.status().is_success();
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => return tool_result(format!("render failed: {e}"), true),
    };
    if !ok {
        return tool_result(String::from_utf8_lossy(&bytes).into_owned(), true);
    }
    match tokio::fs::write(path, &bytes).await {
        Ok(()) => tool_result(
            format!("wrote a {w}x{h} render ({} bytes) to {path}", bytes.len()),
            false,
        ),
        Err(e) => tool_result(format!("cannot write {path}: {e}"), true),
    }
}

async fn render_tool(http: &reqwest::Client, base: &str, args: &Value) -> Value {
    let views: Vec<String> = match &args["views"] {
        Value::Array(list) => list
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        Value::String(s) => vec![s.clone()],
        _ => vec!["iso".into()],
    };
    let views = if views.is_empty() {
        vec!["iso".to_string()]
    } else {
        views
    };
    let mut query = vec![("views", views.join(","))];
    if let Some(size) = args["size"].as_u64() {
        query.push(("size", size.to_string()));
    }
    if let Some(object) = args["object"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| args["object"].as_u64().map(|v| v.to_string()))
    {
        query.push(("object", object));
    }
    if let Some(frame) = args["frame"].as_f64() {
        query.push(("frame", frame.to_string()));
    }
    if let Some(samples) = args["samples"].as_u64() {
        query.push(("samples", samples.to_string()));
    }
    let resp = match http
        .get(format!("{base}/api/render"))
        .query(&query)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return tool_result(
                format!("Tatara editor is not reachable at {base} ({e})."),
                true,
            );
        }
    };
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        return tool_result(text, true);
    }
    let png = resp.bytes().await.unwrap_or_default();
    let summary = match http.get(format!("{base}/api/context")).send().await {
        Ok(r) => r.json::<Value>().await.ok(),
        Err(_) => None,
    };
    let caption = match summary {
        Some(ctx) => format!(
            "Rendered {} at revision {}. Objects: {}.",
            views.join(", "),
            ctx["revision"],
            ctx["objects"]
                .as_array()
                .map(|o| o
                    .iter()
                    .map(|o| format!("{} (#{})", o["name"].as_str().unwrap_or("?"), o["id"]))
                    .collect::<Vec<_>>()
                    .join(", "))
                .unwrap_or_default()
        ),
        None => format!("Rendered {}.", views.join(", ")),
    };
    use base64::Engine as _;
    json!({
        "content": [
            { "type": "image", "data": base64::engine::general_purpose::STANDARD.encode(&png), "mimeType": "image/png" },
            { "type": "text", "text": caption }
        ],
        "isError": false
    })
}

fn tool_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}
