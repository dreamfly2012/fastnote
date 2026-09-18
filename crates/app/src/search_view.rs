//! 库内全文搜索面板。
//!
//! 复用命令面板的 `CmdInput` 作为 IME 友好的输入框；每次输入即实时搜索，
//! 结果点击后通过 `OpenNoteAt` 动作交给 Workspace 打开对应笔记并跳转到行。
//! 关闭走 `command_palette::Close`（与命令面板同一套 Esc / 点遮罩逻辑）。

use fastnote_core::index::{SearchHit, VaultIndex};
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement,
    InteractiveElement, MouseButton, MouseDownEvent, ParentElement, Render, Styled,
    Window, actions, div, px,
};
use crate::command_palette::{Close, CmdInput, Submit};
use crate::OpenNoteAt;
use crate::theme::{Metrics, Theme};
use crate::ui;
use crate::When;

actions!(search_view, []);

pub struct SearchView {
    pub input: Entity<CmdInput>,
    index: VaultIndex,
    theme: Theme,
    focus: FocusHandle,
}

impl SearchView {
    pub fn new(theme: Theme, index: VaultIndex, initial: String, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| CmdInput::new(theme, cx));
        if !initial.is_empty() {
            input.update(cx, |i, _cx| {
                i.set_text(&initial);
            });
        }
        let focus = cx.focus_handle();
        Self {
            input,
            index,
            theme,
            focus,
        }
    }

    fn run_search(&self, cx: &App) -> Vec<SearchHit> {
        let q = self.input.read(cx).text();
        if q.trim().is_empty() {
            return Vec::new();
        }
        self.index.search(&q, 5, 50)
    }

    fn on_submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        let hits = self.run_search(cx);
        if let Some(h) = hits.into_iter().next() {
            window.dispatch_action(Box::new(OpenNoteAt(h.path, h.line)), cx);
        }
    }

}

// 打开结果 / 关闭面板都走窗口动作派发，由 Workspace 的 `.on_action` 接住。

impl Focusable for SearchView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SearchView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let hits = self.run_search(cx);
        let input = self.input.clone();

        ui::scrim(theme)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_sv: &mut SearchView, _ev: &MouseDownEvent, window, cx| {
                    window.dispatch_action(Box::new(Close), cx);
                }),
            )
            .key_context("SearchView")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_submit))
            .child(
                ui::panel(theme, px(620.))
                    .max_h(px(520.))
                    // 停止冒泡，否则点面板内部会连带触发遮罩的关闭
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_sv: &mut SearchView, _ev: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation()
                        }),
                    )
                    .child(div().px(px(14.)).py(px(11.)).child(input))
                    .child(
                        div()
                            .border_t_1()
                            .border_color(theme.rule)
                            .flex_col()
                            .overflow_hidden()
                            .max_h(px(440.))
                            .py(px(6.))
                            .when(hits.is_empty(), |d| {
                                d.child(
                                    div()
                                        .px(px(14.))
                                        .py(px(12.))
                                        .text_size(px(13.))
                                        .text_color(theme.muted)
                                        .child("输入关键词搜索全部笔记（文件名 / 标题 / 正文）"),
                                )
                            })
                            .children(hits.into_iter().enumerate().map(|(i, h)| {
                                let path = h.path.clone();
                                let line = h.line;
                                let name = h.name.clone();
                                let snippet = h.snippet.clone();
                                let count = i;
                                div()
                                    .id(count)
                                    .mx(px(6.))
                                    .px(px(9.))
                                    .py(px(7.))
                                    .rounded(Metrics::RADIUS_BTN)
                                    .text_size(px(13.))
                                    .cursor(gpui::CursorStyle::PointingHand)
                                    .hover(|s| s.bg(theme.hover))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .text_color(theme.heading)
                                                    .child(name.clone()),
                                            )
                                            .child(ui::kbd(
                                                theme,
                                                format!(
                                                    "{}:{}",
                                                    path.file_name()
                                                        .map(|s| s.to_string_lossy().to_string())
                                                        .unwrap_or_default(),
                                                    line + 1
                                                ),
                                            )),
                                    )
                                    .child(
                                        div()
                                            .pt(px(2.))
                                            .text_size(px(12.))
                                            .text_color(theme.muted)
                                            .truncate()
                                            .child(snippet),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |_sv: &mut SearchView, _ev: &MouseDownEvent, window, cx| {
                                            window.dispatch_action(Box::new(OpenNoteAt(path.clone(), line)), cx);
                                        }),
                                    )
                            })),
                    ),
            )
    }
}

// 让 rgba! 宏可用（和 command_palette 保持一致）
