use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use std::time::{Instant, Duration};

pub struct WeChatClient {
    app_id: String, app_secret: String, http: reqwest::Client,
    cached_token: Arc<RwLock<Option<(String, Instant)>>>,
}

#[derive(Deserialize)] struct TokenResp { access_token: Option<String>, #[serde(default)] errcode: i64, #[serde(default)] errmsg: String }
#[derive(Deserialize)] struct ImgResp { #[serde(default)] url: Option<String>, #[serde(default)] errcode: i64, #[serde(default)] errmsg: String }
#[derive(Deserialize)] struct CoverResp { #[serde(default)] media_id: Option<String>, #[serde(default)] errcode: i64, #[serde(default)] errmsg: String }
#[derive(Deserialize)] struct DraftResp { #[serde(default)] media_id: Option<String>, #[serde(default)] errcode: i64, #[serde(default)] errmsg: String }

impl WeChatClient {
    pub fn new(app_id: &str, app_secret: &str) -> Self {
        Self { app_id: app_id.into(), app_secret: app_secret.into(),
            http: reqwest::Client::builder().timeout(Duration::from_secs(60)).build().unwrap(),
            cached_token: Arc::new(RwLock::new(None)) }
    }

    async fn get_token(&self) -> Result<String> {
        { let g = self.cached_token.read().await;
          if let Some((t, c)) = g.as_ref() && c.elapsed().as_secs() < 6000 { return Ok(t.clone()); } }
        let url = format!("https://api.weixin.qq.com/cgi-bin/token?grant_type=client_credential&appid={}&secret={}", self.app_id, self.app_secret);
        let r: TokenResp = self.http.get(&url).send().await?.json().await.map_err(|e| anyhow!("token 失败: {e}"))?;
        match r.access_token {
            Some(t) => { *self.cached_token.write().await = Some((t.clone(), Instant::now())); Ok(t) }
            None => Err(anyhow!("token 错误: {} - {}", r.errcode, r.errmsg))
        }
    }

    async fn upload(&self, url: &str, data: &[u8], filename: &str, ct: &str) -> Result<String> {
        let part = reqwest::multipart::Part::bytes(data.to_vec()).file_name(filename.to_string()).mime_str(ct).unwrap();
        let form = reqwest::multipart::Form::new().part("media", part);
        let resp = self.http.post(url).multipart(form).send().await?;
        let body = resp.text().await?;
        eprintln!("[wechat] 上传响应: {}", &body[..body.len().min(200)]);
        Ok(body)
    }

    async fn upload_cover(&self, token: &str, data: &[u8], name: &str, ct: &str) -> Result<String> {
        let url = format!("https://api.weixin.qq.com/cgi-bin/material/add_material?access_token={}&type=image", token);
        let body = self.upload(&url, data, name, ct).await?;
        let r: CoverResp = serde_json::from_str(&body).map_err(|e| anyhow!("封面解析: {e}"))?;
        if r.errcode != 0 { return Err(anyhow!("封面错误: {} - {}", r.errcode, r.errmsg)); }
        r.media_id.ok_or_else(|| anyhow!("封面未返回 media_id"))
    }

    async fn upload_img(&self, token: &str, data: &[u8], name: &str, ct: &str) -> Result<String> {
        let url = format!("https://api.weixin.qq.com/cgi-bin/media/uploadimg?access_token={}", token);
        let body = self.upload(&url, data, name, ct).await?;
        let r: ImgResp = serde_json::from_str(&body).map_err(|e| anyhow!("图片解析: {e}"))?;
        if r.errcode != 0 { return Err(anyhow!("图片错误: {} - {}", r.errcode, r.errmsg)); }
        r.url.ok_or_else(|| anyhow!("图片未返回 url"))
    }

    pub async fn push_draft(&self, title: &str, content: &str, photos: &[(String, Vec<u8>, String)]) -> Result<String> {
        let token = self.get_token().await?;
        if photos.is_empty() { return Err(anyhow!("需要至少一张照片作为封面图")); }
        // First {{photo:name}} that is a real photo, skipping template sticker pseudonyms
        let mut cover_name = photos[0].0.clone();
        let mut s = content;
        while let Some(p) = s.find("{{photo:") {
            let inner = &s[p + 8..];
            let Some(e) = inner.find("}}") else { break };
            let name = inner[..e].to_string();
            if !name.starts_with("__tpl_asset_") { cover_name = name; break; }
            s = &inner[e + 2..];
        }
        eprintln!("[wechat] 封面: {cover_name}");
        let ci = photos.iter().position(|(n,_,_)| *n == cover_name).unwrap_or(0);
        let (cn, cd, cc) = &photos[ci];
        let thumb = self.upload_cover(&token, cd, cn, cc).await?;
        let mut urls = HashMap::new();
        for (name, data, ct) in photos {
            let url = self.upload_img(&token, data, name, ct).await?;
            urls.insert(name.clone(), url.clone());
            eprintln!("[wechat] 图片: {name} -> {url}");
        }
        let mut html = content.to_string();
        for (name, url) in &urls {
            // 模板贴纸贴在 AI 的定位包装层内：让 img 跟随包装宽高，不做全宽卡图样式
            let style = if name.starts_with("__tpl_asset_") { "width:100%;display:block" } else { "width:100%;border-radius:8px" };
            html = html.replace(&format!("{{{{photo:{name}}}}}"), &format!("<img src=\"{url}\" style=\"{style}\" />"));
        }
        let html = compress_html(&html);
        let body = serde_json::json!({ "articles": [{ "title": title, "content": html, "thumb_media_id": thumb, "need_open_comment": 0, "only_fans_can_comment": 0 }] });
        let url = format!("https://api.weixin.qq.com/cgi-bin/draft/add?access_token={}", token);
        let resp = self.http.post(&url).json(&body).send().await?;
        let text = resp.text().await?;
        eprintln!("[wechat] 草稿: {}", &text[..text.len().min(300)]);
        let r: DraftResp = serde_json::from_str(&text).map_err(|e| anyhow!("草稿解析: {e}"))?;
        if r.errcode != 0 { return Err(anyhow!("草稿错误: {} - {}", r.errcode, r.errmsg)); }
        r.media_id.ok_or_else(|| anyhow!("草稿未返回 media_id"))
    }
}

fn compress_html(html: &str) -> String {
    let mut r = html.to_string();
    while let Some(s) = r.find("<style") { if let Some(e) = r[s..].find("</style>") { r = format!("{}{}", &r[..s], &r[s+e+8..]); } else { break; } }
    while let Some(s) = r.find("<!--") { if let Some(e) = r[s..].find("-->") { r = format!("{}{}", &r[..s], &r[s+e+3..]); } else { break; } }
    let mut c = String::with_capacity(r.len()); let mut sp = false;
    for ch in r.chars() { if ch.is_whitespace() { if !sp { c.push(' '); sp = true; } } else { c.push(ch); sp = false; } }
    let c = c.replace("> <", "><");
    eprintln!("[wechat] HTML 压缩: {} -> {}", html.len(), c.len());
    if c.len() > 19000 { c[..19000].to_string() } else { c }
}
