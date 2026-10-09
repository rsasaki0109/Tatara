//! End-to-end checks of the HTTP API, the MCP bridge and chat with a mock provider.

use std::process::Stdio;

use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use tatara::server;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn spawn(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

async fn editor(ai: Option<server::AiConfig>) -> String {
    spawn(server::router(server::state(ai), "web/dist".into())).await
}

#[tokio::test]
async fn commands_history_and_export() {
    let base = editor(None).await;
    let c = reqwest::Client::new();
    let r: Value = c
        .post(format!("{base}/api/commands"))
        .json(&json!({"commands": [
            {"op": "add", "name": "Box", "primitive": {"kind": "cube"}},
            {"op": "extrude", "id": "Box", "face": 4, "distance": 0.5}
        ]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["created"], json!([1]));
    assert_eq!(r["revision"], 1);

    let bad = c
        .post(format!("{base}/api/commands"))
        .json(&json!({"commands": [{"op": "delete", "id": 7}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 422);
    let stale = c
        .post(format!("{base}/api/commands"))
        .json(&json!({"commands": [{"op": "clear"}], "expected_revision": 0}))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), 409);

    let ctx: Value = c
        .get(format!("{base}/api/context"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ctx["objects"][0]["face_count"], 10);
    assert_eq!(ctx["bounds"]["max"][1], 1.0);

    let obj = c
        .get(format!("{base}/api/export/obj"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(obj.contains("o Box"));

    let saved: Value = c
        .get(format!("{base}/api/scene"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    c.post(format!("{base}/api/undo"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let st: Value = c
        .get(format!("{base}/api/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st["scene"]["objects"], json!([]));
    assert_eq!(st["history"]["can_redo"], true);

    c.put(format!("{base}/api/scene"))
        .json(&saved)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let st: Value = c
        .get(format!("{base}/api/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st["scene"]["objects"][0]["name"], "Box");
    assert_eq!(st["history"]["can_redo"], false);
}

#[tokio::test]
async fn chat_uses_mock_provider() {
    let provider = Router::new().route(
        "/v1/chat/completions",
        post(|Json(body): Json<Value>| async move {
            assert_eq!(body["model"], "mock");
            assert!(body["messages"][0]["content"].as_str().unwrap().contains("vessel"));
            let reply = "```json\n{\"commands\":[{\"op\":\"add\",\"name\":\"Vase\",\"primitive\":{\"kind\":\"vessel\",\"profile\":[[0.2,0],[0.3,0.4]]},\"color\":\"#3f7f5f\"}]}\n```";
            Json(json!({"choices": [{"message": {"role": "assistant", "content": reply}}]}))
        }),
    );
    let provider_url = spawn(provider).await;
    let base = editor(Some(server::AiConfig {
        base_url: format!("{provider_url}/v1"),
        model: "mock".into(),
        api_key: None,
    }))
    .await;
    let c = reqwest::Client::new();
    let r: Value = c
        .post(format!("{base}/api/chat"))
        .json(&json!({"prompt": "make a green vase"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["created"], json!([1]), "{r}");
    let ctx: Value = c
        .get(format!("{base}/api/context"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ctx["objects"][0]["material"]["color"], "#3f7f5f");

    let off = editor(None).await;
    let r = c
        .post(format!("{off}/api/chat"))
        .json(&json!({"prompt": "hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
}

#[tokio::test]
async fn mcp_bridge_edits_the_shared_scene() {
    let base = editor(None).await;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_tatara"))
        .arg("--mcp")
        .env("TATARA_URL", &base)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut call = async |msg: Value| -> Value {
        stdin
            .write_all(format!("{msg}\n").as_bytes())
            .await
            .unwrap();
        serde_json::from_str(&out.next_line().await.unwrap().unwrap()).unwrap()
    };
    let init = call(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}})).await;
    assert_eq!(init["result"]["serverInfo"]["name"], "tatara");
    let tools = call(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).await;
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 13);
    let r = call(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "apply_commands", "arguments": {"commands": [{"op": "add", "primitive": {"kind": "torus"}}]}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = call(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "get_scene", "arguments": {}}})).await;
    assert!(
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Torus")
    );
    let r = call(json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "undo", "arguments": {}}})).await;
    assert_eq!(r["result"]["isError"], false);
    let r = call(json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "undo", "arguments": {}}})).await;
    assert_eq!(r["result"]["isError"], true);

    // Files: export the (empty) scene, add a torus, export, then import it back.
    let dir = std::env::temp_dir().join(format!("tatara-mcp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("scene.glb");
    let path = path.to_str().unwrap();
    call(json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "redo", "arguments": {}}})).await;
    let r = call(json!({"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "export_gltf", "arguments": {"path": path}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = call(json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "import_gltf", "arguments": {"path": path}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = call(json!({"jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {"name": "import_gltf", "arguments": {"path": "/etc/passwd"}}})).await;
    assert_eq!(r["result"]["isError"], true);
    let r = call(json!({"jsonrpc": "2.0", "id": 11, "method": "tools/call", "params": {"name": "get_scene", "arguments": {}}})).await;
    let ctx: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(ctx["objects"].as_array().unwrap().len(), 2);
    assert_eq!(
        ctx["objects"][0]["face_count"],
        ctx["objects"][1]["face_count"]
    );
    // A final render goes to a file; a bad camera is reported, not written.
    let png = dir.join("render.png");
    let png = png.to_str().unwrap();
    let r = call(json!({"jsonrpc": "2.0", "id": 30, "method": "tools/call", "params": {"name": "render_image", "arguments": {"path": png, "size": [48, 27], "samples": 2, "aperture": 0.05}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert!(std::fs::read(png).unwrap().starts_with(b"\x89PNG"));
    let r = call(json!({"jsonrpc": "2.0", "id": 31, "method": "tools/call", "params": {"name": "render_image", "arguments": {"path": png, "eye": [0, 1, 4]}}})).await;
    assert_eq!(r["result"]["isError"], true);
    std::fs::remove_dir_all(dir).ok();

    // History: agent batches are marked, and an earlier step can be revised.
    let r = call(json!({"jsonrpc": "2.0", "id": 35, "method": "tools/call", "params": {"name": "get_history", "arguments": {"limit": 50}}})).await;
    let history: Value = serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let first = &history["steps"][0];
    assert_eq!(first["source"], "agent", "{history}");
    let step = first["step"].clone();
    let commands = first["commands"].clone();
    let r = call(json!({"jsonrpc": "2.0", "id": 36, "method": "tools/call", "params": {"name": "revise_step", "arguments": {"step": step, "commands": commands}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = call(json!({"jsonrpc": "2.0", "id": 37, "method": "tools/call", "params": {"name": "revise_step", "arguments": {"step": 999, "commands": commands}}})).await;
    assert_eq!(r["result"]["isError"], true);

    // Proposals: offered for review, listed back with the author.
    let r = call(json!({"jsonrpc": "2.0", "id": 32, "method": "tools/call", "params": {"name": "propose_changes", "arguments": {"title": "A lamp", "commands": [{"op": "add", "name": "Lamp", "primitive": {"kind": "sphere"}}]}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = call(json!({"jsonrpc": "2.0", "id": 33, "method": "tools/call", "params": {"name": "list_proposals", "arguments": {}}})).await;
    let listed: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(listed["pending"][0]["title"], "A lamp");
    assert_eq!(listed["pending"][0]["author"], "agent");
    let r = call(json!({"jsonrpc": "2.0", "id": 34, "method": "tools/call", "params": {"name": "propose_changes", "arguments": {"title": "Broken", "commands": [{"op": "delete", "id": "Nope"}]}}})).await;
    assert_eq!(r["result"]["isError"], true);

    // Vision: the agent gets a PNG back.
    let r = call(json!({"jsonrpc": "2.0", "id": 12, "method": "tools/call", "params": {"name": "render_view", "arguments": {"views": ["front", "top"], "size": 64}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert_eq!(r["result"]["content"][0]["type"], "image");
    assert_eq!(r["result"]["content"][0]["mimeType"], "image/png");
    use base64::Engine as _;
    let png = base64::engine::general_purpose::STANDARD
        .decode(r["result"]["content"][0]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(&png[1..4], b"PNG");
    assert!(
        r["result"]["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("Torus")
    );
    let r = call(json!({"jsonrpc": "2.0", "id": 13, "method": "tools/call", "params": {"name": "render_view", "arguments": {"views": ["sideways"]}}})).await;
    assert_eq!(r["result"]["isError"], true);

    // Measurements: the report lists every object and its issues.
    let r = call(json!({"jsonrpc": "2.0", "id": 14, "method": "tools/call", "params": {"name": "inspect_scene", "arguments": {}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let report: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(
        report["objects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["name"] == "Torus"),
        "{report}"
    );
    assert!(report["summary"].is_string());
    child.kill().await.unwrap();
}
