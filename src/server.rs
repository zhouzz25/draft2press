use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{header, StatusCode},
    response::{
        sse::{Event, Sse},
        Html, IntoResponse, Json,
    },
    routing::{delete, get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    annotation: AnnotationItem,
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
        .route("/api/photos/{name}/required", post(set_photo_required))
        .route("/api/write", post(write_article))
        .route("/api/revise", post(revise_article))
        .route("/api/format", post(format_article))
        .route("/api/download-zip", post(download_zip))
        .route("/api/import", post(import_article))
        .route("/api/cancel", post(cancel_task))
        .route("/api/config", get(get_config).post(update_config))
        .route("/api/settings/check", post(check_settings_dirty))
        .route("/api/templates", get(list_templates).post(generate_template))
        .route("/api/templates/{id}", delete(delete_template))
        .route("/api/templates/{id}/asset", post(upload_template_asset))
        .route("/api/templates/{id}/assets", get(list_template_assets))
        .route("/api/assets/{id}/{file}", get(serve_asset))
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

// Runs an LLM call as a cancellable SSE task with streaming progress:
// a ticker task samples the generated-char counter every 500ms and pushes
// progress events ("已生成 N 字...") while the stream is alive.
async fn run_llm_with_cancel(
    state: &AppState, tx: &tokio::sync::mpsc::Sender<Result<Event, Infallible>>,
    client: LlmClient, messages: Vec<crate::llm::Message>,
) -> Option<crate::llm::LlmResponse> {
    let n = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    { let mut g = state.cancel_tx.lock().await; *g = Some(cancel_tx); }
    let ticker_tx = tx.clone();
    let n2 = n.clone();
    let ticker = tokio::spawn(async move {
        let mut last = (0usize, 0usize);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let cur = (n2.0.load(Ordering::Relaxed), n2.1.load(Ordering::Relaxed));
            if cur == last { continue; }
            last = cur;
            let msg = if cur.0 > 0 { format!("已生成 {} 字...", cur.0) } else { format!("思考中 {} 字...", cur.1) };
            if ticker_tx.send(progress_event(&msg)).await.is_err() { break; }
        }
    });
    // All select branches fall through to ticker.abort() so the sender is always
    // dropped and the SSE stream can terminate (otherwise cancel would hang).
    let outcome = tokio::select! {
        r = client.chat_stream(&messages, move |p| { n.0.store(p.content, Ordering::Relaxed); n.1.store(p.reasoning, Ordering::Relaxed); }) => Ok(r),
        _ = cancel_rx => Err(()),
    };
    ticker.abort();
    { let mut g = state.cancel_tx.lock().await; *g = None; }
    let result = match outcome {
        Err(_) => {
            let _ = tx.send(Ok(Event::default().event("cancelled").data("{}"))).await;
            return None;
        }
        Ok(Err(e)) => {
            let _ = tx.send(error_event(&e.to_string())).await;
            return None;
        }
        Ok(Ok(r)) => r,
    };
    if result.usage.total_tokens == 0 {
        eprintln!("[llm] 该端点流式响应未返回 usage，本次调用未计入 Token 统计");
    }
    { let mut t = state.cost_tracker.lock().await; t.record(&result.usage); }
    if check_budget(state, tx).await { return None; }
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
        let response = match run_llm_with_cancel(&state_c, &tx, client, messages.clone()).await { Some(r) => r, None => return };
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
        let ann = Annotation { selected_text: req.annotation.selected_text, comment: req.annotation.comment };
        let messages = build_revision_messages(&draft, std::slice::from_ref(&ann));
        send_progress(&tx, "正在调用 AI 模型处理本条批注...").await;
        let config = state_c.config.read().await.clone();
        let client = LlmClient::new(config);
        let response = match run_llm_with_cancel(&state_c, &tx, client, messages.clone()).await { Some(r) => r, None => return };
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
    // API key 不回显（连掩码也不给，避免任何字节泄漏），只暴露是否已配置
    Json(serde_json::json!({
        "endpoint": config.endpoint,
        "model": config.model,
        "context_length": config.context_length,
        "thinking_mode": config.thinking_mode,
        "temperature": config.temperature,
        "max_tokens": config.max_tokens,
        "api_key_configured": !config.api_key.is_empty(),
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

// === Style templates: list / AI-generate from natural language + reference link / delete ===

async fn list_templates() -> Json<serde_json::Value> {
    let list = crate::formatter::list_template_ids()
        .into_iter()
        .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
        .collect::<Vec<_>>();
    Json(serde_json::json!(list))
}

#[derive(Deserialize)]
struct TemplateGenRequest {
    description: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

fn sanitize_template_id(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect();
    let id = if cleaned.is_empty() {
        format!("custom_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
    } else {
        cleaned
    };
    if id.starts_with("custom_") { id } else { format!("custom_{id}") }
}

// Fetch a reference page: plain text for the LLM + decorative stickers (gif or small
// width) candidates with their size hints. WeChat articles lazy-load via data-src.
#[derive(Debug, Clone)]
struct ImageCandidate {
    url: String,
    w: u32,
    ratio: f64,
}

fn find_img_attr(tag: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    let sp = tag.find(&needle)?;
    let after = &tag[sp + needle.len()..];
    let first = after.chars().next()?;
    let (quote, off) = if first == '"' || first == '\'' { (first, 1) } else { (first, 0) };
    let s = &after[off..];
    let end = s.find(quote)?;
    Some(s[..end].trim().to_string())
}

async fn fetch_page(url: &str) -> Result<(String, Vec<ImageCandidate>), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let html = client
        .get(url)
        .header(header::USER_AGENT, "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36")
        .header(header::ACCEPT, "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
        .header(header::ACCEPT_LANGUAGE, "zh-CN,zh;q=0.9")
        .send()
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;

    let (origin, page_dir) = {
        let after_scheme = url.split("://").nth(1).unwrap_or(url);
        let origin_end = after_scheme.find('/').map(|p| url.len() - after_scheme.len() + p).unwrap_or(url.len());
        let origin = &url[..origin_end];
        let page_dir = url.rsplit_once('/').map(|(a, _)| a.to_string()).unwrap_or_else(|| url.to_string());
        (origin.to_string(), page_dir)
    };

    let mut imgs: Vec<ImageCandidate> = Vec::new();
    let mut rest = html.as_str();
    while imgs.len() < 16 && !rest.is_empty() {
        let start = match rest.find("<img") { Some(p) => p, None => break };
        let tail = &rest[start + 4..];
        let len = tail.find('>').unwrap_or(0);
        let tag = &tail[..len];
        // WeChat lazy-loads real src in data-src; fall back to src. Also grab
        // data-w/data-ratio so stickers (small width) can be told apart from content photos.
        if let Some(raw) = find_img_attr(tag, "data-src").or_else(|| find_img_attr(tag, "src")) {
            let src = if raw.starts_with("//") { format!("https:{raw}") }
                else if raw.starts_with("http://") || raw.starts_with("https://") { raw.clone() }
                else if raw.starts_with('/') { format!("{origin}{raw}") }
                else { format!("{page_dir}/{raw}") };
            if src.starts_with("http://") || src.starts_with("https://") {
                let w = find_img_attr(tag, "data-w").and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                let ratio = find_img_attr(tag, "data-ratio").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
                imgs.push(ImageCandidate { url: src, w, ratio });
            }
        }
        rest = &tail[len..];
    }

    let mut plain = String::new();
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => plain.push(' '),
            '>' => { plain.push(' '); in_tag = false; }
            c if !in_tag => plain.push(c),
            _ => {}
        }
    }
    let joined = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    let plain: String = joined.chars().take(3000).collect();
    if (html.contains("环境异常") || html.contains("该内容暂时无法浏览") || html.contains("request denied"))
        && imgs.is_empty() && plain.chars().count() < 800 {
        return Err("该链接被微信要求安全验证，抓不到正文和贴纸。请把文章正文直接粘贴到「风格描述」里，或换一篇可以直接打开的文章链接".into());
    }
    if plain.chars().count() < 200 && imgs.is_empty() {
        return Err("参考页面内容过少（可能是保护页或空页），请换链接或直接粘贴正文".into());
    }
    // WeChat CDN sometimes blocks direct fetches; that's a real failure here
    if imgs.len() >= 2 && html.contains("mmbiz.qpic.cn") {
        // keep going: images may still be downloadable individually
    }
    Ok((plain, imgs))
}

// Stickers are tiny decorations: gif animations or small-width images.
// Content photos (large width) are user-provided via {{photo:...}} and skipped here.
fn is_sticker_candidate(c: &ImageCandidate) -> bool {
    let fmt = fmt_kind(&c.url);
    fmt == "gif" || (c.w > 0 && c.w <= 400)
}

// Get image kind. WeChat puts the real type in the wx_fmt query (path only has /640!),
// and rewriting `tp=webp` makes bytes differ from the claimed type – strip it.
fn fmt_kind(url: &str) -> String {
    let query = url.split_once('?').map(|x| x.1).unwrap_or("");
    for pair in query.split('&') {
        if let Some(v) = pair.strip_prefix("wx_fmt=") {
            return v.split('#').next().unwrap_or("").to_lowercase();
        }
    }
    let path = url.split('?').next().unwrap_or("").split('#').next().unwrap_or(url);
    path.rsplit('.').next().unwrap_or("").to_lowercase()
}

fn normalize_img_url(url: &str) -> String {
    let no_frag = url.split_once('#').map(|(a, _)| a).unwrap_or(url);
    if fmt_kind(no_frag) == "gif" {
        no_frag.replace("&tp=webp", "").replace("?tp=webp&", "?").replace("&tp=webp&", "&").to_string()
    } else {
        no_frag.to_string()
    }
}

async fn download_template_assets(id: &str, cands: &[ImageCandidate]) -> Vec<(String, u32, f64)> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let mut saved: Vec<(String, u32, f64)> = Vec::new();
    for c in cands.iter() {
        if !is_sticker_candidate(c) { continue; }
        let fmt = fmt_kind(&c.url);
        if !matches!(fmt.as_str(), "png" | "gif" | "jpg" | "jpeg" | "webp") { continue; }
        let dl_url = normalize_img_url(&c.url);
        let Ok(r) = client.get(&dl_url).send().await else { eprintln!("[template] 贴纸下载请求失败: {dl_url}"); continue };
        if !r.status().is_success() { eprintln!("[template] 贴纸下载非 2xx: {dl_url}"); continue; }
        let ct_ok = r.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok())
            .map(|v| v.starts_with("image/")).unwrap_or(true);
        if !ct_ok { continue; }
        let Ok(bytes) = r.bytes().await else { continue };
        if bytes.is_empty() || bytes.len() > 4_800_000 { continue; }
        let file = format!("asset{}.{}", saved.len() + 1, fmt);
        if save_asset(id, &file, &bytes).await.is_ok() {
            eprintln!("[template] 已采集贴纸 {file}（{}KB，w={}）", bytes.len() / 1024, c.w);
            saved.push((file, c.w, c.ratio));
        }
        if saved.len() >= 24 { break; }
    }
    saved
}

// Natural language (+optional reference URL) -> AI writes a template definition
// in the exact same structure as prompts/templates/*.md -> saved as custom_xxx.md.
// Decorative images collected from the reference page are saved LOCALLY under
// prompts/templates/{id}/assets/ so no hotlinked external image ever ships –
// the export pipeline translates {{asset:...}} into base64 / WeChat-uploaded URLs.
async fn generate_template(
    State(state): State<AppState>,
    Json(req): Json<TemplateGenRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let desc = req.description.trim().to_string();
    if desc.chars().count() < 6 {
        return Err((StatusCode::BAD_REQUEST, "风格描述太短，请写清楚想要的颜色气质、用的场景等".into()));
    }
    let id = sanitize_template_id(req.name.as_deref().unwrap_or(""));

    let mut user = format!("用户想要的模板风格：{desc}\n");
    let mut asset_assets: Vec<(String, u32, f64)> = Vec::new();
    if let Some(url) = req.url.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let (text, cands) = fetch_page(url).await.map_err(|e| (StatusCode::BAD_REQUEST, format!("参考链接不可用：{e}")))?;
        user.push_str(&format!("\n参考页面内容节选（参考其配色与气质）：\n{text}\n"));
        asset_assets = download_template_assets(&id, &cands).await;
        if !asset_assets.is_empty() {
            user.push_str(&format!("\n以下贴纸素材已从参考页面采集并保存到本地（id={}）；注意大尺寸内容照片不会被采集（那类图排版时走 {{photo:...}}）。如需贴纸装饰，请直接嵌入这些占位符：\n{}", id, asset_placeholder_lines(&id, &asset_assets)));
        }
    }

    let messages = vec![
        crate::llm::Message::system(crate::writer::read_prompt("template_gen.md")),
        crate::llm::Message::user(user),
    ];
    let config = state.config.read().await.clone();
    let client = LlmClient::new(config);
    let resp = match client.chat_stream(&messages, |_| {}).await {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_dir_all(format!("prompts/templates/{id}"));
            return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("模板生成失败: {e}")));
        }
    };

    // 有些模型会先把思考过程写出来：从"模板名 + 主色"色板行处裁掉前面的闲话
    let content = if let Some(ci) = resp.content.find("主色") {
        let name_start = resp.content[..ci]
            .rfind('\n')
            .and_then(|p| resp.content[..p].rfind('\n').map(|p2| p2 + 1))
            .unwrap_or(0);
        resp.content[name_start..].trim().to_string()
    } else {
        resp.content.trim().to_string()
    };
    if content.len() < 100 || !content.contains('<') {
        let _ = std::fs::remove_dir_all(format!("prompts/templates/{id}"));
        let preview: String = content.chars().take(200).collect();
        return Err((StatusCode::INTERNAL_SERVER_ERROR, format!("AI 生成的模板内容无效：{preview}")));
    }

    let mut content = content;
    if !asset_assets.is_empty() {
        content.push_str(&format!("\n\n==== 可用贴纸素材，逐个编写具体使用方法 ====\n（大尺寸照片不在列，排版时会走 {{photo:...}}；以下为本地贴纸：）\n{}", asset_placeholder_lines(&id, &asset_assets)));
    }
    std::fs::create_dir_all(format!("prompts/templates/{id}/assets"))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let path = format!("prompts/templates/{id}/template.md");
    std::fs::write(&path, &content).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let name = content.lines().next().map(str::trim).unwrap_or(&id).to_string();
    Ok(Json(serde_json::json!({ "id": id, "name": name, "assets": asset_assets.iter().map(|(f, _, _)| f.clone()).collect::<Vec<_>>() })))
}

// One line per sticker for the LLM prompt / template appendix, e.g.
// "{{asset:id/asset1.gif}}（GIF 动画贴纸，约 230×172px）"
fn asset_placeholder_lines(id: &str, assets: &[(String, u32, f64)]) -> String {
    assets.iter().map(|(f, w, ratio)| {
        let fmt = f.rsplit('.').next().unwrap_or("png");
        let size = if *w > 0 {
            let h = if *ratio > 0.0 { (*w as f64 / *ratio).round() as u32 } else { 0 };
            if h > 0 { format!("约 {}×{}px", w, h) } else { format!("宽约 {}px", w) }
        } else {
            "小尺寸".into()
        };
        let kind = if fmt == "gif" { "GIF 动画贴纸" } else { "静态贴纸" };
        format!("{{{{asset:{id}/{f}}}}}（{kind}，{size}）\n")
    }).collect()
}

async fn delete_template(Path(id): Path<String>) -> Result<StatusCode, (StatusCode, String)> {
    if !id.starts_with("custom_") {
        return Err((StatusCode::BAD_REQUEST, "内置模板不可删除".into()));
    }
    // Folder layout AND legacy flat-md layouts both exit cleanly
    let _ = std::fs::remove_dir_all(format!("prompts/templates/{id}"));
    let _ = std::fs::remove_file(format!("prompts/templates/{id}.md"));
    if !crate::formatter::all_template_id_dirs().iter().any(|(tid, _)| *tid == id) {
        Ok(StatusCode::OK)
    } else {
        Err((StatusCode::INTERNAL_SERVER_ERROR, "删除失败".into()))
    }
}

// === Template local assets (stickers/backgrounds) ===

fn valid_template_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn valid_asset_file(file: &str) -> bool {
    !file.is_empty()
        && file != "." && file != ".."
        && !file.contains('/') && !file.contains('\\')
        && file.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
        && !file.starts_with('.')
}

fn asset_dir(id: &str) -> String {
    format!("prompts/templates/{id}/assets")
}

fn asset_content_type(file: &str) -> &'static str {
    match file.rsplit('.').next().unwrap_or("") {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "image/jpeg",
    }
}

async fn save_asset(id: &str, file: &str, bytes: &[u8]) -> Result<(), String> {
    let dir = asset_dir(id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(format!("{dir}/{file}"), bytes).map_err(|e| e.to_string())
}

async fn list_template_assets(Path(id): Path<String>) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !valid_template_id(&id) {
        return Err((StatusCode::BAD_REQUEST, "非法模板 id".into()));
    }
    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(asset_dir(&id)) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if valid_asset_file(&n) {
                names.push(serde_json::json!({ "file": n, "placeholder": format!("{{{{asset:{id}/{n}}}}}") }));
            }
        }
    }
    Ok(Json(serde_json::json!(names)))
}

async fn upload_template_asset(
    State(_state): State<AppState>,
    Path(id): Path<String>,
    mut multipart: Multipart,
) -> Result<StatusCode, (StatusCode, String)> {
    if !valid_template_id(&id) {
        return Err((StatusCode::BAD_REQUEST, "非法模板 id".into()));
    }
    if !id.starts_with("custom_") {
        return Err((StatusCode::BAD_REQUEST, "仅自定义模板可上传素材".into()));
    }
    let mut saved = 0usize;
    while let Some(field) = multipart.next_field().await.map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))? {
        if field.name() != Some("file") { continue; }
        let name = field.file_name().unwrap_or("").to_string();
        if !valid_asset_file(&name) { continue; }
        let data = field.bytes().await.map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        if data.len() > 2 * 1024 * 1024 { return Err((StatusCode::BAD_REQUEST, "单张素材需小于 2 MB".into())); }
        save_asset(&id, &name, &data).await.map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        saved += 1;
    }
    if saved == 0 { return Err((StatusCode::BAD_REQUEST, "未收到有效素材".into())); }
    Ok(StatusCode::OK)
}

async fn serve_asset(
    Path((id, file)): Path<(String, String)>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    if !valid_template_id(&id) || !valid_asset_file(&file) {
        return Err((StatusCode::BAD_REQUEST, "非法素材路径".into()));
    }
    let bytes = std::fs::read(format!("{}/{file}", asset_dir(&id)))
        .map_err(|_| (StatusCode::NOT_FOUND, "素材不存在".into()))?;
    Ok(axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, asset_content_type(&file))
        .header(header::CACHE_CONTROL, "max-age=600")
        .body(axum::body::Body::from(bytes))
        .unwrap())
}

// {{asset:id/file}} placeholders -> (full, id, file). The export pipelines turn
// these into base64 data URLs / WeChat-uploaded URLs so nothing external is needed.
fn collect_asset_refs(html: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(p) = rest.find("{{asset:") {
        let start = p + "{{asset:".len();
        let Some(end) = rest[start..].find("}}") else { break };
        let spec = &rest[start..start + end];
        if let Some(slash) = spec.find('/') {
            let (id, file) = (spec[..slash].trim(), spec[slash + 1..].trim());
            if valid_template_id(id) && valid_asset_file(file) {
                out.push((rest[p..start + end + 2].to_string(), id.to_string(), file.to_string()));
            }
        }
        rest = &rest[start + end + 2..];
    }
    out
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
    required: bool,
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
            required: p.required,
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
        photos.push(PhotoEntry { name: filename, data_url, description: String::new(), width, height, required: false });
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

#[derive(Deserialize)]
struct RequiredUpdate {
    required: bool,
}

/// 通用的图片筛选方式：用户可勾选"必选"，全选时所有照片都会被 AI 用于排版，
/// 未勾选的照片则由 AI 按文章内容自行取舍
async fn set_photo_required(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(req): Json<RequiredUpdate>,
) -> StatusCode {
    let mut photos = state.photos.lock().await;
    if let Some(photo) = photos.iter_mut().find(|p| p.name == name) {
        photo.required = req.required;
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
        let photo_refs: Vec<PhotoRef> = photos.iter().map(|p| PhotoRef { name: p.name.clone(), description: p.description.clone(), width: p.width, height: p.height, required: p.required }).collect();
        drop(photos);
        send_progress(&tx, &format!("正在准备排版（{} 张可选照片）...", photo_refs.len())).await;
        let task = FormatTask { draft: req.draft, photos: photo_refs, template: req.template };
        let messages = task.build_messages();
        send_progress(&tx, "正在调用 AI 生成排版 HTML...").await;
        let config = state_c.config.read().await.clone();
        let client = LlmClient::new(config);
        let response = match run_llm_with_cancel(&state_c, &tx, client, messages.clone()).await { Some(r) => r, None => return };
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
            serde_json::json!({"name": p.name, "data_url": p.data_url, "description": p.description, "width": p.width, "height": p.height, "required": p.required})
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
            let required = p.get("required").and_then(|r| r.as_bool()).unwrap_or(false);
            photos_state.push(PhotoEntry { name, data_url, description, width, height, required });
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
    let photo_list: Vec<PhotoInfo> = photos.iter().map(|p| PhotoInfo { name: p.name.clone(), data_url: p.data_url.clone(), description: p.description.clone(), width: p.width, height: p.height, required: p.required }).collect();

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

    // Template local assets: {{asset:id/file}} -> pseudo {{photo:__tpl_asset_x.ext}}
    // so they ride the same WeChat material upload pipeline as user photos
    let mut html = req.html.clone();
    let mut asset_entries: Vec<(String, Vec<u8>, String)> = Vec::new();
    for (i, (full, id, file)) in collect_asset_refs(&html).into_iter().enumerate() {
        let path = format!("{}/{file}", asset_dir(&id));
        match std::fs::read(&path) {
            Ok(bytes) => {
                let ext = file.rsplit('.').next().unwrap_or("png");
                let pseudo = format!("__tpl_asset_{i}.{ext}");
                let ct = asset_content_type(&file);
                html = html.replace(&full, &format!("{{{{photo:{pseudo}}}}}"));
                asset_entries.push((pseudo, bytes, ct.to_string()));
            }
            Err(_) => html = html.replace(&full, ""),
        }
    }

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
    photo_data.extend(asset_entries);

    let client = crate::wechat::WeChatClient::new(&app_id, &app_secret);
    let media_id = client.push_draft(&title, &html, &photo_data).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(PublishResponse { media_id, title }))
}

async fn generate_title(config: &ModelConfig, draft: &str) -> String {
    // 注意：只喂原文（markdown 草稿），绝不送排版后的 HTML（那里样式占 90% 的 token）
    let messages = vec![
        crate::llm::Message::system(
            "你是标题机器。任务：为文章生成一个 10 字内的公众号标题。\n\
             铁律：你的回复将由程序直接当作标题使用，任何多余字符都会被发布出去。\n\
             - 只写标题文字本身；输出前后一个字都不能多\n\
             - 禁止：引号 书名号 句号省略号等结尾标点 「标题：」等前缀 markdown 符号 解释 语气词哆嗦\n\
             - 示例\n  输入：讲一场校园歌手大赛的幕后趣事与获奖名单\n  输出：校园歌手大赛落幕\n\
             （注意：例子里输出就是标题四个字，没有「好的」「标题：」这些字）",
        ),
        crate::llm::Message::user(draft.chars().take(500).collect::<String>()),
    ];
    let client = LlmClient::new(config.clone());
    let raw = client.chat_with_max_tokens(&messages, 60).await.map(|r| r.content).unwrap_or_default();

    // 无脑后处理：任何模型都压回"标题本体"
    let sanitized: String = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with(['-', '*', '>']))
        .unwrap_or_else(|| raw.trim())
        .trim_start_matches(['【', '】'])
        .to_string();
    let mut t = sanitized
        .replace("《", "").replace("》", "")
        .replace("「", "").replace("」", "")
        .replace("\"", "").replace("'", "")
        .replace("标题：", "").replace("标题:", "")
        .replace("题目：", "").replace("题目:", "")
        .replace("标题为：", "")
        .trim()
        .to_string();
    while t.ends_with('。') || t.ends_with('.') || t.ends_with('！') || t.ends_with('！') || t.ends_with('？') || t.ends_with('?') || t.ends_with('，') || t.ends_with(',') || t.ends_with(' ') { t.pop(); }
    let t: String = t.chars().take(20).collect();
    if t.chars().count() < 2 { "未命名文章".into() } else { t }
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

    // Template local assets: pack the files into the zip as images/assetN.ext
    let mut asset_files: Vec<(String, String, String, Vec<u8>)> = Vec::new(); // (placeholder, zipname, ct, bytes)
    for (i, (full, id, file)) in collect_asset_refs(&html).into_iter().enumerate() {
        let path = format!("{}/{file}", asset_dir(&id));
        match std::fs::read(&path) {
            Ok(bytes) => {
                let ext = file.rsplit('.').next().unwrap_or("png");
                let zipname = format!("images/asset{0}.{ext}", i + 1);
                let ct = asset_content_type(&file);
                html = html.replace(&full, &format!("<img src=\"{zipname}\" style=\"width:100%;display:block\" />"));
                asset_files.push((full, zipname, ct.to_string(), bytes));
            }
            Err(_) => html = html.replace(&full, ""),
        }
    }
    for (_, zipname, _, bytes) in &asset_files {
        zip.start_file(zipname.clone(), opts)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        zip.write_all(bytes).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

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
