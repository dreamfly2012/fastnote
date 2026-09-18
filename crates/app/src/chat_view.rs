//! 本地知识库问答浮层（RAG）。
//!
//! 提问 → 用 `VaultIndex::search` 在**本地**笔记里检索片段 → 片段作为上下文交给 AI。
//!
//! 设计取舍：不引入向量库。笔记载体是 Markdown，检索要的是"能定位到行"，
//! 子串匹配 + 标题优先已经足够好用，而且索引可以随保存即时重建——
//! 上百 MB 依赖换来的召回提升，在这个规模上并不划算。
//!
//! 答案流式吐字，与编辑器里的 ghost text 走同一套 `Stream` + `drain()` 机制。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fastnote_ai::{AiConfig, AiEvent, Stream, Task};
use fastnote_core::index::{SearchHit, VaultIndex};
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Render, Styled, Window, actions, div, px,
};

use crate::OpenNoteAt;
use crate::When;
use crate::command_palette::{CmdInput, Close, Submit};
use crate::theme::{Metrics, Theme};
use crate::ui;

actions!(chat_view, []);

/// 送进模型的上下文上限（字符数）。超了直接截断，避免把整库发出去。
const CONTEXT_MAX_CHARS: usize = 4_000;
/// 每次提问每篇笔记最多取几个片段 / 总共取几个片段
const HITS_PER_NOTE: usize = 3;
const HITS_TOTAL: usize = 12;
/// 轮询 AI 流的间隔
const POLL_MS: u64 = 40;

/// 一轮问答。
struct Turn {
    question: String,
    answer: String,
    sources: Vec<SearchHit>,
}

pub struct ChatView {
    pub input: Entity<CmdInput>,
    index: VaultIndex,
    cfg: AiConfig,
    theme: Theme,
    focus: FocusHandle,
    turns: Vec<Turn>,
    stream: Option<Stream>,
    streaming: bool,
    /// 取消标志：关闭面板时置位，后台轮询循环立刻退出
    token: Arc<AtomicBool>,
}

impl ChatView {
    pub fn new(theme: Theme, index: VaultIndex, cfg: AiConfig, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| CmdInput::new(theme, cx));
        Self {
            input,
            index,
            cfg,
            theme,
            focus: cx.focus_handle(),
            turns: Vec::new(),
            stream: None,
            streaming: false,
            token: Arc::new(AtomicBool::new(false)),
        }
    }

    /// 把检索结果拼成给模型的上下文，带出处标注。
    fn build_context(hits: &[SearchHit]) -> String {
        let mut out = String::new();
        for h in hits {
            let line = h.snippet.trim();
            if line.is_empty() {
                continue;
            }
            let block = format!("### {} （第 {} 行）\n{}\n\n", h.name, h.line + 1, line);
            if out.len() + block.len() > CONTEXT_MAX_CHARS {
                break;
            }
            out.push_str(&block);
        }
        out
    }

    /// 中断进行中的请求（关闭面板 / 再次提问时调用）。
    fn cancel(&mut self) {
        self.token.store(true, Ordering::Relaxed);
        if let Some(s) = self.stream.as_ref() {
            s.cancel();
        }
        self.stream = None;
        self.streaming = false;
    }

    fn on_submit(&mut self, _: &Submit, _window: &mut Window, cx: &mut Context<Self>) {
        if self.streaming {
            return;
        }
        let question = self.input.read(cx).text().trim().to_string();
        if question.is_empty() {
            return;
        }
        self.input.update(cx, |i, cx| {
            i.set_text("");
            cx.notify();
        });

        // 本地检索：结果既要喂给模型，也要留在 UI 上作为出处
        let hits = self.index.search(&question, HITS_PER_NOTE, HITS_TOTAL);
        let context = Self::build_context(&hits);
        let context = if context.trim().is_empty() {
            None
        } else {
            Some(context)
        };

        if self.turns.is_empty() && !self.cfg.is_configured() {
            self.turns.push(Turn {
                question,
                answer: "尚未配置 AI 端点。按 Ctrl+Shift+, 打开设置，或设置环境变量 FASTNOTE_API_KEY。"
                    .into(),
                sources: hits,
            });
            cx.notify();
            return;
        }

        self.turns.push(Turn {
            question: question.clone(),
            answer: String::new(),
            sources: hits,
        });
        self.stream = Some(fastnote_ai::run_task(
            &self.cfg,
            &Task::Chat { context },
            &question,
        ));
        self.streaming = true;
        self.token = Arc::new(AtomicBool::new(false));
        let token = self.token.clone();

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(POLL_MS))
                    .await;
                if token.load(Ordering::Relaxed) {
                    return;
                }
                let events = match this
                    .update(cx, |v, _| v.stream.as_mut().map(|s| s.drain()).unwrap_or_default())
                {
                    Ok(e) => e,
                    Err(_) => return,
                };
                if events.is_empty() {
                    continue;
                }

                let mut finished = false;
                let mut error: Option<String> = None;
                let _ = this.update(cx, |v, cx| {
                    for ev in events {
                        match ev {
                            AiEvent::Delta(d) => {
                                if let Some(t) = v.turns.last_mut() {
                                    t.answer.push_str(&d);
                                }
                            }
                            AiEvent::Done => finished = true,
                            AiEvent::Error(e) => {
                                error = Some(e);
                                finished = true;
                            }
                        }
                    }
                    if let Some(e) = error {
                        if let Some(t) = v.turns.last_mut() {
                            if t.answer.is_empty() {
                                t.answer = format!("⚠️ {e}");
                            } else {
                                t.answer.push_str(&format!("\n\n⚠️ {e}"));
                            }
                        }
                    }
                    if finished {
                        v.stream = None;
                        v.streaming = false;
                    }
                    cx.notify();
                });
                if finished {
                    return;
                }
            }
        })
        .detach();

        cx.notify();
    }
}

impl Focusable for ChatView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Drop for ChatView {
    fn drop(&mut self) {
        self.token.store(true, Ordering::Relaxed);
    }
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let input = self.input.clone();
        let busy = self.streaming;

        let mut body = div().flex().flex_col().gap_3().px_3().py_3();
        if self.turns.is_empty() {
            body = body.child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.muted)
                    .child(
                        "问点什么，我会先在本地笔记里找相关片段，再依据片段回答（带出处）。",
                    ),
            );
        }

        for turn in self.turns.iter().enumerate() {
            let (i, t) = turn;
            let sources = t.sources.clone();
            let question = t.question.clone();
            let mut answer_block = div().flex().flex_col().text_size(px(13.)).text_color(theme.text);
            for line in t.answer.split('\n') {
                answer_block = answer_block.child(div().child(line.to_string()));
            }
            if t.answer.is_empty() {
                answer_block = answer_block.child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.muted)
                        .child("思考中…"),
                );
            }

            body = body.child(
                div()
                    .id(("turn", i as u32))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p(px(11.))
                    .rounded(Metrics::RADIUS_BTN)
                    .bg(theme.surface)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_size(px(13.))
                            .child(div().flex_none().text_color(theme.muted).child("问"))
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme.heading)
                                    .child(question),
                            ),
                    )
                    .child(answer_block)
                    .when(!sources.is_empty(), |d| {
                        let mut row = div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.muted)
                                    .child("出处："),
                            );
                        for (k, h) in sources.into_iter().enumerate() {
                            let path = h.path.clone();
                            let line = h.line;
                            let label = format!("{}:{}", h.name, h.line + 1);
                            row = row.child(
                                div()
                                    .id(("src", (i * 64 + k) as u32))
                                    .px(px(6.))
                                    .py(px(1.))
                                    .rounded(px(5.))
                                    .bg(theme.tag_bg)
                                    .text_size(px(11.))
                                    .text_color(theme.tag)
                                    .cursor(gpui::CursorStyle::PointingHand)
                                    .hover(|s| s.bg(theme.hover))
                                    .child(label)
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            move |_cv: &mut ChatView,
                                                  _ev: &MouseDownEvent,
                                                  window,
                                                  cx| {
                                                window.dispatch_action(
                                                    Box::new(OpenNoteAt(path.clone(), line)),
                                                    cx,
                                                );
                                            },
                                        ),
                                    ),
                            );
                        }
                        d.child(row)
                    }),
            );
        }

        ui::scrim(theme)
            .key_context("ChatView")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_submit))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|cv: &mut ChatView, _ev: &MouseDownEvent, window, cx| {
                    cv.cancel();
                    window.dispatch_action(Box::new(Close), cx);
                }),
            )
            .child(
                ui::panel(theme, px(720.))
                    .max_h(px(600.))
                    // 停止冒泡，否则点面板内部会连带触发遮罩的关闭
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_cv: &mut ChatView, _ev: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation()
                        }),
                    )
                    .child(
                        ui::header(theme)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(3.))
                                    .child(ui::title(theme, "库问答"))
                                    .child(ui::sub(
                                        theme,
                                        if busy {
                                            "生成中…（Esc 取消）"
                                        } else {
                                            "答案来自本地笔记检索，回车提问"
                                        },
                                    )),
                            )
                            .child(ui::icon_btn(
                                theme,
                                "chat-close",
                                "×",
                                cx.listener(
                                    |cv: &mut ChatView, _ev: &MouseDownEvent, window, cx| {
                                        cv.cancel();
                                        window.dispatch_action(Box::new(Close), cx);
                                    },
                                ),
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .max_h(px(430.))
                            .p(px(14.))
                            .overflow_hidden()
                            .child(body),
                    )
                    .child(
                        div()
                            .border_t_1()
                            .border_color(theme.rule)
                            .px(px(14.))
                            .py(px(11.))
                            .child(input),
                    ),
            )
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn hit(name: &str, line: usize, snippet: &str) -> SearchHit {
        SearchHit {
            path: PathBuf::from(format!("{name}.md")),
            name: name.into(),
            line,
            snippet: snippet.into(),
        }
    }

    #[test]
    fn context_lists_sources_with_line_numbers() {
        let ctx = ChatView::build_context(&[
            hit("索引设计", 3, "索引只做三件事"),
            hit("库问答设计", 7, "先检索再问"),
        ]);
        assert!(ctx.contains("### 索引设计 （第 4 行）"), "行号要转成 1-based：{ctx}");
        assert!(ctx.contains("### 库问答设计 （第 8 行）"));
        assert!(ctx.contains("索引只做三件事"));
    }

    #[test]
    fn context_skips_blank_snippets() {
        let ctx = ChatView::build_context(&[hit("空", 0, "   "), hit("有内容", 1, "正文")]);
        assert!(!ctx.contains("空"));
        assert!(ctx.contains("正文"));
    }

    #[test]
    fn context_is_capped() {
        // 每段约 500 字符，10 段必然超过 4000 上限
        let hits: Vec<SearchHit> = (0..10)
            .map(|i| hit(&format!("笔记{i}"), i, &"x".repeat(500)))
            .collect();
        let ctx = ChatView::build_context(&hits);
        assert!(
            ctx.len() <= CONTEXT_MAX_CHARS + 512,
            "上下文应在上限附近截断，实际 {}",
            ctx.len()
        );
        assert!(ctx.len() < 5000);
    }

    #[test]
    fn empty_hits_produce_empty_context() {
        assert!(ChatView::build_context(&[]).is_empty());
    }
}
