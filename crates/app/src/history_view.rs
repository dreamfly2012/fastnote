//! 历史版本浮层：列出一篇笔记的快照，可预览、可恢复。
//!
//! 快照由 core 的 `history` 模块管理（存在库内 `.fastnote/history/`），
//! 这里只做展示与"点恢复"的入口 —— 真正的写回交给 Workspace，
//! 因为只有它知道编辑器里此刻有没有未落盘的内容。

use std::path::PathBuf;

use fastnote_core::history::{self, Snapshot};
use gpui::{
    App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Render, Styled, Window, actions, div,
    px,
};

use crate::command_palette::Close;
use crate::theme::{Metrics, Theme};
use crate::ui;
use crate::{OpenNoteAt, RestoreSnapshot};
use crate::When;

actions!(history_view, []);

/// 预览最多显示的字符数：再长也没人看，还拖慢渲染。
const PREVIEW_MAX: usize = 3_000;

pub struct HistoryView {
    note: PathBuf,
    root: PathBuf,
    snaps: Vec<Snapshot>,
    selected: usize,
    preview: String,
    theme: Theme,
    focus: FocusHandle,
    notice: Option<String>,
}

impl HistoryView {
    pub fn open(theme: Theme, root: PathBuf, note: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut v = Self {
            note,
            root,
            snaps: Vec::new(),
            selected: 0,
            preview: String::new(),
            theme,
            focus: cx.focus_handle(),
            notice: None,
        };
        v.reload();
        v
    }

    /// 重新扫描快照并刷新预览（恢复之后要调用）。
    pub fn reload(&mut self) {
        self.snaps = history::list(&self.root, &self.note);
        self.selected = self.selected.min(self.snaps.len().saturating_sub(1));
        self.load_preview();
    }

    fn load_preview(&mut self) {
        self.preview = self
            .snaps
            .get(self.selected)
            .and_then(|s| history::read(&s.path).ok())
            .map(|s| {
                if s.chars().count() > PREVIEW_MAX {
                    let cut: String = s.chars().take(PREVIEW_MAX).collect();
                    format!("{cut}\n…（已截断）")
                } else {
                    s
                }
            })
            .unwrap_or_default();
    }

    /// 当前选中的快照路径。
    fn current(&self) -> Option<&Snapshot> {
        self.snaps.get(self.selected)
    }
}

impl Focusable for HistoryView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for HistoryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let file_name = self
            .note
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let count = self.snaps.len();
        let selected = self.selected;
        let stamp = self
            .current()
            .map(|s| s.stamp.clone())
            .unwrap_or_else(|| "—".into());
        let notice = self.notice.clone();

        let items: Vec<(usize, String, String)> = self
            .snaps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let size = if s.bytes < 1024 {
                    format!("{} B", s.bytes)
                } else {
                    format!("{:.1} KB", s.bytes as f64 / 1024.)
                };
                (i, s.stamp.clone(), size)
            })
            .collect();
        let preview = self.preview.clone();
        let note = self.note.clone();

        ui::scrim(theme)
            .key_context("HistoryView")
            .track_focus(&self.focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_v: &mut HistoryView, _ev: &MouseDownEvent, window, cx| {
                    window.dispatch_action(Box::new(Close), cx);
                }),
            )
            .child(
                ui::panel(theme, px(760.))
                    .max_h(px(560.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_v: &mut HistoryView, _ev: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation()
                        }),
                    )
                    .child(
                        ui::header(theme)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(ui::title(theme, format!("历史版本 · {file_name}")))
                                    .child(ui::sub(
                                        theme,
                                        format!("共 {count} 个版本{}", match &notice {
                                            Some(n) => format!(" · {n}"),
                                            None => String::new(),
                                        }),
                                    )),
                            )
                            .child(ui::icon_btn(
                                theme,
                                "hist-close",
                                "×",
                                cx.listener(
                                    |_v: &mut HistoryView,
                                     _ev: &MouseDownEvent,
                                     window,
                                     cx| {
                                        window.dispatch_action(Box::new(Close), cx);
                                    },
                                ),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .h(px(430.))
                            .bg(theme.surface)
                            // ---- 版本列表 ----
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(240.))
                                    .h_full()
                                    .flex_col()
                                    .py(px(6.))
                                    .overflow_hidden()
                                    .border_r_1()
                                    .border_color(theme.rule)
                                    .when(items.is_empty(), |d| {
                                        d.child(
                                            div()
                                                .px(px(12.))
                                                .py(px(10.))
                                                .child(ui::sub(
                                                    theme,
                                                    "还没有历史版本。保存这篇笔记时会自动留档。",
                                                )),
                                        )
                                    })
                                    .children(items.into_iter().map(|(i, stamp, size)| {
                                        let sel = i == selected;
                                        div()
                                            .id(("hist", i))
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap_2()
                                            .mx(px(6.))
                                            .px(px(9.))
                                            .py(px(6.))
                                            .rounded(Metrics::RADIUS_BTN)
                                            .text_size(px(12.))
                                            .when(sel, |d| d.bg(theme.active))
                                            .when(!sel, |d| d.hover(|s| s.bg(theme.hover)))
                                            .cursor(gpui::CursorStyle::PointingHand)
                                            .child(
                                                div()
                                                    .text_color(if sel {
                                                        theme.heading
                                                    } else {
                                                        theme.text
                                                    })
                                                    .child(stamp),
                                            )
                                            .child(
                                                div().flex_none().text_color(theme.muted).child(size),
                                            )
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(
                                                    move |v: &mut HistoryView,
                                                          _ev: &MouseDownEvent,
                                                          _w,
                                                          cx| {
                                                        v.selected = i;
                                                        v.load_preview();
                                                        v.notice = None;
                                                        cx.notify();
                                                    },
                                                ),
                                            )
                                    })),
                            )
                            // ---- 预览 ----
                            .child(
                                div()
                                    .flex_1()
                                    .h_full()
                                    .flex_col()
                                    .overflow_hidden()
                                    .child(
                                        div()
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .px(px(14.))
                                            .py(px(8.))
                                            .border_b_1()
                                            .border_color(theme.rule)
                                            .child(ui::sub(theme, "预览"))
                                            .child(ui::chip(theme, stamp.clone())),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .id("hist-preview")
                                            .overflow_hidden()
                                            .p(px(14.))
                                            .text_size(px(11.5))
                                            .text_color(theme.text)
                                            .child(preview),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .px(px(14.))
                            .py(px(10.))
                            .border_t_1()
                            .border_color(theme.rule)
                            .child(ui::sub(
                                theme,
                                "恢复前会把当前内容也留一份档，所以恢复本身也可以反悔",
                            ))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(ui::btn(
                                        theme,
                                        "hist-open",
                                        "打开这篇笔记",
                                        ui::Kind::Plain,
                                        cx.listener(
                                            move |_v: &mut HistoryView,
                                                  _ev: &MouseDownEvent,
                                                  window,
                                                  cx| {
                                                // 先打开笔记（当前文档可能早换了），再关掉本浮层，
                                                // 否则刚打开的笔记被浮层挡着看不见
                                                window.dispatch_action(
                                                    Box::new(OpenNoteAt(note.clone(), 0)),
                                                    cx,
                                                );
                                                window.dispatch_action(Box::new(Close), cx);
                                            },
                                        ),
                                    ))
                                    .child(ui::btn(
                                        theme,
                                        "hist-restore",
                                        "恢复此版本",
                                        ui::Kind::Primary,
                                        cx.listener(
                                            move |v: &mut HistoryView,
                                                  _ev: &MouseDownEvent,
                                                  window,
                                                  cx| {
                                                if let Some(snap) =
                                                    v.current().map(|s| s.path.clone())
                                                {
                                                    window.dispatch_action(
                                                        Box::new(RestoreSnapshot(
                                                            v.note.clone(),
                                                            snap,
                                                        )),
                                                        cx,
                                                    );
                                                }
                                            },
                                        ),
                                    )),
                            ),
                    ),
            )
    }
}

impl HistoryView {
    /// 恢复完成后由 Workspace 调用：刷新列表并给出提示。
    pub fn after_restore(&mut self, msg: impl Into<String>) {
        self.reload();
        self.notice = Some(msg.into());
    }
}
