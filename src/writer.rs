use crate::llm::Message;

const PROMPTS_DIR: &str = "prompts";

pub fn read_prompt(name: &str) -> String {
    let path = format!("{PROMPTS_DIR}/{name}");
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("警告：读取 prompt 文件失败 {path}: {e}，使用空 prompt");
        String::new()
    })
}

pub struct WritingTask {
    pub topic: String,
    pub article_type: Option<String>,
    pub materials_context: String,
}

impl WritingTask {
    pub fn build_messages(&self) -> Vec<Message> {
        let system = Message::system(read_prompt("system.md"));

        let article_type_line = self
            .article_type
            .as_ref()
            .filter(|s| !s.is_empty())
            .map(|s| format!("文章类型：{s}\n"))
            .unwrap_or_default();

        let template = read_prompt("user_prompt.md");
        let user_content = template
            .replace("{topic}", &self.topic)
            .replace("{article_type}", &article_type_line)
            .replace("{materials}", &self.materials_context);

        vec![system, Message::user(user_content)]
    }
}

pub struct Annotation {
    pub selected_text: String,
    pub comment: String,
}

pub fn build_revision_messages(draft: &str, annotations: &[Annotation]) -> Vec<Message> {
    let system = Message::system(read_prompt("revision_system.md"));

    let (selected_text, comment) = annotations
        .first()
        .map(|a| (a.selected_text.as_str(), a.comment.as_str()))
        .unwrap_or(("", ""));

    let template = read_prompt("revision_user.md");
    let user_content = template
        .replace("{draft}", draft)
        .replace("{selected_text}", selected_text)
        .replace("{comment}", comment);

    vec![system, Message::user(user_content)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writing_task_without_article_type() {
        let task = WritingTask {
            topic: "Rust 异步编程入门".into(),
            article_type: None,
            materials_context: String::new(),
        };
        let messages = task.build_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert!(messages[1].content.contains("Rust 异步编程入门"));
        assert!(!messages[1].content.contains("文章类型"));
    }

    #[test]
    fn writing_task_with_article_type() {
        let task = WritingTask {
            topic: "测试选题".into(),
            article_type: Some("活动报道".into()),
            materials_context: "[参考资料]\n--- 素材: 公告.txt ---\n活动时间地点\n".into(),
        };
        let messages = task.build_messages();
        assert!(messages[1].content.contains("活动报道"));
        assert!(messages[1].content.contains("参考资料"));
    }

    #[test]
    fn revision_messages_contain_draft_and_annotations() {
        let draft = "# 测试标题\n\n这是初稿内容。";
        let annotations = vec![
            Annotation {
                selected_text: "这是初稿内容。".into(),
                comment: "太干了，加个例子".into(),
            },
        ];
        let messages = build_revision_messages(draft, &annotations);
        assert_eq!(messages.len(), 2);
        assert!(messages[1].content.contains("初稿"));
        assert!(messages[1].content.contains("批注"));
        assert!(messages[1].content.contains("太干了"));
    }
}
