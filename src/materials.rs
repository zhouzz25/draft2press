use anyhow::{anyhow, Context, Result};
use std::io::Read;

const DEFAULT_CHUNK_SIZE: usize = 2000;
const DEFAULT_CONTEXT_CHARS: usize = 20000;

pub struct PhotoEntry {
    pub name: String,
    pub data_url: String,
    pub description: String,
    pub width: u32,
    pub height: u32,
}

pub struct Material {
    pub name: String,
    #[allow(dead_code)]
    pub content: String,
    pub chunks: Vec<String>,
}

pub struct MaterialLibrary {
    materials: Vec<Material>,
    chunk_size: usize,
}

impl MaterialLibrary {
    pub fn new() -> Self {
        Self {
            materials: Vec::new(),
            chunk_size: DEFAULT_CHUNK_SIZE,
        }
    }

    #[allow(dead_code)]
    pub fn add_file(&mut self, path: &str) -> Result<()> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("读取素材文件失败: {}", path))?;
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(path)
            .to_string();
        self.add_text(&name, &content);
        Ok(())
    }

    pub fn add_text(&mut self, name: &str, content: &str) {
        let chunks = chunk_text(content, self.chunk_size);
        self.materials.push(Material {
            name: name.to_string(),
            content: content.to_string(),
            chunks,
        });
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.materials.is_empty()
    }

    pub fn build_context(&self) -> String {
        self.build_context_with_limit(DEFAULT_CONTEXT_CHARS)
    }

    pub fn build_context_with_limit(&self, max_chars: usize) -> String {
        let mut result = String::new();
        let mut remaining = max_chars;

        for mat in &self.materials {
            if remaining == 0 {
                break;
            }
            let header = format!("--- 素材: {} ---\n", mat.name);
            if header.len() >= remaining {
                break;
            }
            result.push_str(&header);
            remaining -= header.len();

            for chunk in &mat.chunks {
                if chunk.len() >= remaining {
                    let truncated = safe_truncate(chunk, remaining.saturating_sub(3));
                    result.push_str(truncated);
                    result.push_str("...\n");
                    remaining = 0;
                    break;
                }
                result.push_str(chunk);
                result.push('\n');
                remaining -= chunk.len() + 1;
            }
            result.push('\n');
        }

        if result.is_empty() {
            String::new()
        } else {
            format!("[参考资料]\n{result}")
        }
    }
}

fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub fn extract_text(filename: &str, data: &[u8]) -> Result<String> {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "txt" | "md" => Ok(String::from_utf8_lossy(data).into_owned()),
        "docx" => extract_docx_text(data),
        "pdf" => extract_pdf_text(data),
        _ => Ok(String::from_utf8_lossy(data).into_owned()),
    }
}

fn extract_docx_text(data: &[u8]) -> Result<String> {
    let cursor = std::io::Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| anyhow!("无法打开 docx: {e}"))?;

    let mut xml = String::new();
    let mut doc = archive
        .by_name("word/document.xml")
        .map_err(|e| anyhow!("docx 中找不到 word/document.xml: {e}"))?;
    doc.read_to_string(&mut xml)?;

    Ok(extract_text_from_docx_xml(&xml))
}

fn extract_text_from_docx_xml(xml: &str) -> String {
    let mut paragraphs: Vec<String> = Vec::new();

    for para in xml.split("</w:p>") {
        let mut para_text = String::new();
        let mut remaining = para;

        while let Some(start) = remaining.find("<w:t") {
            remaining = &remaining[start..];
            let Some(tag_end) = remaining.find('>') else {
                break;
            };
            if remaining[..tag_end].ends_with('/') {
                remaining = &remaining[tag_end + 1..];
                continue;
            }
            remaining = &remaining[tag_end + 1..];
            let Some(close) = remaining.find("</w:t>") else {
                break;
            };
            let text = &remaining[..close];
            if !para_text.is_empty() {
                para_text.push(' ');
            }
            para_text.push_str(text);
            remaining = &remaining[close + 6..];
        }

        if !para_text.is_empty() {
            paragraphs.push(decode_xml_entities(&para_text));
        }
    }

    paragraphs.join("\n\n")
}

fn decode_xml_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

fn extract_pdf_text(data: &[u8]) -> Result<String> {
    use std::io::Write;
    let temp_path = std::env::temp_dir().join(format!("xiumi_pdf_{}.pdf", std::process::id()));
    {
        let mut f = std::fs::File::create(&temp_path)
            .map_err(|e| anyhow!("创建临时文件失败: {e}"))?;
        f.write_all(data)
            .map_err(|e| anyhow!("写入临时文件失败: {e}"))?;
    }
    let result = pdf_extract::extract_text(&temp_path)
        .map_err(|e| anyhow!("PDF 文本提取失败: {e}"));
    let _ = std::fs::remove_file(&temp_path);
    result
}

fn chunk_text(text: &str, chunk_size: usize) -> Vec<String> {
    let paragraphs: Vec<&str> = text.split("\n\n").collect();
    let mut chunks = Vec::new();
    let mut current = String::new();

    for para in paragraphs {
        if para.len() > chunk_size {
            if !current.is_empty() {
                chunks.push(current.clone());
                current.clear();
            }
            let words: Vec<&str> = para.split_whitespace().collect();
            for word in words {
                if current.len() + word.len() + 1 > chunk_size
                    && !current.is_empty()
                {
                    chunks.push(current.trim().to_string());
                    current.clear();
                }
                current.push_str(word);
                current.push(' ');
            }
        } else if current.len() + para.len() + 2 > chunk_size {
            if !current.is_empty() {
                chunks.push(current.clone());
                current.clear();
            }
            current.push_str(para);
        } else {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(para);
        }
    }

    if !current.is_empty() {
        chunks.push(current.trim().to_string());
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_short_text() {
        let chunks = chunk_text("这是一段短文本", 2000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "这是一段短文本");
    }

    #[test]
    fn chunk_long_text_splits() {
        let long = "word ".repeat(5000);
        let chunks = chunk_text(&long, 2000);
        assert!(chunks.len() > 1);
    }

    #[test]
    fn chunk_paragraphs() {
        let text = "第一段\n\n第二段\n\n第三段";
        let chunks = chunk_text(text, 2000);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("第一段"));
        assert!(chunks[0].contains("第三段"));
    }

    #[test]
    fn library_context_empty() {
        let lib = MaterialLibrary::new();
        assert!(lib.is_empty());
        assert_eq!(lib.build_context(), "");
    }

    #[test]
    fn library_context_with_material() {
        let mut lib = MaterialLibrary::new();
        lib.add_text("公告.txt", "这是一份官方公告内容。");
        let ctx = lib.build_context();
        assert!(ctx.contains("公告.txt"));
        assert!(ctx.contains("官方公告"));
    }

    #[test]
    fn library_context_truncation() {
        let mut lib = MaterialLibrary::new();
        lib.add_text("大文件.txt", &"内容内容".repeat(1000));
        let ctx = lib.build_context_with_limit(100);
        assert!(ctx.len() <= 200);
    }

    #[test]
    fn extract_txt_file() {
        let data = b"hello world";
        let text = extract_text("test.txt", data).unwrap();
        assert!(text.contains("hello world"));
    }

    #[test]
    fn extract_docx_from_test_file() {
        let docx = std::fs::read_dir("test")
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "docx"))
            .unwrap();
        let data = std::fs::read(&docx).unwrap();
        let text = extract_text(docx.to_str().unwrap(), &data).unwrap();
        let preview: String = text.chars().take(500).collect();
        eprintln!("=== DOCX 提取结果 (前500字) ===");
        eprintln!("{preview}");
        assert!(text.len() > 100, "docx 提取结果太短: {} 字", text.len());
    }

    #[test]
    fn extract_pdf_from_test_file() {
        let path = "test/20260728111526-闽北乡村生态旅游开发研讨-纪要文本-1.pdf";
        if !std::path::Path::new(path).exists() {
            eprintln!("跳过 pdf 测试：文件不存在");
            return;
        }
        let data = std::fs::read(path).unwrap();
        let text = extract_text(path, &data);
        match text {
            Ok(t) => {
                eprintln!("=== PDF 提取结果 (前500字) ===");
                eprintln!("{}", &t[..t.len().min(500)]);
                assert!(t.len() > 50, "pdf 提取结果太短: {} 字", t.len());
            }
            Err(e) => {
                panic!("PDF 提取失败: {e}");
            }
        }
    }
}
