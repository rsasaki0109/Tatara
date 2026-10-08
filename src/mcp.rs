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
    json!([
        {
            "name": "get_scene",
            "description": "Read the shared Tatara scene: object IDs, names, transforms, materials, world bounds, counts and revision. Set include_mesh to also get vertices and polygon indices.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "include_mesh": { "type": "boolean", "default": false },
                    "frame": { "type": "number", "description": "Also report each animated object's pose at this frame" }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "apply_commands",
            "description": "Apply an atomic batch of modeling commands to the shared scene (one undo step). Pass expected_revision from get_scene to reject stale edits. Units are meters, Y up, rotations in radians. Sculpt with `sculpt` brush strokes (draw, inflate, smooth, flatten, grab) on a `quadsphere`. Materials take a preset (glass, gold, chrome, neon, jade, ...) plus colour, roughness, metalness, emissive, opacity and transmission.",
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
            "name": "render_view",
            "description": "Look at the shared scene: returns a PNG rendered from one or more labelled camera views (tiled two per row), with shadows, outlines, see-through glass, glowing emissive surfaces and a 1 m ground grid. Use it after editing to check proportions, placement and intersections.",
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
                    "frame": { "type": "number", "description": "Render animated objects as posed at this frame" }
                },
                "additionalProperties": false
            }
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
                "instructions": "Tatara is a 3D editor. Call get_scene first, then apply_commands. Edits appear live in the user's browser and are undoable."
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
    if name == "import_gltf" || name == "export_gltf" {
        return file_tool(http, base, name, &args).await;
    }
    let request = match name {
        "get_scene" if args["include_mesh"].as_bool() == Some(true) => {
            http.get(format!("{base}/api/scene"))
        }
        "get_scene" => match args["frame"].as_f64() {
            Some(f) => http
                .get(format!("{base}/api/context"))
                .query(&[("frame", f.to_string())]),
            None => http.get(format!("{base}/api/context")),
        },
        "apply_commands" => http.post(format!("{base}/api/commands")).json(&args),
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
