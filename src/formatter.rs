use crate::llm::Message;

const PROMPTS_DIR: &str = "prompts";

fn read_prompt(name: &str) -> String {
    let path = format!("{PROMPTS_DIR}/{name}");
    std::fs::read_to_string(&path).unwrap_or_default()
}

/// 枚举模板：支持两种布局
/// 1) 文件夹式（首选）：prompts/templates/{id}/template.md + assets/
/// 2) 旧平铺兼容：prompts/templates/{id}.md
pub fn all_template_id_dirs() -> Vec<(String, String)> {
    let dir = format!("{PROMPTS_DIR}/templates");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path.file_stem() else { continue };
            let id = stem.to_string_lossy().to_string();
            if path.is_dir() {
                let inner = path.join("template.md");
                if inner.exists() {
                    out.push((id, inner.to_string_lossy().to_string()));
                }
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push((id, path.to_string_lossy().to_string()));
            }
        }
    }
    out.sort();
    out
}

/// Template id + display name (template.md first line) for /api/templates;
/// scans both folder-style and legacy flat-md layouts.
pub fn list_template_ids() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (id, md_path) in all_template_id_dirs() {
        let content = std::fs::read_to_string(&md_path).unwrap_or_default();
        let first = content.lines().next().map(str::trim).unwrap_or("");
        let name = if first.is_empty() { id.clone() } else { first.to_string() };
        out.push((id, name));
    }
    out
}

pub struct FormatTask {
    pub draft: String,
    pub photos: Vec<PhotoRef>,
    pub template: Option<String>,
}

pub struct PhotoRef {
    pub name: String,
    pub description: String,
    pub width: u32,
    pub height: u32,
    /// 用户勾选的"必选"照片
    pub required: bool,
}

/// 横图/竖图/方图标注，宽高未知时返回空串
fn orientation(width: u32, height: u32) -> String {
    if width == 0 || height == 0 {
        return String::new();
    }
    let orient = if width >= height * 5 / 4 {
        "横图"
    } else if height >= width * 5 / 4 {
        "竖图"
    } else {
        "方图"
    };
    format!("{orient} {width}x{height}，")
}

impl FormatTask {
    pub fn build_messages(&self) -> Vec<Message> {
        let mut system = read_prompt("format_system.md");

        // 把每个模板的完整定义（样式片段 + 贴图素材用法）注入 system prompt，
        // 让 AI 真正"看到"可照搬的样式细节，而不是只有一个名字
        let template_ids = all_template_id_dirs();
        if !template_ids.is_empty() {
            system.push_str("\n\n可用模板库（");
            if let Some(t) = &self.template {
                system.push_str(&format!("用户已指定模板: {t}，必须沿用该模板的样式与贴图，不要改配色"));
            } else {
                system.push_str("根据文章内容自动选择最合适的一个模板");
            }
            system.push_str("）：\n");
            for (id, md_path) in &template_ids {
                let content = std::fs::read_to_string(md_path).unwrap_or_default();
                system.push_str(&format!("\n====== 模板 {id} ======\n{content}\n"));
            }
            system.push_str("\n请完整使用选中模板中的配色与 style 片段（照搬 style 属性值），贴图素材直接使用其 {{asset:...}} 占位符。");
        }

        let mut user_content = String::from("请将以下文章排版为公众号 HTML：\n\n");
        user_content.push_str(&self.draft);
        user_content.push_str("\n\n");

        if !self.photos.is_empty() {
            user_content.push_str("照片列表（用 {{photo:文件名}} 标记配图位置）：\n");
            for p in &self.photos {
                let tag = if p.required { "必选" } else { "可选" };
                let desc = if p.description.is_empty() { "（无描述）" } else { &p.description };
                user_content.push_str(&format!("- {}（{}，{}）：{}\n", p.name, tag, orientation(p.width, p.height), desc));
            }
            user_content.push_str("\n标记为「必选」的照片必须全部用上，放到文章最合适的位置；「可选」照片由你根据文章内容自行决定是否使用。\n");
        } else {
            user_content.push_str("（本次无照片提供）");
        }

        vec![Message::system(system), Message::user(user_content)]
    }
}

pub fn extract_html(content: &str) -> String {
    let mut html = content.trim().to_string();

    if html.starts_with("```") {
        if let Some(pos) = html.find('\n') {
            html = html[pos + 1..].to_string();
        }
        if html.ends_with("```") {
            html = html[..html.len() - 3].trim_end().to_string();
        }
    }

    if let Some(start) = html.find('<') {
        html = html[start..].to_string();
    } else {
        return String::new();
    }

    if let Some(end) = html.rfind('>') {
        html = html[..=end].to_string();
    }

    html.trim().to_string()
}
