//! 快捷键帮助面板。
//!
//! `Ctrl+?`（即 `Ctrl+Shift+/`）弹出，列出全部快捷键；Esc 或点击空白关闭。
//! gpui 0.2.2 无内置滚动容器，故采用两栏紧凑布局，保证一屏内显示完。

use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Render, Styled, Window, div, px, InteractiveElement,
};

use crate::command_palette::Close;
use crate::theme::Theme;
use crate::ui;

/// 分组：标题 + 若干 (说明, 按键)。
pub struct ShortcutGroup {
    pub title: &'static str,
    pub items: Vec<(&'static str, &'static str)>,
}

/// 全部快捷键，按主题分组，便于两栏展示。
pub fn shortcuts() -> Vec<ShortcutGroup> {
    vec![
        ShortcutGroup {
            title: "文件",
            items: vec![
                ("打开文件", "Ctrl+O"),
                ("打开笔记库", "Ctrl+Shift+O"),
                ("新建笔记", "Ctrl+N"),
                ("保存", "Ctrl+S"),
                ("另存为", "Ctrl+Shift+S"),
            ],
        },
        ShortcutGroup {
            title: "编辑 · 光标与选择",
            items: vec![
                ("移动光标", "↑ ↓ ← →"),
                ("按词移动", "Ctrl+← / →"),
                ("行首 / 行尾", "Home / End"),
                ("文档首 / 尾", "Ctrl+Home / End"),
                ("上 / 下翻页", "PageUp / PageDown"),
                ("选择文本", "Shift+方向键"),
                ("全选", "Ctrl+A"),
            ],
        },
        ShortcutGroup {
            title: "编辑 · 文本",
            items: vec![
                ("换行", "Enter"),
                ("缩进 / 反缩进", "Tab / Shift+Tab"),
                ("撤销 / 重做", "Ctrl+Z / Ctrl+Shift+Z"),
                ("复制 / 剪切 / 粘贴", "Ctrl+C / X / V"),
                ("切换待办勾选", "Ctrl+Enter"),
            ],
        },
        ShortcutGroup {
            title: "格式（Markdown）",
            items: vec![
                ("加粗", "Ctrl+B"),
                ("斜体", "Ctrl+I"),
                // 展示 Ctrl+Shift+` 而不是 Ctrl+`：Windows 上 Ctrl+` 的 WM_KEYDOWN 在
                // gpui 平台层就被吞掉（按键到不了 app，绑什么都无效）；而 Ctrl+Shift+`
                // 会以 key="~"、mods=ctrl 到达，代码里按字符绑了 `ctrl-~`，实测可用。
                // 无此平台问题的机器上两者皆可，展示 Shift 形更稳。
                ("行内代码", "Ctrl+Shift+`"),
            ],
        },
        ShortcutGroup {
            title: "视图",
            items: vec![
                ("浮现顶部工具条", "鼠标贴窗口顶端"),
                ("顶栏常显 / 收起", "Ctrl+Shift+M"),
                ("收起 / 展开侧栏", "Ctrl+\\"),
                ("切换深 / 浅色", "Ctrl+Shift+L"),
                ("聚焦编辑区", "Ctrl+Shift+E"),
                ("切换知识面板", "Ctrl+Shift+R"),
            ],
        },
        ShortcutGroup {
            title: "知识库",
            items: vec![
                ("全库搜索", "Ctrl+Shift+F"),
                ("关系图谱", "Ctrl+Shift+G"),
                ("每日笔记", "Ctrl+D"),
                ("库问答", "Ctrl+Shift+Q"),
                ("白板", "Ctrl+Shift+B"),
                ("历史版本", "Ctrl+Shift+H"),
            ],
        },
        ShortcutGroup {
            title: "AI 与命令",
            items: vec![
                // 同上：Ctrl+, 在 Windows 上被平台层吞掉，实际可用的是 Ctrl+Shift+,
                // （到达形状 key="<"、mods=ctrl，代码里绑 `ctrl-<`）
                ("打开 AI 设置", "Ctrl+Shift+,"),
                ("打开命令面板", "Ctrl+K"),
                ("采纳 AI 续写", "Alt+Tab"),
                ("忽略 AI 续写", "Esc"),
                ("打开本帮助", "Ctrl+?"),
            ],
        },
        ShortcutGroup {
            title: "模板",
            items: vec![
                ("模板目录", "库内 templates/"),
                ("从模板新建", "Ctrl+K → 模板名"),
                ("变量", "{{date}} {{title}} …"),
            ],
        },
    ]
}

pub struct HelpView {
    focus: FocusHandle,
    theme: Theme,
}

impl HelpView {
    pub fn new(theme: Theme, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            theme,
        }
    }
}

impl EventEmitter<Close> for HelpView {}

impl Focusable for HelpView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for HelpView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let groups = shortcuts();
        let mid = (groups.len() + 1) / 2;

        ui::scrim(theme)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_v: &mut HelpView, _ev: &MouseDownEvent, window, cx| {
                    window.dispatch_action(Box::new(Close), cx)
                }),
            )
            .key_context("Help")
            .track_focus(&self.focus)
            .child(
                ui::panel(theme, px(760.))
                    // 面板内部不冒泡到遮罩，否则点面板任意位置都会关掉它
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_v: &mut HelpView, _ev: &MouseDownEvent, _w, cx| {
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
                                    .child(ui::title(theme, "快捷键"))
                                    .child(ui::sub(theme, "Esc 或点击空白处关闭")),
                            )
                            .child(ui::icon_btn(
                                theme,
                                "help-close",
                                "×",
                                cx.listener(|_v: &mut HelpView, _ev: &MouseDownEvent, window, cx| {
                                    window.dispatch_action(Box::new(Close), cx)
                                }),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(28.))
                            .p(px(16.))
                            .text_color(theme.text)
                            .child(build_column(&groups[..mid], theme))
                            .child(build_column(&groups[mid..], theme)),
                    ),
            )
    }
}

/// 一栏：若干分组的纵向堆叠。
fn build_column(groups: &[ShortcutGroup], theme: Theme) -> impl IntoElement {
    div()
        .flex_col()
        .gap_3()
        .flex_1()
        .children(groups.iter().map(|g| build_group(g, theme)))
}

/// 单个分组：标题 + 键值对列表。
fn build_group(g: &ShortcutGroup, theme: Theme) -> impl IntoElement {
    div()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .pb(px(2.))
                .child(div().flex_none().size(px(5.)).rounded(px(2.)).bg(theme.accent))
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme.heading)
                        .child(g.title.to_string()),
                ),
        )
        .child(
            div()
                .flex_col()
                .gap(px(2.))
                .children(g.items.iter().map(|(desc, keys)| {
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .py(px(2.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(theme.text)
                                .child(desc.to_string()),
                        )
                        .child(ui::kbd(theme, keys.to_string()))
                })),
        )
}
