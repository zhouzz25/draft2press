use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{header, StatusCode},
    response::{
        sse::{Event, Sse},
        Html, IntoResponse, Json,
    },
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::CorsLayer;

use crate::config::ModelConfig;
use crate::cost::{CostTracker, TokenUsage};
use crate::formatter::{extract_html, FormatTask, PhotoRef};
use crate::llm::{LlmClient, Message};
use crate::materials::PhotoEntry;
use crate::writer::{build_revision_messages, Annotation, WritingTask};

struct MaterialEntry {
    name: String,
    content: String,
    size: usize,
}

#[derive(Clone)]
struct AppState {
    config: Arc<RwLock<ModelConfig>>,
    materials: Arc<Mutex<Vec<MaterialEntry>>>,
    photos: Arc<Mutex<Vec<PhotoEntry>>>,
    cost_tracker: Arc<Mutex<CostTracker>>,
    cancel_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

#[derive(Serialize)]
struct WriteResponse {
    content: String,
    usage: TokenUsage,
    debug_messages: Vec<Message>,
}

#[derive(Serialize)]
struct ImportResponse {
    content: String,
}

#[derive(Serialize)]
struct StatsResponse {
    call_count: usize,
    total_input_tokens: usize,
    total_output_tokens: usize,
    total_tokens: usize,
    total_cost: f64,
    total_cost_cny: f64,
}

#[derive(Serialize)]
struct MaterialInfo {
    name: String,
    size: usize,
}

#[derive(Deserialize)]
struct WriteRequest {
    topic: String,
    #[serde(default)]
    article_type: Option<String>,
}

#[derive(Deserialize)]
struct ReviseRequest {
    draft: String,
    annotations: Vec<AnnotationItem>,
}

#[derive(Deserialize, Serialize)]
struct AnnotationItem {
    selected_text: String,
    comment: String,
}

#[derive(Deserialize)]
struct ConfigUpdate {
    endpoint: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    context_length: Option<usize>,
    thinking_mode: Option<bool>,
    temperature: Option<f64>,
    max_tokens: Option<usize>,
    #[serde(default)]
    pricing: Option<PricingUpdate>,
    #[serde(default)]
    vision_endpoint: Option<String>,
    #[serde(default)]
    vision_api_key: Option<String>,
    #[serde(default)]
    vision_model: Option<String>,
    #[serde(default)]
    pub vision_max_tokens: Option<usize>,
    #[serde(default)]
    vision_desc_max_chars: Option<usize>,
    #[serde(default)]
    pub wx_app_id: Option<String>,
    #[serde(default)]
    pub wx_app_secret: Option<String>,
    #[serde(default)]
    pub token_budget: Option<usize>,
}

#[derive(Deserialize)]
struct PricingUpdate {
    input_per_1k: Option<f64>,
    output_per_1k: Option<f64>,
}

pub async fn run_server(config: ModelConfig, port: u16) -> anyhow::Result<()> {
    let pricing = config.pricing.clone();
    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        materials: Arc::new(Mutex::new(Vec::new())),
        photos: Arc::new(Mutex::new(Vec::new())),
        cost_tracker: Arc::new(Mutex::new(CostTracker::new(pricing))),
        cancel_tx: Arc::new(Mutex::new(None)),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/style.css", get(serve_css))
        .route("/app.js", get(serve_js))
        .route("/api/materials", get(list_materials).post(upload_material))
        .route("/api/materials/{name}", post(remove_material))
        .route("/api/photos", get(list_photos).post(upload_photo))
        .route("/api/photos/{name}", post(remove_photo))
        .route("/api/photo-img/{name}", get(serve_photo_img))
        .route("/api/photos/{name}/description", post(update_photo_description))
        .route("/api/write", post(write_article))
        .route("/api/revise", post(revise_article))
        .route("/api/format", post(format_article))
        .route("/api/download-zip", post(download_zip))
        .route("/api/import", post(import_article))
        .route("/api/cancel", post(cancel_task))
        .route("/api/config", get(get_config).post(update_config))
        .route("/api/settings/check", post(check_settings_dirty))
        .route("/api/stats", get(get_stats))
        .route("/api/photos/{name}/recognize", post(recognize_photo))
        .route("/api/session", post(save_session))
        .route("/api/sessions", get(list_sessions))
        .route("/api/session/{id}", get(load_session))
        .route("/api/publish", post(publish_draft))
        .layer(CorsLayer::permissive())
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024))
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    println!("服务已启动: http://localhost:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../frontend/index.html"))
}

async fn serve_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../frontend/style.css"),
    )
}

async fn serve_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/javascript; charset=utf-8")],
        include_str!("../frontend/app.js"),
    )
}

async fn list_materials(State(state): State<AppState>) -> Json<Vec<MaterialInfo>> {
    let materials = state.materials.lock().await;
    let list: Vec<MaterialInfo> = materials
        .iter()
        .map(|m| MaterialInfo {
            name: m.name.clone(),
            size: m.size,
        })
        .collect();
    Json(list)
}

async fn upload_material(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<StatusCode, (StatusCode, String)> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    {
        let filename = field.file_name().unwrap_or("unknown").to_string();
        let data = field
            .bytes()
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        let content = crate::materials::extract_text(&filename, &data)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        let size = content.len();

        let mut materials = state.materials.lock().await;
        materials.retain(|m| m.name != filename);
        materials.push(MaterialEntry {
            name: filename,
            content,
            size,
        });
    }
    Ok(StatusCode::OK)
}

async fn remove_material(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> StatusCode {
    let mut materials = state.materials.lock().await;
    materials.retain(|m| m.name != name);
    StatusCode::OK
}

fn progress_event(msg: &str) -> Result<Event, Infallible> {
    Ok(Event::default().event("progress").data(
        serde_json::json!({ "msg": msg }).to_string(),
    ))
}

async fn send_progress(
    tx: &tokio::sync::mpsc::Sender<Result<Event, Infallible>>,
    msg: &str,
) {
    let _ = tx.send(progress_event(msg)).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
}

async fn check_budget(state: &AppState, tx: &tokio::sync::mpsc::Sender<Result<Event, Infallible>>) -> bool {
    let config = state.config.read().await;
    if let Some(budget) = config.token_budget {
        let total = state.cost_tracker.lock().await.total_tokens();
        if total >= budget {
            drop(config);
            let _ = tx.send(error_event(&format!("Token 用量 ({total}) 已达预算上限 ({budget})，自动中断"))).await;
            return true;
        }
    }
    false
}

fn done_event(response: WriteResponse) -> Result<Event, Infallible> {
    Ok(Event::default()
        .event("done")
        .data(serde_json::to_string(&response).unwrap_or_default()))
}

fn error_event(msg: &str) -> Result<Event, Infallible> {
    Ok(Event::default()
        .event("error")
        .data(serde_json::json!({ "msg": msg }).to_string()))
}

async fn run_llm_with_cancel(
    state: &AppState, tx: &tokio::sync::mpsc::Sender<Result<Event, Infallible>>,
    client: LlmClient, messages: &[crate::llm::Message],
) -> Option<crate::llm::LlmResponse> {
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    { let mut g = state.cancel_tx.lock().await; *g = Some(cancel_tx); }
    let result = tokio::select! {
        r = client.chat(messages) => match r {
            Ok(r) => r,
            Err(e) => { let _ = tx.send(error_event(&e.to_string())).await; return None; }
        },
        _ = cancel_rx => { let _ = tx.send(Ok(Event::default().event("cancelled").data("{}"))).await; return None; }
    };
    { let mut t = state.cost_tracker.lock().await; t.record(&result.usage); }
    if check_budget(state, tx).await { return None; }
    { let mut g = state.cancel_tx.lock().await; *g = None; }
    Some(result)
}

async fn write_article(
    State(state): State<AppState>,
    Json(req): Json<WriteRequest>,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let state_c = state.clone();
    tokio::spawn(async move {
        send_progress(&tx, "正在加载素材...").await;
        let materials = state_c.materials.lock().await;
        let mut lib = crate::materials::MaterialLibrary::new();
        for m in materials.iter() { lib.add_text(&m.name, &m.content); }
        let context = lib.build_context();
        drop(materials);
        send_progress(&tx, "正在构建 Prompt...").await;
        let task = WritingTask { topic: req.topic, article_type: req.article_type, materials_context: context };
        let messages = task.build_messages();
        send_progress(&tx, "正在调用 AI 模型，请稍候...").await;
        let config = state_c.config.read().await.clone();
        let client = LlmClient::new(config);
        let response = match run_llm_with_cancel(&state_c, &tx, client, &messages).await { Some(r) => r, None => return };
        let _ = tx.send(progress_event("生成完成，正在处理结果...")).await;
        let _ = tx.send(done_event(WriteResponse { content: response.content, usage: response.usage, debug_messages: messages })).await;
    });
    Sse::new(ReceiverStream::new(rx))
}

async fn revise_article(
    State(state): State<AppState>,
    Json(req): Json<ReviseRequest>,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let state_c = state.clone();
    let draft = req.draft.clone();
    tokio::spawn(async move {
        send_progress(&tx, "正在构建批注修订 Prompt...").await;
        let annotations: Vec<Annotation> = req.annotations.into_iter().map(|a| Annotation { selected_text: a.selected_text, comment: a.comment }).collect();
        let messages = build_revision_messages(&draft, &annotations);
        send_progress(&tx, &format!("正在调用 AI 模型修订文章（{} 条批注）...", annotations.len())).await;
        let config = state_c.config.read().await.clone();
        let client = LlmClient::new(config);
        let response = match run_llm_with_cancel(&state_c, &tx, client, &messages).await { Some(r) => r, None => return };
        let content = if response.content.is_empty() { draft } else { response.content };
        let _ = tx.send(done_event(WriteResponse { content, usage: response.usage, debug_messages: messages })).await;
    });
    Sse::new(ReceiverStream::new(rx))
}

async fn cancel_task(State(state): State<AppState>) -> StatusCode {
    let mut guard = state.cancel_tx.lock().await;
    if let Some(tx) = guard.take() {
        let _ = tx.send(());
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn get_config(State(state): State<AppState>) -> Json<serde_json::Value> {
    let config = state.config.read().await;
    let key = &config.api_key;
    let masked = if key.len() > 8 {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    } else {
        "***".to_string()
    };
    Json(serde_json::json!({
        "endpoint": config.endpoint,
        "model": config.model,
        "context_length": config.context_length,
        "thinking_mode": config.thinking_mode,
        "temperature": config.temperature,
        "max_tokens": config.max_tokens,
        "api_key_masked": masked,
        "pricing": {
            "input_per_k": config.pricing.input_per_1k,
            "output_per_1k": config.pricing.output_per_1k,
        },
        "vision_endpoint": config.vision_endpoint,
        "vision_model": config.vision_model,
        "vision_max_tokens": config.vision_max_tokens,
        "vision_desc_max_chars": config.vision_desc_max_chars,
        "vision_configured": config.vision_model.is_some(),
        "wx_app_id": config.wx_app_id,
        "wx_configured": config.wx_app_id.is_some() && config.wx_app_secret.is_some(),
        "token_budget": config.token_budget
    }))
}

async fn update_config(
    State(state): State<AppState>,
    Json(req): Json<ConfigUpdate>,
) -> StatusCode {
    let mut config = state.config.write().await;
    if let Some(v) = req.endpoint {
        config.endpoint = v;
    }
    if let Some(v) = req.api_key {
        config.api_key = v;
    }
    if let Some(v) = req.model {
        config.model = v;
    }
    if let Some(v) = req.context_length {
        config.context_length = v;
    }
    if let Some(v) = req.thinking_mode {
        config.thinking_mode = v;
    }
    if let Some(v) = req.temperature {
        config.temperature = v;
    }
    if let Some(v) = req.max_tokens {
        config.max_tokens = v;
    }
    if let Some(p) = req.pricing {
        if let Some(v) = p.input_per_1k {
            config.pricing.input_per_1k = v;
        }
        if let Some(v) = p.output_per_1k {
            config.pricing.output_per_1k = v;
        }
    }
    if let Some(v) = req.vision_endpoint { config.vision_endpoint = Some(v); }
    if let Some(v) = req.vision_api_key { config.vision_api_key = Some(v); }
    if let Some(v) = req.vision_model { config.vision_model = Some(v); }
    if let Some(v) = req.vision_max_tokens { config.vision_max_tokens = Some(v); }
    if let Some(v) = req.vision_desc_max_chars { config.vision_desc_max_chars = Some(v); }
    if let Some(v) = req.wx_app_id { config.wx_app_id = Some(v); }
    if let Some(v) = req.wx_app_secret { config.wx_app_secret = Some(v); }
    if let Some(v) = req.token_budget { config.token_budget = Some(v); }
    // Persist to config.toml
    if let Ok(toml_str) = toml::to_string(&*config) {
        let _ = std::fs::write("config.toml", toml_str);
    }
    StatusCode::OK
}

// Compare the settings-form payload (same shape as ConfigUpdate) against the
// current server config, so the frontend can ask whether closing the settings
// modal would discard unsaved edits. Semantics mirror update_config:
// "None" means the field was left untouched; empty strings for keys/secrets
// mean "don't modify"; a non-empty key/secret typed but not saved counts as a change.
async fn check_settings_dirty(
    State(state): State<AppState>,
    Json(req): Json<ConfigUpdate>,
) -> Json<serde_json::Value> {
    let c = state.config.read().await;
    let mut changed = false;
    if let Some(v) = &req.endpoint { changed |= v != &c.endpoint; }
    if let Some(v) = &req.model { changed |= v != &c.model; }
    if let Some(v) = req.context_length { changed |= v != c.context_length; }
    if let Some(v) = req.max_tokens { changed |= v != c.max_tokens; }
    if let Some(v) = req.temperature { changed |= (v - c.temperature).abs() > 1e-9; }
    if let Some(v) = req.thinking_mode { changed |= v != c.thinking_mode; }
    if let Some(p) = &req.pricing {
        if let Some(v) = p.input_per_1k { changed |= (v - c.pricing.input_per_1k).abs() > 1e-12; }
        if let Some(v) = p.output_per_1k { changed |= (v - c.pricing.output_per_1k).abs() > 1e-12; }
    }
    if let Some(v) = &req.api_key { changed |= !v.is_empty(); }
    if let Some(v) = &req.vision_endpoint { changed |= c.vision_endpoint.as_deref() != Some(v.as_str()); }
    if let Some(v) = &req.vision_api_key { changed |= !v.is_empty(); }
    if let Some(v) = &req.vision_model { changed |= c.vision_model.as_deref() != Some(v.as_str()); }
    if let Some(v) = req.vision_max_tokens { changed |= c.vision_max_tokens != Some(v); }
    if let Some(v) = req.vision_desc_max_chars { changed |= c.vision_desc_max_chars != Some(v); }
    if let Some(v) = &req.wx_app_id { changed |= c.wx_app_id.as_deref() != Some(v.as_str()); }
    if let Some(v) = &req.wx_app_secret { changed |= !v.is_empty(); }
    if let Some(v) = req.token_budget { changed |= c.token_budget != Some(v); }
    Json(serde_json::json!({ "changed": changed }))
}

async fn get_stats(State(state): State<AppState>) -> Json<StatsResponse> {
    let tracker = state.cost_tracker.lock().await;
    Json(StatsResponse {
        call_count: tracker.call_count,
        total_input_tokens: tracker.total_input_tokens,
        total_output_tokens: tracker.total_output_tokens,
        total_tokens: tracker.total_tokens(),
        total_cost: tracker.total_cost,
        total_cost_cny: tracker.total_cost * 7.2,
    })
}

async fn import_article(
    mut multipart: Multipart,
) -> Result<Json<ImportResponse>, (StatusCode, String)> {
    if let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    {
        let filename = field.file_name().unwrap_or("unknown").to_string();
        let data = field
            .bytes()
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        let content = crate::materials::extract_text(&filename, &data)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        return Ok(Json(ImportResponse { content }));
    }
    Err((StatusCode::BAD_REQUEST, "未收到文件".to_string()))
}

// === Photos ===

#[derive(Serialize)]
struct PhotoInfo {
    name: String,
    data_url: String,
    description: String,
    width: u32,
    height: u32,
}

async fn list_photos(State(state): State<AppState>) -> Json<Vec<PhotoInfo>> {
    let photos = state.photos.lock().await;
    let list: Vec<PhotoInfo> = photos
        .iter()
        .map(|p| PhotoInfo {
            name: p.name.clone(),
            data_url: p.data_url.clone(),
            description: p.description.clone(),
            width: p.width,
            height: p.height,
        })
        .collect();
    Json(list)
}

async fn upload_photo(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<StatusCode, (StatusCode, String)> {
    let mut file: Option<(String, String)> = None;
    let mut width = 0u32;
    let mut height = 0u32;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    {
        match field.name().unwrap_or("") {
            "width" => width = field.text().await.ok().and_then(|t| t.parse().ok()).unwrap_or(0),
            "height" => height = field.text().await.ok().and_then(|t| t.parse().ok()).unwrap_or(0),
            _ => {
                let filename = field.file_name().unwrap_or("unknown").to_string();
                let content_type = field.content_type().unwrap_or("image/jpeg").to_string();
                let data = field
                    .bytes()
                    .await
                    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
                let data_url = format!(
                    "data:{};base64,{}",
                    content_type,
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data)
                );
                file = Some((filename, data_url));
            }
        }
    }
    if let Some((filename, data_url)) = file {
        let mut photos = state.photos.lock().await;
        photos.retain(|p| p.name != filename);
        photos.push(PhotoEntry { name: filename, data_url, description: String::new(), width, height });
    }
    Ok(StatusCode::OK)
}

async fn remove_photo(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> StatusCode {
    let mut photos = state.photos.lock().await;
    photos.retain(|p| p.name != name);
    StatusCode::OK
}

#[allow(dead_code)]
fn parse_data_url(data_url: &str) -> Result<(String, Vec<u8>), (StatusCode, String)> {
    let parts: Vec<&str> = data_url.splitn(2, ',').collect();
    if parts.len() != 2 { return Err((StatusCode::BAD_REQUEST, "图片数据格式错误".into())); }
    let content_type = parts[0].strip_prefix("data:").and_then(|s| s.split(';').next()).unwrap_or("image/jpeg").to_string();
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, parts[1])
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("解码失败: {e}")))?;
    Ok((content_type, bytes))
}

async fn serve_photo_img(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let photos = state.photos.lock().await;
    let photo = photos.iter().find(|p| p.name == name)
        .ok_or((StatusCode::NOT_FOUND, "照片不存在".to_string()))?;
    let data_url = photo.data_url.clone();
    drop(photos);
    let parts: Vec<&str> = data_url.splitn(2, ',').collect();
    if parts.len() != 2 {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "图片数据格式错误".into()));
    }
    let content_type = parts[0].strip_prefix("data:")
        .and_then(|s| s.split(';').next())
        .unwrap_or("image/jpeg").to_string();
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, parts[1])
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("解码失败: {e}")))?;
    Ok(axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, &content_type)
        .header(header::CACHE_CONTROL, "max-age=3600")
        .body(axum::body::Body::from(bytes))
        .unwrap())
}

#[derive(Deserialize)]
struct DescriptionUpdate {
    description: String,
}

async fn update_photo_description(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<DescriptionUpdate>,
) -> StatusCode {
    let mut photos = state.photos.lock().await;
    if let Some(photo) = photos.iter_mut().find(|p| p.name == name) {
        photo.description = req.description;
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

#[derive(Serialize)]
struct RecognizeResponse {
    description: String,
    usage: Option<TokenUsage>,
    debug_messages: Vec<crate::llm::Message>,
}

async fn recognize_photo(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<RecognizeResponse>, (StatusCode, String)> {
    let config = state.config.read().await;
    let endpoint = config.vision_endpoint.as_deref().or(Some(&config.endpoint)).unwrap_or("");
    let api_key = config.vision_api_key.as_deref().unwrap_or(&config.api_key);
    let model = config.vision_model.as_deref().unwrap_or("");

    if model.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "未配置 vision 模型，请在设置页填写 vision_model".into()));
    }

    if api_key.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "未配置 API Key，请在设置页填写 API Key 或 vision_api_key".into()));
    }

    let data_url = {
        let photos = state.photos.lock().await;
        photos.iter().find(|p| p.name == name).map(|p| p.data_url.clone())
            .ok_or_else(|| (StatusCode::NOT_FOUND, "照片不存在".to_string()))?
    };

    let max_chars = config.vision_desc_max_chars.unwrap_or(30);
    let body = serde_json::json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": format!("用中文简洁描述这张图片，{}字以内。", max_chars)},
                {"type": "image_url", "image_url": {"url": data_url}}
            ]
        }],
        "max_tokens": config.vision_max_tokens.unwrap_or(4000)
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let resp = client.post(endpoint).bearer_auth(api_key).json(&body).send().await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("vision API 请求失败: {e}")))?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if !status.is_success() {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("vision API 错误: {text}")));
    }

    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("解析失败: {e}")))?;

    let preview: String = text.chars().take(500).collect();
    eprintln!("[vision] 响应: {preview}");

    let usage_val = v.get("usage").cloned();
    let description = {
        let content = &v["choices"][0]["message"]["content"];
        if let Some(s) = content.as_str() {
            s.to_string()
        } else if let Some(arr) = content.as_array() {
            arr.iter()
                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("")
        } else {
            String::new()
        }
    };

    if description.is_empty() {
        return Err((StatusCode::INTERNAL_SERVER_ERROR, "vision API 未返回有效描述".into()));
    }

    let usage = if let Some(u) = usage_val {
        let pt = u.get("prompt_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as usize;
        let ct = u.get("completion_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as usize;
        let tt = u.get("total_tokens").and_then(|t| t.as_u64()).unwrap_or(0) as usize;
        let mut tracker = state.cost_tracker.lock().await;
        tracker.record(&crate::cost::TokenUsage { prompt_tokens: pt, completion_tokens: ct, total_tokens: tt });
        Some(crate::cost::TokenUsage { prompt_tokens: pt, completion_tokens: ct, total_tokens: tt })
    } else { None };

    // Auto-save the description
    {
        let mut photos = state.photos.lock().await;
        if let Some(photo) = photos.iter_mut().find(|p| p.name == name) {
            photo.description = description.clone();
        }
    }

    let debug_messages = vec![
        crate::llm::Message::system("vision 识图"),
        crate::llm::Message::user(format!("识别图片: {}", name)),
        crate::llm::Message::assistant(description.clone()),
    ];

    Ok(Json(RecognizeResponse { description, usage, debug_messages }))
}

// === Format (排版) ===

#[derive(Deserialize)]
struct FormatRequest {
    draft: String,
    #[serde(default)]
    template: Option<String>,
}

async fn format_article(
    State(state): State<AppState>,
    Json(req): Json<FormatRequest>,
) -> Sse<ReceiverStream<Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel(16);
    let state_c = state.clone();
    tokio::spawn(async move {
        let photos = state_c.photos.lock().await;
        let photo_refs: Vec<PhotoRef> = photos.iter().map(|p| PhotoRef { name: p.name.clone(), description: p.description.clone(), width: p.width, height: p.height }).collect();
        drop(photos);
        send_progress(&tx, &format!("正在准备排版（{} 张可选照片）...", photo_refs.len())).await;
        let task = FormatTask { draft: req.draft, photos: photo_refs, template: req.template };
        let messages = task.build_messages();
        send_progress(&tx, "正在调用 AI 生成排版 HTML...").await;
        let config = state_c.config.read().await.clone();
        let client = LlmClient::new(config);
        let response = match run_llm_with_cancel(&state_c, &tx, client, &messages).await { Some(r) => r, None => return };
        let html = extract_html(&response.content);
        if html.is_empty() { let _ = tx.send(error_event("排版结果为空，可能是 max_tokens 不足或模型未输出 HTML。")).await; return; }
        let _ = tx.send(progress_event("排版完成")).await;
        let _ = tx.send(Ok(Event::default().event("done").data(serde_json::json!({ "html": html, "usage": response.usage }).to_string()))).await;
    });
    Sse::new(ReceiverStream::new(rx))
}

// === Session management (R5) ===

#[derive(Deserialize)]
struct SessionSaveRequest {
    topic: String,
    #[serde(default)]
    article_type: Option<String>,
    draft: String,
    annotations: Vec<AnnotationItem>,
}

#[derive(Serialize)]
struct SessionInfo {
    id: String,
    timestamp: String,
    topic: String,
}

#[derive(Serialize)]
struct SessionLoadResponse {
    topic: String,
    article_type: Option<String>,
    draft: String,
    annotations: Vec<AnnotationItem>,
    materials: Vec<MaterialInfo>,
    photos: Vec<PhotoInfo>,
    stats: StatsResponse,
}

async fn save_session(
    State(state): State<AppState>,
    Json(req): Json<SessionSaveRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let materials = state.materials.lock().await;
    let photos = state.photos.lock().await;
    let tracker = state.cost_tracker.lock().await;

    let timestamp = format!("{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs());
    let session = serde_json::json!({
        "timestamp": &timestamp,
        "topic": req.topic,
        "article_type": req.article_type,
        "draft": req.draft,
        "annotations": req.annotations,
        "materials": materials.iter().map(|m| {
            serde_json::json!({"name": m.name, "content": m.content, "size": m.size})
        }).collect::<Vec<_>>(),
        "photos": photos.iter().map(|p| {
            serde_json::json!({"name": p.name, "data_url": p.data_url, "description": p.description, "width": p.width, "height": p.height})
        }).collect::<Vec<_>>(),
        "stats": {
            "call_count": tracker.call_count,
            "total_input_tokens": tracker.total_input_tokens,
            "total_output_tokens": tracker.total_output_tokens,
            "total_cost": tracker.total_cost,
        }
    });

    let dir = "sessions";
    std::fs::create_dir_all(dir).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let path = format!("{dir}/{timestamp}.json");
    std::fs::write(&path, serde_json::to_string_pretty(&session).unwrap())
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({ "id": timestamp, "path": path })))
}

async fn list_sessions() -> Result<Json<Vec<SessionInfo>>, (StatusCode, String)> {
    let dir = "sessions";
    if !std::path::Path::new(dir).exists() {
        return Ok(Json(Vec::new()));
    }
    let mut sessions = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".json") { continue; }
        let id = name.trim_end_matches(".json").to_string();
        let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap_or_default();
        let topic = v.get("topic").and_then(|t| t.as_str()).unwrap_or("").to_string();
        sessions.push(SessionInfo { id: id.clone(), timestamp: id, topic });
    }
    sessions.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(Json(sessions))
}

async fn load_session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SessionLoadResponse>, (StatusCode, String)> {
    let path = format!("sessions/{id}.json");
    let content = std::fs::read_to_string(&path)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("会话不存在: {e}")))?;
    let v: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("解析失败: {e}")))?;

    let topic = v.get("topic").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let article_type = v.get("article_type").and_then(|t| t.as_str()).map(|s| s.to_string());
    let draft = v.get("draft").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let annotations: Vec<AnnotationItem> = v.get("annotations")
        .and_then(|a| serde_json::from_value(a.clone()).ok())
        .unwrap_or_default();

    if let Some(mats) = v.get("materials").and_then(|m| m.as_array()) {
        let mut materials = state.materials.lock().await;
        materials.clear();
        for m in mats {
            let name = m.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
            let size = m.get("size").and_then(|s| s.as_u64()).unwrap_or(0) as usize;
            materials.push(MaterialEntry { name, content, size });
        }
    }

    if let Some(photos) = v.get("photos").and_then(|p| p.as_array()) {
        let mut photos_state = state.photos.lock().await;
        photos_state.clear();
        for p in photos {
            let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let data_url = p.get("data_url").and_then(|d| d.as_str()).unwrap_or("").to_string();
            let description = p.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string();
            let width = p.get("width").and_then(|w| w.as_u64()).unwrap_or(0) as u32;
            let height = p.get("height").and_then(|h| h.as_u64()).unwrap_or(0) as u32;
            photos_state.push(PhotoEntry { name, data_url, description, width, height });
        }
    }

    let tracker = state.cost_tracker.lock().await;
    let stats = StatsResponse {
        call_count: tracker.call_count,
        total_input_tokens: tracker.total_input_tokens,
        total_output_tokens: tracker.total_output_tokens,
        total_tokens: tracker.total_tokens(),
        total_cost: tracker.total_cost,
        total_cost_cny: tracker.total_cost * 7.2,
    };
    drop(tracker);

    let materials = state.materials.lock().await;
    let photos = state.photos.lock().await;
    let mat_list: Vec<MaterialInfo> = materials.iter().map(|m| MaterialInfo { name: m.name.clone(), size: m.size }).collect();
    let photo_list: Vec<PhotoInfo> = photos.iter().map(|p| PhotoInfo { name: p.name.clone(), data_url: p.data_url.clone(), description: p.description.clone(), width: p.width, height: p.height }).collect();

    Ok(Json(SessionLoadResponse {
        topic,
        article_type,
        draft,
        annotations,
        materials: mat_list,
        photos: photo_list,
        stats,
    }))
}

// === Publish to WeChat draft ===

#[derive(Deserialize)]
struct PublishRequest {
    #[serde(default)]
    title: Option<String>,
    html: String,
    draft: Option<String>,
}

#[derive(Serialize)]
struct PublishResponse {
    media_id: String,
    title: String,
}

async fn publish_draft(
    State(state): State<AppState>,
    Json(req): Json<PublishRequest>,
) -> Result<Json<PublishResponse>, (StatusCode, String)> {
    let config = state.config.read().await.clone();
    let app_id = config.wx_app_id.clone().unwrap_or_default();
    let app_secret = config.wx_app_secret.clone().unwrap_or_default();

    if app_id.is_empty() || app_secret.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "未配置公众号 AppID/AppSecret，请在设置页填写".into()));
    }

    // Auto-generate title if not provided
    let title = match req.title.as_deref().map(|t| t.trim()) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => generate_title(&config, &req.draft.unwrap_or_default()).await,
    };

    let photos = state.photos.lock().await;
    let mut photo_data: Vec<(String, Vec<u8>, String)> = Vec::new();
    for p in photos.iter() {
        let parts: Vec<&str> = p.data_url.splitn(2, ',').collect();
        if parts.len() != 2 { continue; }
        let header = parts[0];
        let b64 = parts[1];
        let content_type = header.strip_prefix("data:").and_then(|s| s.split(';').next()).unwrap_or("image/jpeg");
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, b64)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("图片解码失败: {e}")))?;
        photo_data.push((p.name.clone(), bytes, content_type.to_string()));
    }
    drop(photos);

    let client = crate::wechat::WeChatClient::new(&app_id, &app_secret);
    let media_id = client.push_draft(&title, &req.html, &photo_data).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(PublishResponse { media_id, title }))
}

async fn generate_title(config: &ModelConfig, draft: &str) -> String {
    let messages = vec![
        crate::llm::Message::system("根据文章内容生成公众号标题。要求：10字以内，直接输出标题文字本身，不要引号、不要书名号、不要解释、不要前缀、不要标点符号结尾。例如输入'今天去吃了热干面'，输出'热干面里的武汉味'。"),
        crate::llm::Message::user(draft.chars().take(500).collect::<String>()),
    ];
    let client = LlmClient::new(config.clone());
    client.chat_with_max_tokens(&messages, 50).await.map(|r| r.content.trim().chars().take(12).collect()).unwrap_or_else(|_| "未命名文章".into())
}

// === Download as ZIP ===

#[derive(Deserialize)]
struct DownloadZipRequest {
    html: String,
}

async fn download_zip(
    State(state): State<AppState>,
    Json(req): Json<DownloadZipRequest>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    use std::io::Write;
    let buf: Vec<u8> = Vec::new();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(buf));
    let opts = zip::write::SimpleFileOptions::default();

    // HTML with relative image paths
    let mut html = req.html.clone();
    let photos = state.photos.lock().await;
    for (idx, p) in photos.iter().enumerate() {
        let idx = idx + 1;
        let placeholder = format!("{{{{photo:{}}}}}", p.name);
        let ext = p.data_url.split(';').next().and_then(|s| s.split('/').nth(1)).unwrap_or("jpeg");
        let img_ref = format!("images/img{idx}.{ext}");
        html = html.replace(&placeholder, &format!("<img src=\"{img_ref}\" style=\"width:100%;border-radius:8px;margin:10px 0\" />"));

        zip.start_file(format!("images/img{idx}.{ext}"), opts)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let parts: Vec<&str> = p.data_url.splitn(2, ',').collect();
        if parts.len() == 2 {
            let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, parts[1])
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("解码失败: {e}")))?;
            zip.write_all(&bytes).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
    }
    drop(photos);

    zip.start_file("article.html", opts)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    zip.write_all(html.as_bytes()).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let result = zip.finish().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let bytes = result.into_inner();

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/zip"), (header::CONTENT_DISPOSITION, "attachment; filename=\"article.zip\"")],
        bytes,
    ))
}
