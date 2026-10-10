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

    let review: Value = c
        .post(format!("{base}/api/chat"))
        .json(&json!({"prompt":"another vase for review","mode":"proposal"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(review["mode"], "proposal");
    assert_eq!(review["revision"], r["revision"]);
    assert_eq!(review["created"], json!([]));
    let scene: Value = c
        .get(format!("{base}/api/scene"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(scene["objects"].as_array().unwrap().len(), 1);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_tatara"))
        .arg("--mcp")
        .env("TATARA_URL", &base)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
    for (id, method, params) in [
        (1, "tools/list", json!({})),
        (
            2,
            "tools/call",
            json!({"name":"list_proposals","arguments":{}}),
        ),
    ] {
        stdin
            .write_all(
                format!(
                    "{}\n",
                    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let response: Value =
            serde_json::from_str(&out.next_line().await.unwrap().unwrap()).unwrap();
        if id == 1 {
            assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 16);
        } else {
            let proposals: Value =
                serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                    .unwrap();
            assert_eq!(proposals["pending"][0]["id"], review["proposal_id"]);
            assert_eq!(proposals["pending"][0]["author"], "chat");
        }
    }
    drop(stdin);
    child.wait().await.unwrap();
    let id = review["proposal_id"].as_u64().unwrap();
    let accepted = c
        .post(format!("{base}/api/proposal/accept?id={id}"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), 200);
    c.post(format!("{base}/api/undo"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let restored: Value = c
        .get(format!("{base}/api/scene"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        restored["objects"].as_array().unwrap().len(),
        1,
        "acceptance is one undo step"
    );
    let invalid = c
        .post(format!("{base}/api/chat"))
        .json(&json!({"prompt":"vase","mode":"unknown"}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 400);

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
async fn chat_review_refuses_stale_and_invalid_provider_changes() {
    use std::sync::Arc;
    use tokio::sync::Notify;
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider = Router::new().route("/v1/chat/completions", post({
        let started = started.clone();
        let release = release.clone();
        move |Json(body): Json<Value>| {
            let started = started.clone();
            let release = release.clone();
            async move {
                let prompt = body["messages"][1]["content"].as_str().unwrap();
                let commands = if prompt == "delayed" {
                    started.notify_one();
                    release.notified().await;
                    json!([{"op":"add","primitive":{"kind":"cube"}}])
                } else {
                    json!([{"op":"delete","id":999999}])
                };
                Json(json!({"choices":[{"message":{"content":json!({"commands":commands}).to_string()}}]}))
            }
        }
    }));
    let provider_url = spawn(provider).await;
    let base = editor(Some(server::AiConfig {
        base_url: format!("{provider_url}/v1"),
        model: "mock".into(),
        api_key: None,
    }))
    .await;
    let c = reqwest::Client::new();
    let pending = tokio::spawn({
        let c = c.clone();
        let base = base.clone();
        async move {
            c.post(format!("{base}/api/chat"))
                .json(&json!({"prompt":"delayed","mode":"proposal"}))
                .send()
                .await
                .unwrap()
        }
    });
    started.notified().await;
    c.post(format!("{base}/api/commands"))
        .json(&json!({"commands":[{"op":"add","primitive":{"kind":"sphere"}}]}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    release.notify_one();
    assert_eq!(pending.await.unwrap().status(), 409);
    assert_eq!(
        c.post(format!("{base}/api/chat"))
            .json(&json!({"prompt":"invalid","mode":"proposal"}))
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    let proposals: Value = c
        .get(format!("{base}/api/proposals"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(proposals["pending"].as_array().unwrap().is_empty());
    let scene: Value = c
        .get(format!("{base}/api/scene"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(scene["revision"], 1);
    assert_eq!(scene["objects"].as_array().unwrap().len(), 1);
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
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 16);
    let r = call(json!({"jsonrpc":"2.0","id":100,"method":"tools/call","params":{"name":"update_presence","arguments":{"actor":{"id":"agent-1","name":"Agent One"},"selection":[1],"editing":[1]}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = call(json!({"jsonrpc":"2.0","id":101,"method":"tools/call","params":{"name":"get_presence","arguments":{}}})).await;
    let presence: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(presence["peers"][0]["actor"]["name"], "Agent One");
    let r = call(json!({"jsonrpc":"2.0","id":102,"method":"tools/call","params":{"name":"leave_presence","arguments":{"id":"agent-1"}}})).await;
    assert_eq!(r["result"]["isError"], false);
    let r = call(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "apply_commands", "arguments": {"actor":{"id":"agent-1","name":"Agent One"},"commands": [{"op": "add", "primitive": {"kind": "torus"}}]}}})).await;
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
    let history: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let first = &history["steps"][0];
    assert_eq!(first["source"], "agent", "{history}");
    assert_eq!(first["actor"]["name"], "Agent One");
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
    // Distance intent uses the existing typed command tool and inspection.
    let r = call(json!({"jsonrpc":"2.0","id":50,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[
        {"op":"add","name":"Distance A","primitive":{"kind":"cube"},"translation":[10,0.5,0]},
        {"op":"add","name":"Distance B","primitive":{"kind":"cube"},"translation":[14,0.5,0]},
        {"op":"constrain","id":"Distance B","distance":2,"from":"Distance A"},
        {"op":"transform","id":"Distance A","translation":[11,0.5,0]}
    ]}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = call(json!({"jsonrpc":"2.0","id":51,"method":"tools/call","params":{"name":"get_scene","arguments":{}}})).await;
    let scene: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let b = scene["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "Distance B")
        .unwrap();
    assert_eq!(b["transform"]["translation"][0], 13.0);
    let r = call(json!({"jsonrpc":"2.0","id":52,"method":"tools/call","params":{"name":"inspect_scene","arguments":{}}})).await;
    let report: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(
        !report["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "constraint_violation")
    );
    // Alignment is available through the same 16 tools, with free axes retained.
    let r = call(json!({"jsonrpc":"2.0","id":53,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[
        {"op":"add","name":"Aligned A","primitive":{"kind":"cube"},"translation":[20,1,0]},
        {"op":"add","name":"Aligned B","primitive":{"kind":"cube"},"translation":[23,4,2]},
        {"op":"constrain","id":"Aligned B","align":"y","from":"Aligned A"}
    ]}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = call(json!({"jsonrpc":"2.0","id":54,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[{"op":"transform","id":"Aligned B","translation":[25,6,3]}]}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = call(json!({"jsonrpc":"2.0","id":55,"method":"tools/call","params":{"name":"get_scene","arguments":{}}})).await;
    let scene: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let a = scene["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "Aligned A")
        .unwrap();
    assert_eq!(a["transform"]["translation"], json!([20.0, 6.0, 0.0]));
    let r = call(json!({"jsonrpc":"2.0","id":56,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[{"op":"constrain","id":"Aligned A","align":"y","from":"Aligned A"}]}}})).await;
    assert_eq!(r["result"]["isError"], true, "{r}");
    // Maintained layouts use the same command schema and shared scene.
    let r = call(json!({"jsonrpc":"2.0","id":57,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[
        {"op":"add","name":"Layout anchor","primitive":{"kind":"cube"},"translation":[40,0,0]},
        {"op":"add","name":"Layout A","primitive":{"kind":"cube"},"translation":[37,0,0]},
        {"op":"add","name":"Layout B","primitive":{"kind":"cube"},"translation":[43,0,0]},
        {"op":"arrange","ids":["Layout A","Layout B"],"layout":"row","around":"Layout anchor","spacing":2,"keep":true}
    ]}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = call(json!({"jsonrpc":"2.0","id":58,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[{"op":"move","id":"Layout anchor","offset":[2,0,2]}]}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = call(json!({"jsonrpc":"2.0","id":59,"method":"tools/call","params":{"name":"get_scene","arguments":{}}})).await;
    let scene: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(scene["arrangements"].as_array().unwrap().len(), 1);
    let b = scene["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "Layout B")
        .unwrap();
    assert_eq!(b["transform"]["translation"], json!([43.5, 0.5, 2.0]));
    let r = call(json!({"jsonrpc":"2.0","id":60,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[{"op":"unarrange","id":scene["arrangements"][0]["id"]}]}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    // Rebase independent object edits without losing the newer command.
    let r = call(json!({"jsonrpc":"2.0","id":61,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[
        {"op":"add","name":"Rebase A","primitive":{"kind":"cube"},"translation":[60,0,0]},
        {"op":"add","name":"Rebase B","primitive":{"kind":"cube"},"translation":[63,0,0]}
    ]}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = call(json!({"jsonrpc":"2.0","id":62,"method":"tools/call","params":{"name":"get_scene","arguments":{}}})).await;
    let scene: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let context = scene["revision"].as_u64().unwrap();
    let r = call(json!({"jsonrpc":"2.0","id":63,"method":"tools/call","params":{"name":"apply_commands","arguments":{"expected_revision":context,"commands":[{"op":"transform","id":"Rebase A","translation":[61,0,0]}]}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = call(json!({"jsonrpc":"2.0","id":64,"method":"tools/call","params":{"name":"apply_commands","arguments":{"expected_revision":context,"rebase":true,"commands":[{"op":"material","id":"Rebase B","color":"#ff0000"}]}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let result: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(result["rebased_from"], context);
    let r = call(json!({"jsonrpc":"2.0","id":65,"method":"tools/call","params":{"name":"get_history","arguments":{}}})).await;
    let history: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        history["steps"].as_array().unwrap().last().unwrap()["rebased_from"],
        context
    );
    let r = call(json!({"jsonrpc":"2.0","id":66,"method":"tools/call","params":{"name":"apply_commands","arguments":{"expected_revision":context,"rebase":true,"commands":[{"op":"transform","id":"Rebase A","translation":[62,0,0]}]}}})).await;
    assert_eq!(r["result"]["isError"], true, "{r}");

    // Creation rebase uses the current allocator through the same MCP tool.
    let r = call(json!({"jsonrpc":"2.0","id":660,"method":"tools/call","params":{"name":"apply_commands","arguments":{"expected_revision":context,"rebase":true,"commands":[{"op":"add","name":"Rebased MCP creation","primitive":{"kind":"cube"}}]}}})).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let result: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(result["rebased_from"], context);
    assert_eq!(result["created"].as_array().unwrap().len(), 1);
    let r = call(json!({"jsonrpc":"2.0","id":661,"method":"tools/call","params":{"name":"apply_commands","arguments":{"expected_revision":context,"rebase":true,"commands":[{"op":"add_camera","name":"Rebased MCP creation"}]}}})).await;
    assert_eq!(r["result"]["isError"], true, "{r}");

    let r = call(json!({"jsonrpc":"2.0","id":67,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[{"op":"add_camera","name":"MCP Shot","translation":[0,1,4]},{"op":"camera_settings","id":"MCP Shot","lens":{"fov":50,"focus":4}}]}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    let output = std::env::temp_dir().join(format!("tatara-camera-{}.png", std::process::id()));
    let r = call(json!({"jsonrpc":"2.0","id":68,"method":"tools/call","params":{"name":"render_image","arguments":{"camera":"MCP Shot","size":[8,8],"samples":1,"path":output}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    assert!(std::fs::read(&output).unwrap().starts_with(b"\x89PNG"));
    std::fs::remove_file(output).unwrap();

    let r=call(json!({"jsonrpc":"2.0","id":69,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[{"op":"add_light","name":"MCP Key","translation":[0,2,3]},{"op":"light_settings","id":"MCP Key","lamp":{"color":"#ff6633","intensity":30}}]}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    let r=call(json!({"jsonrpc":"2.0","id":70,"method":"tools/call","params":{"name":"get_scene","arguments":{}}})).await;
    let scene: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(
        scene["objects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["name"] == "MCP Key" && o["light"]["intensity"] == 30.)
    );
    let r=call(json!({"jsonrpc":"2.0","id":71,"method":"tools/call","params":{"name":"render_view","arguments":{"views":["front"],"size":64}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    assert!(
        r["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["type"] == "image")
    );
    let r=call(json!({"jsonrpc":"2.0","id":72,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[{"op":"constrain","id":"MCP Key","orientation":"MCP Shot"},{"op":"transform","id":"MCP Shot","rotation":[0.2,0.4,0.1]}]}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    let r=call(json!({"jsonrpc":"2.0","id":73,"method":"tools/call","params":{"name":"get_scene","arguments":{}}})).await;
    let scene: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(
        scene["constraints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["kind"] == "orientation")
    );
    let light = scene["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "MCP Key")
        .unwrap();
    assert!(light["transform"]["rotation"][1].as_f64().unwrap().abs() > 0.1);
    let r=call(json!({"jsonrpc":"2.0","id":74,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[{"op":"add","name":"MCP surface","primitive":{"kind":"cylinder","segments":9},"translation":[80,0.5,0]}]}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    let r=call(json!({"jsonrpc":"2.0","id":75,"method":"tools/call","params":{"name":"get_history","arguments":{"limit":1}}})).await;
    let history: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let step = history["steps"][0]["step"].as_u64().unwrap();
    let r=call(json!({"jsonrpc":"2.0","id":76,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[{"op":"extrude","id":"MCP surface","face":{"normal":[0,1,0],"centre":[0.2943407405,0.5,0.1071312683],"max_distance":0.1},"distance":0.4}]}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    let r=call(json!({"jsonrpc":"2.0","id":77,"method":"tools/call","params":{"name":"revise_step","arguments":{"step":step,"commands":[{"op":"add","name":"MCP surface","primitive":{"kind":"cylinder","segments":12},"translation":[80,0.5,0]}]}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    let r=call(json!({"jsonrpc":"2.0","id":78,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[
        {"op":"set_keyframe","id":"MCP Shot","property":"camera_fov","frame":1,"value":20,"interpolation":"linear"},
        {"op":"set_keyframe","id":"MCP Shot","property":"camera_fov","frame":3,"value":60},
        {"op":"set_keyframe","id":"MCP Key","property":"light_intensity","frame":1,"value":0,"interpolation":"linear"},
        {"op":"set_keyframe","id":"MCP Key","property":"light_intensity","frame":3,"value":40}
    ]}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    let r=call(json!({"jsonrpc":"2.0","id":79,"method":"tools/call","params":{"name":"get_scene","arguments":{"frame":2}}})).await;
    let scene: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let objs = scene["objects"].as_array().unwrap();
    assert_eq!(
        objs.iter().find(|o| o["name"] == "MCP Shot").unwrap()["camera"]["fov"],
        40.
    );
    assert_eq!(
        objs.iter().find(|o| o["name"] == "MCP Key").unwrap()["light"]["intensity"],
        20.
    );
    let output = std::env::temp_dir().join(format!("tatara-optics-{}.png", std::process::id()));
    let r=call(json!({"jsonrpc":"2.0","id":80,"method":"tools/call","params":{"name":"render_image","arguments":{"camera":"MCP Shot","frame":2,"size":[8,8],"samples":1,"path":output}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    assert!(std::fs::read(&output).unwrap().starts_with(b"\x89PNG"));
    std::fs::remove_file(output).unwrap();
    let r=call(json!({"jsonrpc":"2.0","id":81,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[{"op":"camera_settings","id":"MCP Shot","lens":{"ortho_height":4}},{"op":"set_keyframe","id":"MCP Shot","property":"camera_height","frame":1,"value":2,"interpolation":"linear"},{"op":"set_keyframe","id":"MCP Shot","property":"camera_height","frame":3,"value":6}]}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    let r=call(json!({"jsonrpc":"2.0","id":82,"method":"tools/call","params":{"name":"get_scene","arguments":{"frame":2}}})).await;
    let scene: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        scene["objects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == "MCP Shot")
            .unwrap()["camera"]["ortho_height"],
        4.
    );
    let output = std::env::temp_dir().join(format!("tatara-ortho-{}.png", std::process::id()));
    let r=call(json!({"jsonrpc":"2.0","id":83,"method":"tools/call","params":{"name":"render_image","arguments":{"camera":"MCP Shot","frame":2,"size":[8,8],"samples":1,"path":output}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    assert!(std::fs::read(&output).unwrap().starts_with(b"\x89PNG"));
    std::fs::remove_file(output).unwrap();
    let r=call(json!({"jsonrpc":"2.0","id":84,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[{"op":"light_settings","id":"MCP Key","lamp":{"kind":"spot","inner_cone":0.2,"outer_cone":0.8,"range":8,"intensity":40}}]}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    let r=call(json!({"jsonrpc":"2.0","id":85,"method":"tools/call","params":{"name":"get_scene","arguments":{}}})).await;
    let scene: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let spot = scene["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "MCP Key")
        .unwrap();
    assert_eq!(spot["light"]["kind"], "spot");
    assert_eq!(spot["light"]["range"], 8.);
    let r=call(json!({"jsonrpc":"2.0","id":86,"method":"tools/call","params":{"name":"apply_commands","arguments":{"commands":[{"op":"light_settings","id":"MCP Key","lamp":{"kind":"spot","inner_cone":0.8,"outer_cone":0.8}}]}}})).await;
    assert_eq!(r["result"]["isError"], true);
    let output = std::env::temp_dir().join(format!("tatara-spot-{}.png", std::process::id()));
    let r=call(json!({"jsonrpc":"2.0","id":87,"method":"tools/call","params":{"name":"render_image","arguments":{"camera":"MCP Shot","size":[8,8],"samples":1,"path":output}}})).await;
    assert_ne!(r["result"]["isError"], true, "{r}");
    assert!(std::fs::read(&output).unwrap().starts_with(b"\x89PNG"));
    std::fs::remove_file(output).unwrap();

    child.kill().await.unwrap();
}

#[tokio::test]
async fn concurrent_independent_edits_rebase_and_undo_preserves_the_first_writer() {
    let base = editor(None).await;
    let c = reqwest::Client::new();
    let command_url = format!("{base}/api/commands");
    assert_eq!(
        c.post(&command_url)
            .json(&json!({"commands":[
                {"op":"add","name":"A","primitive":{"kind":"cube"}},
                {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[3,0,0]}
            ]}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let alice = json!({"expected_revision":1,"rebase":true,"actor":{"id":"alice","name":"Alice"},"commands":[{"op":"transform","id":"A","translation":[1,0,0]}]});
    let bob = json!({"expected_revision":1,"rebase":true,"actor":{"id":"bob","name":"Bob"},"commands":[{"op":"material","id":"B","color":"#ff0000"}]});
    let (a, b) = tokio::join!(
        c.post(&command_url).json(&alice).send(),
        c.post(&command_url).json(&bob).send()
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.status(), 200);
    assert_eq!(b.status(), 200);
    let a: Value = a.json().await.unwrap();
    let b: Value = b.json().await.unwrap();
    assert_eq!(
        [&a, &b].iter().filter(|r| r["rebased_from"] == 1).count(),
        1
    );
    let scene: Value = c
        .get(format!("{base}/api/scene"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(scene["objects"][0]["transform"]["translation"][0], 1.0);
    assert_eq!(scene["objects"][1]["material"]["color"], "#ff0000");
    assert_eq!(
        c.post(format!("{base}/api/undo"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let undone: Value = c
        .get(format!("{base}/api/scene"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    if a["revision"] == 2 {
        assert_eq!(undone["objects"][0]["transform"]["translation"][0], 1.0);
        assert_ne!(undone["objects"][1]["material"]["color"], "#ff0000");
    } else {
        assert_eq!(undone["objects"][0]["transform"]["translation"][0], 0.0);
        assert_eq!(undone["objects"][1]["material"]["color"], "#ff0000");
    }
    assert_eq!(
        c.post(format!("{base}/api/redo"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(c.post(&command_url).json(&json!({"expected_revision":5,"commands":[{"op":"transform","id":"A","translation":[2,0,0]}]})).send().await.unwrap().status(), 200);
    let before: Value = c
        .get(format!("{base}/api/scene"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let stale = c.post(&command_url).json(&json!({"expected_revision":5,"rebase":true,"commands":[{"op":"transform","id":"A","translation":[3,0,0]}]})).send().await.unwrap();
    assert_eq!(stale.status(), 409);
    let error: Value = stale.json().await.unwrap();
    assert!(error["error"].as_str().unwrap().contains("A"));
    let after: Value = c
        .get(format!("{base}/api/scene"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after, before);
}

#[tokio::test]
async fn collaboration_presence_and_concurrent_edits() {
    let base = editor(None).await;
    let c = reqwest::Client::new();
    let alice = json!({"id":"alice", "name":"Alice"});
    let bob = json!({"id":"bob", "name":"Bob"});
    // Subscribe before joining: presence notifications do not change revision.
    let mut events = c.get(format!("{base}/api/events")).send().await.unwrap();
    let p = json!({"actor":alice, "cursor":[0.25,0.4], "selection":[1], "editing":[1], "camera":{"eye":[2,2,4],"target":[0,0,0],"fov":36}});
    let r = c
        .post(format!("{base}/api/presence"))
        .json(&p)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let data: Value = r.json().await.unwrap();
    assert_eq!(data["peers"][0]["editing"], json!([1]));
    let stream = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut text = String::new();
        while !text.contains("event: presence\ndata: 1") {
            text.push_str(&String::from_utf8_lossy(
                &events.chunk().await.unwrap().unwrap(),
            ));
        }
        text
    })
    .await
    .unwrap();
    assert!(stream.contains("event: presence"));
    let command = |actor: Value| json!({"actor":actor, "source":"UI", "expected_revision":0, "commands":[{"op":"add", "primitive":{"kind":"cube"}}]});
    let (a, b) = tokio::join!(
        c.post(format!("{base}/api/commands"))
            .json(&command(alice))
            .send(),
        c.post(format!("{base}/api/commands"))
            .json(&command(bob))
            .send()
    );
    let a = a.unwrap();
    let b = b.unwrap();
    let (winner, loser) = if a.status() == 200 { (a, b) } else { (b, a) };
    assert_eq!(winner.status(), 200);
    assert_eq!(loser.status(), 409);
    let error: Value = loser.json().await.unwrap();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("refresh the scene")
    );
    let state: Value = c
        .get(format!("{base}/api/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["scene"]["revision"], 1);
    assert_eq!(state["scene"]["objects"].as_array().unwrap().len(), 1);
    let history: Value = c
        .get(format!("{base}/api/history"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(matches!(
        history["steps"][0]["actor"]["name"].as_str(),
        Some("Alice" | "Bob")
    ));
    let mut bad = p.clone();
    bad["cursor"] = json!([1.1, 0]);
    assert_eq!(
        c.post(format!("{base}/api/presence"))
            .json(&bad)
            .send()
            .await
            .unwrap()
            .status(),
        422
    );
    let left: Value = c
        .delete(format!("{base}/api/presence"))
        .json(&json!({"id":"alice"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(left["peers"], json!([]));
}

#[tokio::test]
async fn distance_http_rejects_conflicts_and_inspects_imported_residuals() {
    let base = editor(None).await;
    let c = reqwest::Client::new();
    let r = c
        .post(format!("{base}/api/commands"))
        .json(&json!({"commands":[
            {"op":"add","name":"A","primitive":{"kind":"cube"}},
            {"op":"add","name":"B","primitive":{"kind":"cube"},"translation":[3,0,0]},
            {"op":"constrain","id":"B","distance":2,"from":"A"}
        ]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let scene: Value = c
        .get(format!("{base}/api/scene"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let r = c
        .post(format!("{base}/api/commands"))
        .json(&json!({"commands":[{"op":"constrain","id":"B","mirrors":"A"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
    let error: Value = r.json().await.unwrap();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("distance constraint")
    );
    let current: Value = c
        .get(format!("{base}/api/scene"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(current, scene);
    let mut broken = scene;
    broken["objects"][1]["transform"]["translation"] = json!([9, 0, 0]);
    assert_eq!(
        c.put(format!("{base}/api/scene"))
            .json(&broken)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let report: Value = c
        .get(format!("{base}/api/inspect"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        report["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "constraint_violation" && i["actual"] == 9.0)
    );
}
