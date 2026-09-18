//! 白板浮层：在笔记里的 ` ```board ` 代码块上做自由布局的节点与连线。
//!
//! 交互上刻意避开了"把鼠标窗口坐标换算成画布坐标"：
//!
//! - **拖动**只用到鼠标位移（按下点 → 当前点），是绝对增量，不需要知道画布在窗口里的位置，
//!   也不会因为窗口大小变化而失准；
//! - **命中**交给 gpui 的元素层次（节点自己的 `on_mouse_down`），不在数据层做坐标判定。
//!
//! 这样画布在窗口里怎么居中、多宽多高，都不影响交互正确性。
//!
//! 连线用 `canvas` 画（div 画不出斜线），节点用绝对定位 div（文字 / hover / 点击好做），
//! 这一层与关系图谱是同一套路子。
//!
//! 落盘时机：**每次改动结束后**通过 `BoardSync` 交给 Workspace 写回笔记
//! （拖拽只在松手时写一次）。这样无论用户是点 × 还是按 Esc 离开，改动都不会丢 ——
//! 也就不需要在关闭路径上做任何保存逻辑。

use std::path::PathBuf;

use fastnote_core::Board;
use fastnote_core::board;
use gpui::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, PathBuilder, Pixels,
    Render, Styled, Window, actions, canvas, div, point, px,
};

use crate::When;
use crate::command_palette::{CmdInput, Close, Submit};
use crate::theme::{Metrics, Theme};
use crate::ui;
use crate::{BoardSync, OpenNoteAt};

actions!(board_view, []);

/// 画布尺寸：渲染的唯一基准（数据层存的是归一化坐标，与这里解耦）。
const CANVAS_W: f32 = 880.;
const CANVAS_H: f32 = 520.;
/// 节点卡片尺寸
const NODE_W: f32 = 132.;
const NODE_H: f32 = 30.;

/// 节点的渲染落点：中心坐标换算成左上角，并夹在画布内。
fn node_left(x: f32) -> f32 {
    (x * CANVAS_W - NODE_W / 2.).clamp(2., CANVAS_W - NODE_W - 2.)
}
fn node_top(y: f32) -> f32 {
    (y * CANVAS_H - NODE_H / 2.).clamp(2., CANVAS_H - NODE_H - 2.)
}

/// `Pixels` 是 `pub(crate)` 的 f32 且没有实现 `Sub`，
/// 但实现了 `Div<Pixels> -> f32`，除以 `px(1.)` 就能取回原始数值。
fn f(v: Pixels) -> f32 {
    v / px(1.)
}
fn abs(base: Pixels, local: f32) -> Pixels {
    px(f(base) + local)
}

/// 新节点的落点：按已有数量排成 5 列的网格，避免新节点全叠在同一个位置。
///
/// 行列一律用**整数**运算 —— 写成 `n % 5.0` 会得到 0.2 这种小数，
/// 位置就会一点点漂走（`(1.0 / 5.0) % 4.0 == 0.2`，不是 0）。
fn slot_for(n: usize) -> (f32, f32) {
    let col = (n % 5) as f32;
    let row = ((n / 5) % 4) as f32;
    (0.3 + col * 0.1, 0.25 + row * 0.16)
}

/// 拖动中的一个节点。
struct Drag {
    id: u32,
    /// 按下时的鼠标窗口坐标
    start: (f32, f32),
    /// 按下时节点的归一化坐标
    from: (f32, f32),
}

pub struct BoardView {
    board: Board,
    /// 写回目标：包含白板代码块的那篇笔记
    pub note: PathBuf,
    theme: Theme,
    focus: FocusHandle,
    drag: Option<Drag>,
    selected: Option<u32>,
    /// 连线起点：按「连线」按钮记这里，再点目标节点连上
    link_from: Option<u32>,
    /// 内存状态与磁盘不一致（松手 / 增删改后由 `touched` 清掉）
    dirty: bool,
    pub input: Entity<CmdInput>,
}

impl BoardView {
    /// 从笔记正文里读出白板；没有代码块就起一块空白的。
    pub fn open(theme: Theme, note: PathBuf, cx: &mut Context<Self>) -> Self {
        let board = std::fs::read_to_string(&note)
            .ok()
            .and_then(|s| board::extract(&s))
            .unwrap_or_default();
        Self::new(theme, note, board, cx)
    }

    pub fn new(theme: Theme, note: PathBuf, board: Board, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| CmdInput::new(theme, cx));
        Self {
            board,
            note,
            theme,
            focus: cx.focus_handle(),
            drag: None,
            selected: None,
            link_from: None,
            dirty: false,
            input,
        }
    }

    /// 把当前白板交给 Workspace 写回笔记。
    ///
    /// 由 Workspace 执行而不是这里直接写文件：只有它知道编辑器里有没有更新过的正文，
    /// 而写回要基于**最新的正文**替换代码块（否则会把编辑器里的改动抹掉）。
    fn touched(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dirty = false;
        window.dispatch_action(
            Box::new(BoardSync(self.note.clone(), self.board.to_json())),
            cx,
        );
        cx.notify();
    }

    /// 新建节点：按已有数量错开摆放，避免新节点全叠在同一个位置。
    fn add_node(&mut self, text: impl Into<String>) {
        let (x, y) = slot_for(self.board.nodes.len());
        let id = self.board.add_node(x, y, text);
        self.selected = Some(id);
        self.link_from = None;
        self.dirty = true;
    }

    // ---------- 画布交互 ----------

    fn on_node_down(
        &mut self,
        id: u32,
        ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some(id);
        // 终止气泡：空白层与节点是兄弟、且都被 hit_test 命中，
        // 不拦住的话紧接着 on_canvas_down 会把刚设好的拖动状态清掉。
        cx.stop_propagation();
        let handle = self.focus.clone();
        window.focus(&handle);

        // 已有连线起点：这次点击把它和目标连上
        if let Some(from) = self.link_from.filter(|from| *from != id) {
            self.link_from = None;
            if self.board.connect(from, id) {
                self.touched(window, cx);
            } else {
                cx.notify();
            }
            return;
        }

        // 双击带链接目标的节点 → 打开对应笔记
        if ev.click_count >= 2 {
            if let Some(name) = self.board.node(id).and_then(|n| n.note.clone()) {
                let target = self.resolve(&name);
                window.dispatch_action(Box::new(OpenNoteAt(target, 0)), cx);
                return;
            }
        }

        // 进入拖动：只记录起点，位移在 move 里按绝对增量算
        let from = self.board.node(id).map(|n| (n.x, n.y)).unwrap_or((0., 0.));
        self.drag = Some(Drag {
            id,
            start: (f(ev.position.x), f(ev.position.y)),
            from,
        });
        cx.notify();
    }

    /// 节点上写的笔记名 -> 实际路径。支持 `目录/名字` 与裸名字。
    fn resolve(&self, name: &str) -> PathBuf {
        let base = self.note.parent().unwrap_or(std::path::Path::new("."));
        let mut p = base.join(name);
        if p.extension().is_none() {
            p.set_extension("md");
        }
        p
    }

    fn on_canvas_move(&mut self, ev: &MouseMoveEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.drag.as_ref() else {
            return;
        };
        // 绝对增量而不是逐帧累加：鼠标拖到极限再拖回来时，位置能立刻跟手
        let dx = f(ev.position.x) - d.start.0;
        let dy = f(ev.position.y) - d.start.1;
        let (id, from) = (d.id, d.from);
        let x = (from.0 + dx / CANVAS_W).clamp(0.06, 0.94);
        let y = (from.1 + dy / CANVAS_H).clamp(0.05, 0.95);
        self.board.move_node(id, x, y);
        self.dirty = true;
        cx.notify();
    }

    fn on_canvas_up(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        // 拖动过程中不写盘，松手才落一次
        if self.drag.take().is_some() && self.dirty {
            self.touched(window, cx);
        }
    }

    /// 空白处按下：清掉选中与连线起点。
    fn on_canvas_down(&mut self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.drag = None;
        let _ = window;
        if self.selected.take().is_some() || self.link_from.take().is_some() {
            cx.notify();
        }
    }

    // ---------- 命令 ----------

    /// 输入框回车：有选中节点就改名，否则新建一个节点。
    fn on_submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_input(window, cx);
    }

    fn apply_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().trim().to_string();
        if text.is_empty() {
            return;
        }
        match self.selected {
            Some(id) if self.board.node(id).is_some() => {
                self.board.set_text(id, text);
                self.dirty = true;
            }
            _ => self.add_node(text),
        }
        self.input.update(cx, |i, cx| {
            i.set_text("");
            cx.notify();
        });
        self.touched(window, cx);
    }

    /// 删除选中节点（挂在它身上的连线会一起清掉）。
    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.take() else {
            return;
        };
        self.board.remove_node(id);
        self.link_from = None;
        self.dirty = true;
        self.touched(window, cx);
    }

    /// 把选中节点设为连线起点。
    fn start_link(&mut self, cx: &mut Context<Self>) {
        self.link_from = self.selected;
        cx.notify();
    }

    /// 顶栏 / 底部共用的小按钮：语气交给 `ui::Kind` 决定
    /// （主操作加粗用强调色、破坏性操作用警示色，都不靠描边区分）。
    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        kind: ui::Kind,
        cx: &mut Context<Self>,
        f: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        let theme = self.theme;
        // 擦成 AnyElement：`cx.listener` 返回的闭包带着 `Context` 的匿名生命周期，
        // 直接返回 `impl IntoElement` 会让隐藏类型捕获它，签名对不上（E0700）。
        ui::btn(
            theme,
            id,
            label,
            kind,
            cx.listener(move |v: &mut BoardView, _ev: &MouseDownEvent, window, cx| {
                f(v, window, cx)
            }),
        )
        .into_any_element()
    }
}

impl Focusable for BoardView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for BoardView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let node_count = self.board.nodes.len();
        let edge_count = self.board.edges.len();
        let selected = self.selected;
        let link_from = self.link_from;

        // 连线端点取渲染后的中心，保证线与卡片始终对齐
        let segs: Vec<(f32, f32, f32, f32)> = self
            .board
            .edges
            .iter()
            .filter_map(|e| {
                let a = self.board.node(e.from)?;
                let b = self.board.node(e.to)?;
                Some((
                    node_left(a.x) + NODE_W / 2.,
                    node_top(a.y) + NODE_H / 2.,
                    node_left(b.x) + NODE_W / 2.,
                    node_top(b.y) + NODE_H / 2.,
                ))
            })
            .collect();
        let line_color = theme.border;

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

        let nodes: Vec<_> = self.board.nodes.clone();
        let file_name = self
            .note
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let input = self.input.clone();

        ui::scrim(theme)
            .key_context("BoardView")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_submit))
            // 拖动与松手挂在最外层遮罩上：gpui 的鼠标移动/松开只发给鼠标下的元素，
            // 挂在画布上就会在鼠标滑到卡片上方时收不到事件。遮罩铺满窗口，
            // 鼠标一定在它里面。
            .on_mouse_move(cx.listener(Self::on_canvas_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_canvas_up))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_v: &mut BoardView, _ev: &MouseDownEvent, window, cx| {
                    window.dispatch_action(Box::new(Close), cx);
                }),
            )
            .child(
                ui::panel(theme, px(CANVAS_W + 24.))
                    // 面板内部按下不往遮罩冒泡：否则点「+ 节点」这类按钮时，
                    // 事件会一路冒到遮罩的关闭 handler，浮层跟着被关掉。
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_v: &mut BoardView, _ev: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation()
                        }),
                    )
                    // ---- 标题栏 ----
                    .child(
                        ui::header(theme)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(ui::title(theme, format!("白板 · {file_name}")))
                                    .child(ui::sub(
                                        theme,
                                        format!("{node_count} 节点 · {edge_count} 连线"),
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(self.button("board-add", "+ 节点", ui::Kind::Plain, cx, |v, w, cx| {
                                        v.add_node(format!("新节点 {}", v.board.nodes.len() + 1));
                                        v.touched(w, cx);
                                    }))
                                    .child(self.button("board-link", "连线", ui::Kind::Plain, cx, |v, _w, cx| {
                                        v.start_link(cx)
                                    }))
                                    .child(self.button("board-del", "删除", ui::Kind::Danger, cx, |v, w, cx| {
                                        v.delete_selected(w, cx)
                                    }))
                                    .child(ui::icon_btn(
                                        theme,
                                        "board-close",
                                        "×",
                                        cx.listener(
                                            |_v: &mut BoardView,
                                             _ev: &MouseDownEvent,
                                             window,
                                             cx| {
                                                window.dispatch_action(Box::new(Close), cx);
                                            },
                                        ),
                                    )),
                            ),
                    )
                    // ---- 画布 ----
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
                                        .flex_col()
                                        .items_center()
                                        .justify_center()
                                        .gap_1()
                                        .text_size(px(12.))
                                        .text_color(theme.muted)
                                        .child("白板是空的")
                                        .child("在下面输入文字回车建第一个节点；拖动可移动；选中后点「连线」再点目标节点"),
                                )
                            })
                            .child(edges_layer)
                            // 空白层：夹在连线与节点之间。点它就是点空白 ——
                            // 节点是它的**兄弟**而不是子元素，所以点节点不会冒泡到这里。
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(Self::on_canvas_down),
                                    ),
                            )
                            .children(nodes.into_iter().map(|n| {
                                let is_sel = selected == Some(n.id);
                                let is_link = link_from == Some(n.id);
                                let id = n.id;
                                let arrow = if n.note.is_some() { " ↗" } else { "" };
                                div()
                                    .id(("board-node", id))
                                    .absolute()
                                    .left(px(node_left(n.x)))
                                    .top(px(node_top(n.y)))
                                    .w(px(NODE_W))
                                    .h(px(NODE_H))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(Metrics::RADIUS_BTN)
                                    .bg(if is_sel { theme.active } else { theme.bg })
                                    .border_1()
                                    .border_color(if is_sel || is_link {
                                        theme.accent
                                    } else {
                                        theme.rule
                                    })
                                    .text_size(px(11.))
                                    .text_color(theme.text)
                                    .overflow_hidden()
                                    .cursor(gpui::CursorStyle::PointingHand)
                                    .hover(|s| s.bg(theme.hover))
                                    .child(
                                        div()
                                            .px_1()
                                            .truncate()
                                            .child(format!("{}{arrow}", n.text)),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            move |v: &mut BoardView,
                                                  ev: &MouseDownEvent,
                                                  window,
                                                  cx| {
                                                v.on_node_down(id, ev, window, cx)
                                            },
                                        ),
                                    )
                            })),
                    )
                    // ---- 输入条 ----
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .px(px(14.))
                            .py(px(10.))
                            .border_t_1()
                            .border_color(theme.rule)
                            .child(div().flex_1().child(input))
                            .child(self.button("board-apply", "应用", ui::Kind::Primary, cx, |v, w, cx| {
                                v.apply_input(w, cx)
                            })),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px(px(14.))
                            .pb(px(12.))
                            .child(ui::sub(
                                theme,
                                "选中节点后输入文字回车改名；未选中时回车新建节点。选中后点「连线」再点另一个节点即可连上，Esc 关闭。",
                            )),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_stay_inside_the_canvas_at_the_edges() {
        // 左上角：卡片要完整可见，不能被画布裁掉
        for (x, y) in [(0.0, 0.0), (-5.0, -5.0)] {
            let l = node_left(x);
            let t = node_top(y);
            assert!(l >= 0.0 && l + NODE_W <= CANVAS_W, "x={x} 越界：left={l}");
            assert!(t >= 0.0 && t + NODE_H <= CANVAS_H, "y={y} 越界：top={t}");
        }
        // 右下角同理
        for (x, y) in [(1.0, 1.0), (9.0, 9.0)] {
            let l = node_left(x);
            let t = node_top(y);
            assert!(l + NODE_W <= CANVAS_W, "x={x} 越界：left={l}");
            assert!(t + NODE_H <= CANVAS_H, "y={y} 越界：top={t}");
        }
    }

    #[test]
    fn canvas_center_maps_to_a_centered_card() {
        // 归一化坐标 (0.5, 0.5) 必须落在画布正中 —— 布局与坐标换算的对应关系
        let cx = node_left(0.5) + NODE_W / 2.;
        let cy = node_top(0.5) + NODE_H / 2.;
        assert!((cx - CANVAS_W / 2.).abs() < 0.01, "水平中心偏移 {cx}");
        assert!((cy - CANVAS_H / 2.).abs() < 0.01, "垂直中心偏移 {cy}");
    }

    #[test]
    fn pixels_round_trip_through_the_div_helper() {
        // `Pixels` 没有 Sub，拖动全靠 `f()` 取回 f32；这条换算必须无损
        let v = px(123.5);
        assert!((f(v) - 123.5).abs() < f32::EPSILON);
        assert_eq!(f(abs(px(10.), 5.)), 15.);
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    /// f32 下 `0.3 + 4 * 0.1` 是 0.70000005，位置断言只能比近似值。
    fn assert_near(got: (f32, f32), want: (f32, f32), what: &str) {
        assert!(
            (got.0 - want.0).abs() < 1e-6 && (got.1 - want.1).abs() < 1e-6,
            "{what}: 落点 {got:?} 与期望 {want:?} 不符"
        );
    }

    #[test]
    fn new_nodes_are_laid_out_on_a_grid() {
        assert_near(slot_for(0), (0.3, 0.25), "第一个节点");
        assert_near(slot_for(1), (0.4, 0.25), "同一行往右排");
        assert_near(slot_for(4), (0.7, 0.25), "第一行最后一格");
        assert_near(slot_for(5), (0.3, 0.41), "第 6 个换行");
        assert_near(slot_for(20), (0.3, 0.25), "20 个之后回到网格起点");
    }

    #[test]
    fn every_slot_stays_inside_the_canvas() {
        for n in 0..200 {
            let (x, y) = slot_for(n);
            assert!((0.0..=1.0).contains(&x), "n={n} 的 x={x} 越界");
            assert!((0.0..=1.0).contains(&y), "n={n} 的 y={y} 越界");
            // 落点还得留出卡片本身的半个尺寸，否则会被 clamp 吸到边上
            assert!(node_left(x) + NODE_W <= CANVAS_W);
            assert!(node_top(y) + NODE_H <= CANVAS_H);
        }
    }
}
