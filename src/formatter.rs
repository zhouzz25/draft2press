use crate::llm::Message;

const PROMPTS_DIR: &str = "prompts";

fn read_prompt(name: &str) -> String {
    let path = format!("{PROMPTS_DIR}/{name}");
    std::fs::read_to_string(&path).unwrap_or_default()
}

fn read_templates() -> Vec<(String, String)> {
    let dir = format!("{PROMPTS_DIR}/templates");
    let mut templates = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "md") {
                continue;
            }
            let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let first_line = content.lines().next().unwrap_or("").to_string();
            let desc = content.lines().find(|l| l.starts_with("适合")).unwrap_or("").to_string();
            templates.push((name, format!("{first_line} {desc}")));
        }
    }
    templates
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

        let templates = read_templates();
        if !templates.is_empty() {
            system.push_str("\n\n可用模板（");
            if let Some(t) = &self.template {
                system.push_str(&format!("使用指定模板: {t}"));
            } else {
                system.push_str("根据文章内容自动选择最合适的模板");
            }
            system.push_str("）：\n");
            for (name, desc) in &templates {
                system.push_str(&format!("- {name}: {desc}\n"));
            }
            system.push_str("\n请严格按照选中模板的配色和样式规范生成 HTML。");
        }

        let mut user_content = String::from("请将以下文章排版为公众号 HTML：\n\n");
        user_content.push_str(&self.draft);
        user_content.push_str("\n\n");

        if !self.photos.is_empty() {
            user_content.push_str("可选照片（请根据描述和文章内容选择合适配图位置，用 {{photo:文件名}} 标记）：\n");
            for p in &self.photos {
                let desc = if p.description.is_empty() { "（无描述）" } else { &p.description };
                user_content.push_str(&format!("- {}（{}）：{}\n", p.name, orientation(p.width, p.height), desc));
            }
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
