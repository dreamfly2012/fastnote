//! 关系图谱浮层：力导向布局 + 连线绘制。
//!
//! 节点坐标由 core 的 `VaultIndex::graph` 算好并归一化到 `[0, 1]`，
//! 这里只做坐标映射，两层叠在同一块固定尺寸的画布上：
//!
//! - **连线**用 `canvas` + `paint_path` 画——div 画不出斜线；
//! - **节点**用绝对定位的 div——文字、hover、点击都好做。
//!
//! 之所以固定画布尺寸而不是跟随窗口：坐标换算要有个确定的基准，
//! 尺寸写死后节点与连线的对齐关系是编译期常量，不会出现缩放错位。

use fastnote_core::index::Graph;
use gpui::{
    App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, PathBuilder, Pixels, Render, Styled,
    Window, actions, canvas, div, point, px,
};

use crate::OpenNoteAt;
use crate::When;
use crate::command_palette::Close;
use crate::theme::{Metrics, Theme};
use crate::ui;

actions!(graph_view, []);

/// 画布尺寸，坐标换算的唯一基准。
const CANVAS_W: f32 = 880.;
const CANVAS_H: f32 = 510.;
/// 节点圆点直径，用于把连线端点对齐到圆心。
const DOT: f32 = 8.;
/// 节点标签最大宽度，避免长笔记名把画布撑破。
const LABEL_MAX_W: f32 = 132.;

/// 把 `base`（画布原点，窗口坐标）与局部像素偏移相加。
///
/// `Pixels` 是 `pub(crate)` 的 f32 且没有实现 `Add`，
/// 但实现了 `Div<Pixels> -> f32`，所以除以 `px(1.)` 就能取回原始数值。
fn abs(base: Pixels, local: f32) -> Pixels {
    px(base / px(1.) + local)
}

pub struct GraphView {
    graph: Graph,
    theme: Theme,
    focus: FocusHandle,
}

impl GraphView {
    pub fn new(theme: Theme, graph: Graph, cx: &mut Context<Self>) -> Self {
        Self {
            graph,
            theme,
            focus: cx.focus_handle(),
        }
    }

    /// 节点上限：超过这个数力导向布局会明显变慢，视觉上也没法看。
    pub const MAX_NODES: usize = 120;
}

impl Focusable for GraphView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for GraphView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let node_count = self.graph.nodes.len();
        let edge_count = self.graph.edges.len();

        // 连线端点：预先把归一化坐标算成画布局部像素，闭包里只做平移
        let segs: Vec<(f32, f32, f32, f32)> = self
            .graph
            .edges
            .iter()
            .filter_map(|e| {
                let a = self.graph.nodes.get(e.from)?;
                let b = self.graph.nodes.get(e.to)?;
                Some((
                    a.x * CANVAS_W + DOT / 2.,
                    a.y * CANVAS_H + DOT,
                    b.x * CANVAS_W + DOT / 2.,
                    b.y * CANVAS_H + DOT,
                ))
            })
            .collect();

        let line_color = theme.border;

        // 连线层：canvas 的 paint 回调在窗口坐标系里工作，
        // 需要用 bounds.origin 把局部坐标平移成窗口坐标。
        let edges_layer = canvas(
            move |bounds, _window, _cx| bounds,
            move |bounds, _state, window, _cx| {
                for (ax, ay, bx, by) in segs {
                    let mut builder = PathBuilder::stroke(px(1.));
                    builder.move_to(point(abs(bounds.origin.x, ax), abs(bounds.origin.y, ay)));
                    builder.line_to(point(abs(bounds.origin.x, bx), abs(bounds.origin.y, by)));
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, line_color);
                    }
                }
            },
        )
        .absolute()
        .inset_0();

        let nodes: Vec<_> = self.graph.nodes.iter().cloned().collect();

        ui::scrim(theme)
            .key_context("GraphView")
            .track_focus(&self.focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_gv: &mut GraphView, _ev: &MouseDownEvent, window, cx| {
                    window.dispatch_action(Box::new(Close), cx);
                }),
            )
            .child(
                ui::panel(theme, px(CANVAS_W + 24.))
                    // 吃掉点击：gpui 的鼠标事件会继续冒泡，空 handler 挡不住，
                    // 必须显式停止传播，否则点面板内部会连带触发遮罩的关闭。
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_gv: &mut GraphView, _ev: &MouseDownEvent, _w, cx| {
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
                                    .child(ui::title(theme, "关系图谱"))
                                    .child(ui::sub(
                                        theme,
                                        format!("{node_count} 篇笔记 · {edge_count} 条链接"),
                                    )),
                            )
                            .child(ui::icon_btn(
                                theme,
                                "graph-close",
                                "×",
                                cx.listener(
                                    |_gv: &mut GraphView, _ev: &MouseDownEvent, window, cx| {
                                        window.dispatch_action(Box::new(Close), cx);
                                    },
                                ),
                            )),
                    )
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .w(px(CANVAS_W))
                            .h(px(CANVAS_H))
                            .m(px(12.))
                            .bg(theme.surface)
                            .rounded(Metrics::RADIUS_BTN)
                            .border_1()
                            .border_color(theme.rule)
                            .overflow_hidden()
                            .when(node_count == 0, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(13.))
                                        .text_color(theme.muted)
                                        .child("笔记库里还没有笔记，或笔记之间还没有 [[双向链接]]"),
                                )
                            })
                            .child(edges_layer)
                            .children(nodes.into_iter().enumerate().map(|(i, node)| {
                                let nx = (node.x * CANVAS_W).clamp(4., CANVAS_W - LABEL_MAX_W - 20.);
                                let ny =
                                    (node.y * CANVAS_H).clamp(4., CANVAS_H - DOT - 14.);
                                let deg = node.degree;
                                // 连接越多点越大，孤立点用灰色区分
                                let dot = if deg >= 4 {
                                    11.
                                } else if deg >= 1 {
                                    8.
                                } else {
                                    6.
                                };
                                let dot_color = if deg >= 1 { theme.accent } else { theme.muted };
                                let path = node.path.clone();
                                div()
                                    .id(i)
                                    .absolute()
                                    .left(px(nx))
                                    .top(px(ny))
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .px_1()
                                    .rounded_md()
                                    .cursor(gpui::CursorStyle::PointingHand)
                                    .hover(|s| s.bg(theme.hover))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            move |_gv: &mut GraphView,
                                                  _ev: &MouseDownEvent,
                                                  window,
                                                  cx| {
                                                window.dispatch_action(
                                                    Box::new(OpenNoteAt(path.clone(), 0)),
                                                    cx,
                                                );
                                            },
                                        ),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .w(px(dot))
                                            .h(px(dot))
                                            .rounded_full()
                                            .bg(dot_color),
                                    )
                                    .child(
                                        div()
                                            .max_w(px(LABEL_MAX_W))
                                            .truncate()
                                            .text_size(px(11.))
                                            .text_color(theme.text)
                                            .child(node.name.clone()),
                                    )
                            })),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px(px(14.))
                            .py(px(9.))
                            .border_t_1()
                            .border_color(theme.rule)
                            .child(ui::sub(theme, "点击节点跳转 · Esc 关闭")),
                    ),
            )
    }
}
