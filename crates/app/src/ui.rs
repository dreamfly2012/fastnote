//! 共用视觉组件：按钮、浮层面板、标题行、分隔条、标签块。
//!
//! 之前每个界面各自写 `.border_1().border_color(theme.border).rounded_md()`，
//! 结果是「每个按钮都是一圈描边」，一屏十几个按钮看着很吵。这里统一成：
//!
//! - **按钮无描边**，靠 hover 底色 + 文字色区分层次；只有主操作与选中态用强调色。
//! - **浮层面板**统一「白底 + 1px 描边 + 大圆角 + 投影」，与压暗遮罩形成层次。
//! - 色觉友好：任何状态都不只靠色相区分 —— 主操作同时加粗，选中态同时加底色。

use gpui::{
    BoxShadow, Div, ElementId, FontWeight, IntoElement, InteractiveElement, MouseButton,
    MouseDownEvent, ParentElement, Pixels, SharedString, Styled, Window, div, point, px,
};

use crate::theme::{Metrics, Theme};
use crate::When;

/// 按钮的语气：决定前景色、字重与 hover 底色。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 普通动作
    Plain,
    /// 主操作（新建等）：强调色 + 加粗
    Primary,
    /// 次要动作（另存为、主题）：弱化文字
    Quiet,
    /// 当前生效的开关（侧栏 / 标签页）
    On,
    /// 破坏性动作（删除）
    Danger,
}

/// 无描边按钮：hover 才出现底色，避免一屏描边。
pub fn btn(
    theme: Theme,
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: Kind,
    on_down: impl Fn(&MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let (fg, weight, hover_bg) = match kind {
        Kind::Primary => (theme.accent, FontWeight::SEMIBOLD, theme.hover),
        Kind::Quiet => (theme.muted, FontWeight::NORMAL, theme.hover),
        Kind::On => (theme.accent, FontWeight::SEMIBOLD, theme.active),
        Kind::Danger => (theme.warn, FontWeight::NORMAL, theme.hover),
        Kind::Plain => (theme.text, FontWeight::NORMAL, theme.hover),
    };

    div()
        .id(id)
        .flex_none()
        .px(px(9.))
        .py(px(5.))
        .rounded(Metrics::RADIUS_BTN)
        .text_size(px(12.))
        .text_color(fg)
        .font_weight(weight)
        .cursor(gpui::CursorStyle::PointingHand)
        .when(kind == Kind::On, |d| d.bg(theme.active))
        .hover(move |s| s.bg(hover_bg))
        .child(label.into())
        .on_mouse_down(MouseButton::Left, on_down)
}

/// 方形的纯图标按钮（关闭 / 收起这类），hover 出底色。
pub fn icon_btn(
    theme: Theme,
    id: impl Into<ElementId>,
    glyph: impl Into<SharedString>,
    on_down: impl Fn(&MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(24.))
        .rounded(Metrics::RADIUS_BTN)
        .text_size(px(14.))
        .text_color(theme.muted)
        .cursor(gpui::CursorStyle::PointingHand)
        .hover(|s| s.bg(theme.hover).text_color(theme.text))
        .child(glyph.into())
        .on_mouse_down(MouseButton::Left, on_down)
}

/// 浮层遮罩：铺满窗口、压暗背景，面板统一从固定的顶部留白往下排
/// （切换不同浮层时面板顶部不跳动）。
pub fn scrim(theme: Theme) -> Div {
    div()
        .absolute()
        .inset_0()
        .flex()
        .justify_center()
        .items_start()
        .bg(theme.scrim)
}

/// 浮层面板外壳：白底 + 描边 + 大圆角 + 投影，内容纵向排列、裁掉溢出。
pub fn panel(theme: Theme, width: Pixels) -> Div {
    div()
        .flex_none()
        .flex_col()
        .w(width)
        .mt(Metrics::OVERLAY_TOP)
        .bg(theme.bg)
        .border_1()
        .border_color(theme.border)
        .rounded(Metrics::RADIUS_PANEL)
        .overflow_hidden()
        .shadow(vec![BoxShadow {
            color: theme.shadow,
            offset: point(px(0.), px(12.)),
            blur_radius: px(32.),
            spread_radius: px(-6.),
        }])
}

/// 浮层标题行：左侧标题（可带副标题），右侧留给关闭等动作。
pub fn header(theme: Theme) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .px(px(14.))
        .py(px(11.))
        .border_b_1()
        .border_color(theme.rule)
}

/// 标题文字
pub fn title(theme: Theme, text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_size(px(14.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.heading)
        .child(text.into())
}

/// 副标题 / 说明文字
pub fn sub(theme: Theme, text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .text_size(px(12.))
        .text_color(theme.muted)
        .child(text.into())
}

/// 竖排分组分隔条（顶栏用）
pub fn v_divider(theme: Theme) -> impl IntoElement {
    div().flex_none().w(px(1.)).h(px(16.)).bg(theme.rule)
}

/// 标签块：底色 + 前景，不单靠色相
pub fn chip(theme: Theme, text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .flex_none()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(5.))
        .bg(theme.tag_bg)
        .text_size(px(11.))
        .text_color(theme.tag)
        .child(text.into())
}

/// 键盘快捷键提示：等宽感的小方块，避免与正文混淆
pub fn kbd(theme: Theme, text: impl Into<SharedString>) -> impl IntoElement {
    div()
        .flex_none()
        .px(px(5.))
        .py(px(1.))
        .rounded(px(4.))
        .bg(theme.code_bg)
        .border_1()
        .border_color(theme.rule)
        .text_size(px(11.))
        .text_color(theme.muted)
        .child(text.into())
}
