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

use crate::engine::{self, CommandBatch, Editor, EngineError};

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
    presence: Mutex<crate::collaboration::Room>,
    editor: Mutex<Editor>,
    /// Live updates: ("revision", scene revision) or ("proposals", their version).
    events: broadcast::Sender<(&'static str, u64)>,
    ai: Option<AiConfig>,
    http: reqwest::Client,
}

pub type Shared = Arc<AppState>;

pub fn state(ai: Option<AiConfig>) -> Shared {
    let (events, _) = broadcast::channel(64);
    Arc::new(AppState {
        presence: Mutex::new(crate::collaboration::Room::default()),
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
        .route("/events", get(get_events))
        .route(
            "/presence",
            get(get_presence).post(post_presence).delete(leave_presence),
        )
        .route("/ai", get(get_ai))
        .route("/chat", post(post_chat))
        .fallback(core)
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

fn changed(state: &AppState, revision: u64) {
    let _ = state.events.send(("revision", revision));
}

fn proposals_changed(state: &AppState, version: u64) {
    let _ = state.events.send(("proposals", version));
}

/// Everything except live events and chat goes through the shared,
/// transport-independent router (also used by the WebAssembly build).
async fn core(
    State(s): State<Shared>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    body: axum::body::Bytes,
) -> Response {
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    // Renders gather the scene under the lock, then run on a worker thread
    // without it, so edits and other requests are not held up.
    if method == axum::http::Method::GET
        && matches!(
            uri.path(),
            "/pathtrace" | "/render" | "/render/image" | "/proposal/render"
        )
    {
        let query = uri.query().unwrap_or("");
        let job: Result<Box<dyn FnOnce() -> crate::api::Response + Send>, _> = {
            let ed = s.editor.lock().await;
            if uri.path() == "/proposal/render" {
                crate::api::proposal_image_job(&ed, query)
                    .map(|j| Box::new(move || j.run()) as Box<_>)
            } else if uri.path() == "/render/image" {
                crate::api::image_job(&ed, query).map(|j| Box::new(move || j.run()) as Box<_>)
            } else if uri.path() == "/render" {
                crate::api::render_job(&ed, query)
                    .map(|j| Box::new(move || crate::api::render_png(j)) as Box<_>)
            } else {
                crate::api::path_job(&ed, query).map(|j| Box::new(move || j.run()) as Box<_>)
            }
        };
        let r = match job {
            Ok(job) => tokio::task::spawn_blocking(job)
                .await
                .unwrap_or_else(|e| crate::api::Response::error(500, e.to_string())),
            Err(e) => e,
        };
        return reply(r);
    }
    let (r, before, after) = {
        let mut ed = s.editor.lock().await;
        let before = ed.proposals_version();
        let r = crate::api::handle(&mut ed, method.as_str(), path, &body, s.ai.is_some());
        (r, before, ed.proposals_version())
    };
    if let Some(revision) = r.changed {
        changed(&s, revision);
    }
    if after != before {
        proposals_changed(&s, after);
    }
    reply(r)
}

fn reply(r: crate::api::Response) -> Response {
    let mut response = (
        StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        [(header::CONTENT_TYPE, r.content_type)],
        r.body,
    )
        .into_response();
    if let Some(d) = r.disposition {
        response.headers_mut().insert(
            header::CONTENT_DISPOSITION,
            header::HeaderValue::from_static(d),
        );
    }
    response
}

/// Read a JSON body, reporting schema errors as JSON messages.
fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body)
        .map_err(|e| api_error(StatusCode::BAD_REQUEST, format!("invalid request: {e}")))
}

async fn get_events(
    State(s): State<Shared>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let receiver = s.events.subscribe();
    let (revision, proposals) = {
        let ed = s.editor.lock().await;
        (ed.scene().revision, ed.proposals_version())
    };
    let presence = s.presence.lock().await.version;
    let first = futures_util::stream::iter([
        ("revision", revision),
        ("proposals", proposals),
        ("presence", presence),
    ]);
    let updates = BroadcastStream::new(receiver).map(|r| r.unwrap_or(("resync", 0)));
    let stream = first
        .chain(updates)
        .map(|(kind, n)| Ok(Event::default().event(kind).data(n.to_string())));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn presence_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn presence_snapshot(room: &crate::collaboration::Room) -> Value {
    json!({ "version": room.version, "peers": room.peers(), "ttl_ms": crate::collaboration::TTL_MS })
}

async fn get_presence(State(s): State<Shared>) -> Json<Value> {
    let mut room = s.presence.lock().await;
    let before = room.version;
    room.expire(presence_now());
    if before != room.version {
        let _ = s.events.send(("presence", room.version));
    }
    Json(presence_snapshot(&room))
}

async fn post_presence(State(s): State<Shared>, body: axum::body::Bytes) -> ApiResult {
    let p: crate::collaboration::Presence = parse(&body)?;
    let mut room = s.presence.lock().await;
    let before = room.version;
    room.update(p, presence_now())?;
    if before != room.version {
        let _ = s.events.send(("presence", room.version));
    }
    Ok(Json(presence_snapshot(&room)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeavePresence {
    id: String,
}

async fn leave_presence(State(s): State<Shared>, body: axum::body::Bytes) -> ApiResult {
    let p: LeavePresence = parse(&body)?;
    let mut room = s.presence.lock().await;
    room.leave(&p.id);
    let _ = s.events.send(("presence", room.version));
    Ok(Json(presence_snapshot(&room)))
}

async fn get_ai(State(s): State<Shared>) -> Json<Value> {
    Json(json!({ "enabled": s.ai.is_some(), "model": s.ai.as_ref().map(|a| a.model.clone()) }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatRequest {
    prompt: String,
    #[serde(default)]
    mode: crate::api::ChatMode,
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
    let before = ed.proposals_version();
    let before_revision = ed.scene().revision;
    let result = crate::api::complete_chat(&mut ed, prompt, &batch, req.mode)?;
    if ed.scene().revision != before_revision {
        changed(&s, ed.scene().revision);
    }
    if ed.proposals_version() != before {
        proposals_changed(&s, ed.proposals_version());
    }
    Ok(Json(result))
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
        source: Some("chat".into()),
        actor: None,
        ..batch
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_mode_is_explicit_and_legacy_requests_still_apply() {
        let legacy: ChatRequest = serde_json::from_value(json!({"prompt":"cube"})).unwrap();
        assert_eq!(legacy.mode, crate::api::ChatMode::Apply);
        let review: ChatRequest =
            serde_json::from_value(json!({"prompt":"cube","mode":"proposal"})).unwrap();
        assert_eq!(review.mode, crate::api::ChatMode::Proposal);
        assert!(
            serde_json::from_value::<ChatRequest>(json!({"prompt":"cube","mode":"unknown"}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<ChatRequest>(json!({"prompt":"cube","apply":false})).is_err()
        );
    }

    #[test]
    fn parses_fenced_replies() {
        let b = parse_batch("Sure!\n```json\n{\"commands\":[{\"op\":\"clear\"}]}\n```").unwrap();
        assert_eq!(b.commands.len(), 1);
        let b = parse_batch("[{\"op\":\"add\",\"primitive\":{\"kind\":\"cube\"}}]").unwrap();
        assert_eq!(b.commands.len(), 1);
        assert!(parse_batch("no").is_err());
    }
}
