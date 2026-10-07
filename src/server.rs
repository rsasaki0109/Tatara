//! Local HTTP server: the authoritative scene, the command API, live updates
//! for the browser, static assets and the optional chat provider.

use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{StatusCode, header},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast};
use tokio_stream::wrappers::BroadcastStream;
use tower_http::services::{ServeDir, ServeFile};

use crate::engine::{self, CommandBatch, Editor, EngineError, Scene};

#[derive(Clone, Debug)]
pub struct AiConfig {
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
}

impl AiConfig {
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var("TATARA_AI_BASE_URL")
            .ok()
            .filter(|s| !s.is_empty())?;
        let model = std::env::var("TATARA_AI_MODEL")
            .ok()
            .filter(|s| !s.is_empty())?;
        let api_key = std::env::var("TATARA_AI_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        Some(Self {
            base_url: base_url.trim_end_matches('/').into(),
            model,
            api_key,
        })
    }
}

pub struct AppState {
    editor: Mutex<Editor>,
    events: broadcast::Sender<u64>,
    ai: Option<AiConfig>,
    http: reqwest::Client,
}

pub type Shared = Arc<AppState>;

pub fn state(ai: Option<AiConfig>) -> Shared {
    let (events, _) = broadcast::channel(64);
    Arc::new(AppState {
        editor: Mutex::new(Editor::new()),
        events,
        ai,
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("http client"),
    })
}

pub fn router(state: Shared, web_dir: PathBuf) -> Router {
    let index = web_dir.join("index.html");
    let api = Router::new()
        .route("/state", get(get_state))
        .route("/scene", get(get_scene).put(put_scene))
        .route("/context", get(get_context))
        .route("/schema", get(get_schema))
        .route("/commands", post(post_commands))
        .route("/undo", post(post_undo))
        .route("/redo", post(post_redo))
        .route("/reset", post(post_reset))
        .route("/export/obj", get(get_obj))
        .route("/export/glb", get(get_glb))
        .route("/import", post(post_import))
        .route("/events", get(get_events))
        .route("/ai", get(get_ai))
        .route("/chat", post(post_chat))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(state);
    Router::new()
        .nest("/api", api)
        .fallback_service(ServeDir::new(web_dir).not_found_service(ServeFile::new(index)))
}

pub async fn serve(addr: SocketAddr, web_dir: PathBuf, ai: Option<AiConfig>) -> anyhow::Result<()> {
    if !web_dir.join("index.html").exists() {
        eprintln!(
            "warning: no web build at {} (run `npm --prefix web ci && npm --prefix web run build`)",
            web_dir.display()
        );
    }
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("Tatara is running at http://{}", listener.local_addr()?);
    if let Some(ai) = &ai {
        eprintln!("In-editor chat: {} via {}", ai.model, ai.base_url);
    }
    axum::serve(listener, router(state(ai), web_dir)).await?;
    Ok(())
}

// ---------------------------------------------------------------------------

struct ApiError(StatusCode, Value);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(self.1)).into_response()
    }
}

impl From<EngineError> for ApiError {
    fn from(e: EngineError) -> Self {
        let status = if e.stale {
            StatusCode::CONFLICT
        } else {
            StatusCode::UNPROCESSABLE_ENTITY
        };
        ApiError(status, json!({ "error": e.to_string(), "detail": e }))
    }
}

fn api_error(status: StatusCode, message: impl Into<String>) -> ApiError {
    ApiError(status, json!({ "error": message.into() }))
}

type ApiResult = Result<Json<Value>, ApiError>;

fn history(ed: &Editor) -> Value {
    json!({ "can_undo": ed.can_undo(), "can_redo": ed.can_redo() })
}

fn changed(state: &AppState, revision: u64) {
    let _ = state.events.send(revision);
}

async fn get_state(State(s): State<Shared>) -> Json<Value> {
    let ed = s.editor.lock().await;
    let mut scene = serde_json::to_value(ed.scene()).expect("scene serializes");
    // Objects with modifiers also carry the evaluated mesh the viewport shows.
    if let Some(objects) = scene["objects"].as_array_mut() {
        for (value, o) in objects.iter_mut().zip(&ed.scene().objects) {
            if !o.modifiers.is_empty() {
                value["display"] = json!(ed.evaluated(o));
            }
        }
    }
    Json(json!({ "scene": scene, "history": history(&ed), "ai": s.ai.is_some() }))
}

async fn get_scene(State(s): State<Shared>) -> Json<Scene> {
    Json(s.editor.lock().await.scene().clone())
}

async fn get_context(State(s): State<Shared>) -> Json<Value> {
    Json(engine::context(&*s.editor.lock().await))
}

async fn get_schema() -> Json<Value> {
    Json(engine::command_schema())
}

/// Parse the body ourselves so schema errors come back as JSON messages.
fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body)
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("invalid request: {e}")))
}

async fn post_commands(State(s): State<Shared>, body: axum::body::Bytes) -> ApiResult {
    let batch: CommandBatch = parse(&body)?;
    let mut ed = s.editor.lock().await;
    let result = ed.apply(&batch)?;
    changed(&s, result.revision);
    Ok(Json(
        json!({ "revision": result.revision, "created": result.created, "history": history(&ed) }),
    ))
}

async fn put_scene(State(s): State<Shared>, body: axum::body::Bytes) -> ApiResult {
    let scene: Scene = parse(&body)?;
    let mut ed = s.editor.lock().await;
    let revision = ed.load(scene)?;
    changed(&s, revision);
    Ok(Json(
        json!({ "revision": revision, "history": history(&ed) }),
    ))
}

async fn post_undo(State(s): State<Shared>) -> ApiResult {
    let mut ed = s.editor.lock().await;
    let revision = ed
        .undo()
        .ok_or_else(|| api_error(StatusCode::CONFLICT, "nothing to undo"))?;
    changed(&s, revision);
    Ok(Json(
        json!({ "revision": revision, "history": history(&ed) }),
    ))
}

async fn post_redo(State(s): State<Shared>) -> ApiResult {
    let mut ed = s.editor.lock().await;
    let revision = ed
        .redo()
        .ok_or_else(|| api_error(StatusCode::CONFLICT, "nothing to redo"))?;
    changed(&s, revision);
    Ok(Json(
        json!({ "revision": revision, "history": history(&ed) }),
    ))
}

async fn post_reset(State(s): State<Shared>) -> ApiResult {
    let mut ed = s.editor.lock().await;
    ed.reset();
    let revision = ed.scene().revision;
    changed(&s, revision);
    Ok(Json(
        json!({ "revision": revision, "history": history(&ed) }),
    ))
}

async fn get_glb(State(s): State<Shared>) -> Response {
    let glb = crate::gltf::export_glb(&*s.editor.lock().await);
    (
        [
            (header::CONTENT_TYPE, "model/gltf-binary"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"scene.glb\"",
            ),
        ],
        glb,
    )
        .into_response()
}

/// Import a `.glb` or `.gltf` (embedded buffers) as one undoable batch.
async fn post_import(State(s): State<Shared>, body: axum::body::Bytes) -> ApiResult {
    let commands = crate::gltf::import(&body)?;
    let mut ed = s.editor.lock().await;
    let result = ed.apply(&CommandBatch {
        commands,
        expected_revision: None,
    })?;
    changed(&s, result.revision);
    Ok(Json(
        json!({ "revision": result.revision, "created": result.created, "history": history(&ed) }),
    ))
}

async fn get_obj(State(s): State<Shared>) -> Response {
    let obj = engine::export_obj(&*s.editor.lock().await);
    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"scene.obj\"",
            ),
        ],
        obj,
    )
        .into_response()
}

async fn get_events(
    State(s): State<Shared>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let current = s.editor.lock().await.scene().revision;
    let first = futures_util::stream::once(async move { current });
    let updates = BroadcastStream::new(s.events.subscribe()).filter_map(|r| async move { r.ok() });
    let stream = first
        .chain(updates)
        .map(|rev| Ok(Event::default().event("revision").data(rev.to_string())));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn get_ai(State(s): State<Shared>) -> Json<Value> {
    Json(json!({ "enabled": s.ai.is_some(), "model": s.ai.as_ref().map(|a| a.model.clone()) }))
}

#[derive(Deserialize)]
struct ChatRequest {
    prompt: String,
}

async fn post_chat(State(s): State<Shared>, body: axum::body::Bytes) -> ApiResult {
    let req: ChatRequest = parse(&body)?;
    let Some(ai) = s.ai.clone() else {
        return Err(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "chat is not configured: set TATARA_AI_BASE_URL and TATARA_AI_MODEL on the server",
        ));
    };
    let prompt = req.prompt.trim();
    if prompt.is_empty() || prompt.len() > 8_000 {
        return Err(api_error(
            StatusCode::BAD_REQUEST,
            "prompt must be 1-8000 characters",
        ));
    }
    let (context, revision) = {
        let ed = s.editor.lock().await;
        (engine::context(&ed), ed.scene().revision)
    };
    let content = ask_provider(&s.http, &ai, prompt, &context).await?;
    let mut batch = parse_batch(&content).map_err(|e| {
        api_error(
            StatusCode::BAD_GATEWAY,
            format!("model reply was not a valid command batch: {e}"),
        )
    })?;
    batch.expected_revision = Some(revision);
    let mut ed = s.editor.lock().await;
    let result = ed.apply(&batch)?;
    changed(&s, result.revision);
    Ok(Json(json!({
        "revision": result.revision,
        "created": result.created,
        "commands": batch.commands,
        "history": history(&ed),
    })))
}

pub fn system_prompt(context: &Value) -> String {
    format!(
        "You are the modeling assistant inside Tatara, a 3D editor. Translate the user's request into \
         modeling commands. Reply with ONLY a JSON object of the form {{\"commands\": [...]}} that \
         validates against this JSON schema:\n{}\n\nScene units are meters, Y is up, rotations are \
         radians. New objects get IDs from next_id in order; you may also reference objects by exact \
         name. Place objects on the ground (y = 0 is the floor) unless asked otherwise.\n\nCurrent scene:\n{}",
        engine::command_schema(),
        context
    )
}

async fn ask_provider(
    http: &reqwest::Client,
    ai: &AiConfig,
    prompt: &str,
    context: &Value,
) -> Result<String, ApiError> {
    let body = json!({
        "model": ai.model,
        "temperature": 0,
        "messages": [
            { "role": "system", "content": system_prompt(context) },
            { "role": "user", "content": prompt },
        ],
    });
    let mut req = http
        .post(format!("{}/chat/completions", ai.base_url))
        .json(&body);
    if let Some(key) = &ai.api_key {
        req = req.bearer_auth(key);
    }
    let bad = |m: String| api_error(StatusCode::BAD_GATEWAY, m);
    let resp = req
        .send()
        .await
        .map_err(|e| bad(format!("provider request failed: {e}")))?;
    let status = resp.status();
    let value: Value = resp
        .json()
        .await
        .map_err(|e| bad(format!("provider returned invalid JSON: {e}")))?;
    if !status.is_success() {
        return Err(bad(format!("provider returned {status}: {value}")));
    }
    value["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| bad("provider reply has no message content".into()))
}

/// Accept a bare object, a bare command array, or either inside a code fence.
pub fn parse_batch(text: &str) -> Result<CommandBatch, String> {
    let start = text.find(['{', '[']).ok_or("no JSON found")?;
    let end = text.rfind(['}', ']']).ok_or("no JSON found")?;
    if end < start {
        return Err("no JSON found".into());
    }
    let value: Value = serde_json::from_str(&text[start..=end]).map_err(|e| e.to_string())?;
    let value = if value.is_array() {
        json!({ "commands": value })
    } else {
        value
    };
    let batch: CommandBatch = serde_json::from_value(value).map_err(|e| e.to_string())?;
    Ok(CommandBatch {
        expected_revision: None,
        ..batch
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_replies() {
        let b = parse_batch("Sure!\n```json\n{\"commands\":[{\"op\":\"clear\"}]}\n```").unwrap();
        assert_eq!(b.commands.len(), 1);
        let b = parse_batch("[{\"op\":\"add\",\"primitive\":{\"kind\":\"cube\"}}]").unwrap();
        assert_eq!(b.commands.len(), 1);
        assert!(parse_batch("no").is_err());
    }
}
