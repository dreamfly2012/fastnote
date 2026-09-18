//! 各类 AI 动作的消息构造。
//!
//! 所有 prompt 都明确要求「只输出结果本身」，因为结果会直接写回编辑器，
//! 任何寒暄或解释都会污染笔记正文。

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn system(c: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: c.into(),
        }
    }
    pub fn user(c: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: c.into(),
        }
    }
    pub fn assistant(c: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: c.into(),
        }
    }
}

/// 编辑器内可触发的 AI 动作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Task {
    /// 行内续写（Ghost Text）
    Complete,
    /// 润色选中文本
    Polish,
    Translate { target: String },
    Summarize,
    Explain,
    /// 命令面板里的自由指令
    Custom { instruction: String },
    /// 侧栏对话，可带笔记库检索到的上下文
    Chat { context: Option<String> },
}

const EDITOR_SYSTEM: &str = "你是嵌在 Markdown 笔记编辑器里的写作助手。\
规则：1) 只输出结果本身，不要任何前言、解释或代码围栏包裹；\
2) 保持原文语言，除非要求翻译；3) 输出使用 Markdown 语法；\
4) 不要重复用户已经写过的内容。";

const COMPLETE_SYSTEM: &str = "你是 Markdown 笔记的行内续写引擎。\
根据光标前的内容自然地续写下去。规则：\
1) 只输出续写部分，不要重复已有文本；\
2) 最多两句话，简洁克制；\
3) 不要添加标题或列表符号，除非上文正处于列表中；\
4) 不要输出任何解释。";

const CHAT_SYSTEM: &str = "你是本地笔记库的问答助手。\
优先依据给出的笔记片段回答；片段中没有的信息要明确说明，不要编造。\
回答使用 Markdown，简洁直接。";

/// 光标前后各取多少字符作为续写上下文。取太多会拖慢首字延迟。
const COMPLETE_BEFORE: usize = 1200;
const COMPLETE_AFTER: usize = 300;

pub fn build(task: &Task, selected: &str) -> Vec<Message> {
    match task {
        Task::Complete => vec![
            Message::system(COMPLETE_SYSTEM),
            Message::user(format!("续写以下内容：\n\n{selected}")),
        ],
        Task::Polish => vec![
            Message::system(EDITOR_SYSTEM),
            Message::user(format!(
                "润色下面这段文字，让表达更清晰流畅，保持原意与信息量，不要扩写：\n\n{selected}"
            )),
        ],
        Task::Translate { target } => vec![
            Message::system(EDITOR_SYSTEM),
            Message::user(format!(
                "把下面内容翻译成{target}，保留 Markdown 结构与代码内容不变：\n\n{selected}"
            )),
        ],
        Task::Summarize => vec![
            Message::system(EDITOR_SYSTEM),
            Message::user(format!(
                "为下面内容写一段摘要，用要点列表，不超过 5 条：\n\n{selected}"
            )),
        ],
        Task::Explain => vec![
            Message::system(EDITOR_SYSTEM),
            Message::user(format!("解释下面内容的含义与关键点：\n\n{selected}")),
        ],
        Task::Custom { instruction } => {
            if selected.trim().is_empty() {
                vec![
                    Message::system(EDITOR_SYSTEM),
                    Message::user(instruction.clone()),
                ]
            } else {
                vec![
                    Message::system(EDITOR_SYSTEM),
                    Message::user(format!("{instruction}\n\n---\n\n{selected}")),
                ]
            }
        }
        Task::Chat { context } => {
            let mut msgs = vec![Message::system(CHAT_SYSTEM)];
            match context {
                Some(ctx) if !ctx.trim().is_empty() => {
                    msgs.push(Message::user(format!(
                        "以下是笔记库中的相关片段：\n\n{ctx}\n\n---\n\n问题：{selected}"
                    )));
                }
                _ => msgs.push(Message::user(selected.to_string())),
            }
            msgs
        }
    }
}

/// 为行内续写截取光标附近的上下文，避免把整篇笔记都发出去。
pub fn completion_context(full_text: &str, cursor_byte: usize) -> String {
    // 光标本身也要吸附到字符边界：按字节偏移换算来的下标可能落在多字节字符中间，
    // 直接拿它切片会 panic（`&s[a..b]` 要求两端都是 char boundary）。
    let cursor = floor_char_boundary(full_text, cursor_byte.min(full_text.len()));
    let start = floor_char_boundary(full_text, cursor.saturating_sub(COMPLETE_BEFORE));
    let before = &full_text[start..cursor];

    let after_end = ceil_char_boundary(full_text, (cursor + COMPLETE_AFTER).min(full_text.len()));
    let after = &full_text[cursor..after_end];

    if after.trim().is_empty() {
        before.to_string()
    } else {
        // 用显式标记告诉模型光标位置，续写质量比只给前文更稳
        format!("{before}<CURSOR>{after}\n\n（在 <CURSOR> 处继续写，只输出插入的内容）")
    }
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polish_includes_selection_and_system_rule() {
        let m = build(&Task::Polish, "一段话");
        assert_eq!(m[0].role, "system");
        assert!(m[1].content.contains("一段话"));
    }

    #[test]
    fn custom_without_selection_sends_instruction_only() {
        let m = build(
            &Task::Custom {
                instruction: "写个周报模板".into(),
            },
            "",
        );
        assert_eq!(m[1].content, "写个周报模板");
    }

    #[test]
    fn chat_with_context_embeds_snippets() {
        let m = build(
            &Task::Chat {
                context: Some("片段A".into()),
            },
            "问题？",
        );
        assert!(m[1].content.contains("片段A"));
        assert!(m[1].content.contains("问题？"));
    }

    #[test]
    fn completion_context_never_splits_multibyte_char() {
        let text = "中".repeat(2000); // 每字 3 字节，边界都在 3 的倍数上
        let cursor = 3001; // 落在一个字符中间
        let ctx = completion_context(&text, cursor);
        // 能正常构造且不 panic 即说明边界处理正确
        assert!(!ctx.is_empty());
        // 吸附到前一个边界：与显式传入合法下标的结果一致
        assert_eq!(ctx, completion_context(&text, 3000));
        // 越界下标同样被夹取，不 panic
        assert!(!completion_context(&text, text.len() + 10).is_empty());
    }

    #[test]
    fn completion_context_marks_cursor_when_text_follows() {
        let ctx = completion_context("前面内容后面还有很多字", 6);
        assert!(ctx.contains("<CURSOR>"));
    }

    #[test]
    fn completion_context_at_end_has_no_marker() {
        let s = "只有前文";
        let ctx = completion_context(s, s.len());
        assert!(!ctx.contains("<CURSOR>"));
        assert_eq!(ctx, s);
    }
}
