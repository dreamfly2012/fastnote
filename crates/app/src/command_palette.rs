//! 命令面板：自由指令 + 预设 AI 动作。
//!
//! 输入框是自建的单行文本视图 `CmdInput`，实现 `EntityInputHandler` 以支持
//! 中文 IME —— gpui 0.2.2 没有内置文本输入组件，也没有命令面板组件。
//!
//! 事件链：CmdInput 回车 -> emit `Submit` -> 命令面板构造 `Task` -> emit
//! `SubmitAi` -> Workspace 执行并关闭面板。

use std::ops::Range;
use std::path::PathBuf;

use fastnote_ai::Task;
use fastnote_core::{Document, Editor};
use gpui::{
    App, Bounds, Context, Element, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, MouseButton, ParentElement, Render, SharedString, Styled,
    Style, UTF16Selection, Window, actions, div, px, relative, uniform_list,
    InteractiveElement, Pixels, MouseDownEvent, AppContext,
};

use crate::theme::{Metrics, Theme};
use crate::ui;
use crate::{NewNoteFromTemplate, RunAppCmd, SubmitAi};
use crate::When;

actions!(command_palette, [Submit, PaletteUp, PaletteDown, Close]);
actions!(cmd_input, [Backspace, Delete, Left, Right, Home, End]);

/// 单个条目的高度。`uniform_list` 要求定高，它同时也是列表高度的计算单位。
const ITEM_H: f32 = 30.;
/// 列表高度上限，超出就靠滚动
const LIST_MAX_H: f32 = 372.;

// ---------------- 预设动作 ----------------

/// 命令面板条目：一个 AI 任务、一个应用命令，或一个具体模板。
#[derive(Clone)]
pub enum PaletteCmd {
    Ai(Task),
    App(AppCmd),
    /// 用指定模板新建笔记。模板是运行时数据（扫描 `templates/` 得到），
    /// 没法写进静态表，所以单独一个变体带上路径。
    NewFromTemplate(PathBuf),
}

/// 无需参数的界面命令。
///
/// 这些动作原先只能靠快捷键或顶栏按钮触发 —— 记不住快捷键的用户等于没有入口。
/// 收进命令面板后，它们有了统一的可检索入口（也是命令面板该有的样子）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppCmd {
    NewNote,
    NewFromTemplate,
    OpenFile,
    OpenVault,
    SaveAs,
    ToggleSidebar,
    ToggleTheme,
    ToggleRightPanel,
    FocusEditor,
    OpenSearch,
    OpenGraph,
    OpenDaily,
    OpenChat,
    OpenBoard,
    Snapshot,
    OpenHistory,
    OpenSettings,
    OpenHelp,
    OpenConfigDir,
}

/// 过滤：标签、分组、检索别名任一命中即可。空查询返回全部。
fn filter_indices(items: &[PaletteItem], q: &str) -> Vec<usize> {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return (0..items.len()).collect();
    }
    items
        .iter()
        .enumerate()
        .filter(|(_, it)| it.matches(&q))
        .map(|(i, _)| i)
        .collect()
}

pub struct PaletteItem {
    pub label: SharedString,
    /// 分组标题，渲染在标签左侧，给长列表一个视觉骨架
    pub group: &'static str,
    /// 检索别名（英文 / 拼音），中文输入法下打英文也能命中
    pub keywords: &'static str,
    pub cmd: PaletteCmd,
}

impl PaletteItem {
    fn matches(&self, q: &str) -> bool {
        self.label.to_lowercase().contains(q)
            || self.keywords.to_lowercase().contains(q)
            || self.group.to_lowercase().contains(q)
    }
}

pub fn default_items() -> Vec<PaletteItem> {    let ai = |label: &'static str, keywords: &'static str, task: Task| PaletteItem {
        label: label.into(),
        group: "AI",
        keywords,
        cmd: PaletteCmd::Ai(task),
    };
    let app = |label: &'static str, keywords: &'static str, cmd: AppCmd| PaletteItem {
        label: label.into(),
        group: "命令",
        keywords,
        cmd: PaletteCmd::App(cmd),
    };

    vec![
        // AI 任务：作用于当前选区或光标所在块
        ai("润色", "polish runse", Task::Polish),
        ai(
            "翻译为英文",
            "translate english en fanyi",
            Task::Translate {
                target: "英文".into(),
            },
        ),
        ai(
            "翻译为中文",
            "translate chinese zh fanyi",
            Task::Translate {
                target: "中文".into(),
            },
        ),
        ai("摘要", "summarize zhaiyao summary", Task::Summarize),
        ai("解释", "explain jieshi explain", Task::Explain),
        ai("续写", "complete xuxie continue", Task::Complete),
        // 文件
        app("新建笔记", "new note xinjian", AppCmd::NewNote),
        app(
            "从模板新建",
            "new from template muban",
            AppCmd::NewFromTemplate,
        ),
        app("打开文件", "open file dakai wenjian", AppCmd::OpenFile),
        app("打开笔记库", "open vault dakai bijiku", AppCmd::OpenVault),
        app("另存为", "save as lingcun as", AppCmd::SaveAs),
        // 知识库
        app("全库搜索", "search find sousuo", AppCmd::OpenSearch),
        app("关系图谱", "graph tupu relations", AppCmd::OpenGraph),
        app("今日笔记", "daily today jinri", AppCmd::OpenDaily),
        app("库问答", "chat ask qa wenku", AppCmd::OpenChat),
        app("打开白板", "board canvas baiban", AppCmd::OpenBoard),
        app("版本快照", "snapshot kuaizhao backup", AppCmd::Snapshot),
        app("历史版本", "history versions lishi", AppCmd::OpenHistory),
        // 视图
        app("收起 / 展开侧栏", "sidebar celan", AppCmd::ToggleSidebar),
        app("切换深 / 浅色", "theme dark light qianse", AppCmd::ToggleTheme),
        app("切换右侧面板", "panel right youce", AppCmd::ToggleRightPanel),
        app("聚焦编辑区", "focus editor jujiao", AppCmd::FocusEditor),
        // 应用
        app("AI 设置", "settings config shezhi", AppCmd::OpenSettings),
        app("快捷键帮助", "help keys bangzhu", AppCmd::OpenHelp),
        app(
            "打开配置目录",
            "config dir peizhimulu",
            AppCmd::OpenConfigDir,
        ),
    ]
}

// ---------------- CmdInput：单行 IME 输入框 ----------------

pub struct CmdInput {
    editor: Editor,
    focus: FocusHandle,
    theme: Theme,
    marked: Option<Range<usize>>,
}

impl CmdInput {
    pub fn new(theme: Theme, cx: &mut Context<Self>) -> Self {
        Self {
            editor: Editor::new(Document::from_str("")),
            focus: cx.focus_handle(),
            theme,
            marked: None,
        }
    }

    pub fn text(&self) -> String {
        let end = self.editor.doc.len_bytes();
        self.editor.doc.slice_to_string(0..end)
    }

    /// 用给定内容替换输入框（搜索面板预填查询时用）。
    pub fn set_text(&mut self, s: &str) {
        self.editor = Editor::new(Document::from_str(s));
        self.marked = None;
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(Submit);
    }
    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.backspace();
        cx.notify();
    }
    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.delete_forward();
        cx.notify();
    }
    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_left(false);
        cx.notify();
    }
    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_right(false);
        cx.notify();
    }
    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_line_start(false);
        cx.notify();
    }
    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_line_end(false);
        cx.notify();
    }

    fn utf16_to_byte(&self, u: usize) -> usize {
        let r = self.editor.doc.rope();
        let cu = u.min(r.len_utf16_cu());
        let c = r.utf16_cu_to_char(cu);
        r.char_to_byte(c)
    }
    fn byte_to_utf16(&self, b: usize) -> usize {
        let r = self.editor.doc.rope();
        let b = b.min(r.len_bytes());
        let c = r.byte_to_char(b);
        r.char_to_utf16_cu(c)
    }
    fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.utf16_to_byte(r.start)..self.utf16_to_byte(r.end)
    }
    fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.byte_to_utf16(r.start)..self.byte_to_utf16(r.end)
    }
}

impl EventEmitter<Submit> for CmdInput {}
impl EventEmitter<Close> for CmdInput {}

impl Focusable for CmdInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EntityInputHandler for CmdInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let r = self.range_from_utf16(&range_utf16);
        actual.replace(self.range_to_utf16(&r));
        Some(self.editor.doc.slice_to_string(r))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let sel = self.editor.selection();
        let reversed = self.editor.cursor() == sel.start && self.editor.has_selection();
        Some(UTF16Selection {
            range: self.range_to_utf16(&sel),
            reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or_else(|| self.marked.clone());
        if let Some(r) = target {
            self.editor.select(r);
        }
        self.marked = None;
        if self.editor.has_selection() {
            self.editor.replace_selection(new_text);
        } else {
            self.editor.insert(new_text);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or_else(|| self.marked.clone());
        if let Some(r) = target {
            self.editor.select(r);
        }
        let start = self.editor.selection().start;
        if self.editor.has_selection() {
            self.editor.replace_selection(new_text);
        } else {
            self.editor.insert(new_text);
        }
        self.marked = if new_text.is_empty() {
            None
        } else {
            Some(start..start + new_text.len())
        };
        if let Some(sel) = new_selected_range_utf16 {
            let s = start + sel.start.min(new_text.len());
            let e = start + sel.end.min(new_text.len());
            self.editor.select(s..e);
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _p: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for CmdInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let text = self.text();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .w_full()
            .h(px(36.))
            .px_3()
            .border_1()
            .border_color(theme.accent)
            .rounded_md()
            .bg(theme.bg)
            .text_color(theme.text)
            .text_size(px(14.))
            .key_context("CmdInput")
            .track_focus(&self.focus)
            .cursor(gpui::CursorStyle::IBeam)
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .when(text.is_empty(), |d| {
                d.child(div().text_color(theme.muted).child("输入指令，或选择下方动作（回车执行）"))
            })
            .when(!text.is_empty(), |d| d.child(div().child(text)))
            .child(div().w(px(2.)).h(px(18.)).bg(theme.accent))
            // 透明占位元素：唯一职责是在 paint 时注册输入处理器，
            // 让中文 IME 能把文字送进 CmdInput。本身不绘制任何内容。
            .child(InputRegistrar {
                view: cx.entity(),
            })
    }
}

/// 仅用于把 `CmdInput` 注册为 IME 输入目标。gpui 0.2.2 没有内置
/// 文本输入组件，必须靠自定义元素在 paint 阶段调 `window.handle_input`。
struct InputRegistrar {
    view: Entity<CmdInput>,
}

impl IntoElement for InputRegistrar {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InputRegistrar {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        // 占满父容器，IME 候选窗位置才自然
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) -> () {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.view.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.view.clone()), cx);
    }
}

// ---------------- CommandPalette ----------------

pub struct CommandPalette {
    pub input: Entity<CmdInput>,
    items: Vec<PaletteItem>,
    /// 在过滤后列表中的下标
    selected: usize,
    focus: FocusHandle,
    theme: Theme,
}

impl CommandPalette {
    /// `templates` 是 `(显示名, 模板路径)`：库里 `templates/` 下的模板会
    /// 作为「模板」分组的条目出现在列表里，选中即按模板新建。
    pub fn new(theme: Theme, templates: Vec<(String, PathBuf)>, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| CmdInput::new(theme, cx));
        let mut items = default_items();
        items.extend(
            templates
                .into_iter()
                .map(|(name, path)| PaletteItem {
                    label: name.into(),
                    group: "模板",
                    keywords: "template muban new",
                    cmd: PaletteCmd::NewFromTemplate(path),
                }),
        );
        Self {
            input,
            items,
            selected: 0,
            focus: cx.focus_handle(),
            theme,
        }
    }

    fn filtered(&self, q: &str) -> Vec<usize> {
        filter_indices(&self.items, q)
    }

    fn on_submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        let q = self.input.read(cx).text();
        let f = self.filtered(&q);
        match f.get(self.selected.min(f.len().saturating_sub(1))) {
            Some(&i) => match self.items[i].cmd.clone() {
                PaletteCmd::Ai(task) => cx.emit(SubmitAi(task)),
                PaletteCmd::App(cmd) => cx.emit(RunAppCmd(cmd)),
                PaletteCmd::NewFromTemplate(path) => cx.emit(NewNoteFromTemplate(path)),
            },
            // 没命中任何条目时，把输入整体当作自由指令交给 AI
            None if !q.trim().is_empty() => cx.emit(SubmitAi(Task::Custom {
                instruction: q.trim().to_string(),
            })),
            None => return,
        }
    }

    fn up(&mut self, _: &PaletteUp, _: &mut Window, cx: &mut Context<Self>) {
        let q = self.input.read(cx).text();
        let f = self.filtered(&q);
        if f.is_empty() {
            return;
        }
        self.selected = self.selected.saturating_sub(1) % f.len();
        cx.notify();
    }

    fn down(&mut self, _: &PaletteDown, _: &mut Window, cx: &mut Context<Self>) {
        let q = self.input.read(cx).text();
        let f = self.filtered(&q);
        if f.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % f.len();
        cx.notify();
    }
}

impl EventEmitter<SubmitAi> for CommandPalette {}
impl EventEmitter<RunAppCmd> for CommandPalette {}
impl EventEmitter<NewNoteFromTemplate> for CommandPalette {}
impl EventEmitter<Close> for CommandPalette {}

impl Focusable for CommandPalette {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let q = self.input.read(cx).text();
        let f = std::rc::Rc::new(self.filtered(&q));
        let count = f.len();
        let selected = self.selected.min(count.saturating_sub(1));
        let input = self.input.clone();
        let this = cx.entity();

        // 条目多了以后（命令 + 模板）必须能滚动，否则后面的条目看不见也选不到。
        // 高度跟着条数走，少的时候面板短、多的时候到上限再滚。
        let list = uniform_list("palette-items", count, {
            let f = f.clone();
            let this = this.clone();
            move |range, window, cx| {
                let pal = this.read(cx);
                range
                    .filter_map(|i| {
                        let idx = *f.get(i)?;
                        let it = pal.items.get(idx)?;
                        let sel = i == selected;
                        let label = it.label.clone();
                        let group = it.group;
                        Some(
                            div()
                                .id(i)
                                .flex()
                                .items_center()
                                .gap_2()
                                .h(px(ITEM_H))
                                .mx(px(6.))
                                .px(px(8.))
                                .rounded(Metrics::RADIUS_BTN)
                                .text_size(px(13.))
                                .text_color(if sel { theme.heading } else { theme.text })
                                .when(sel, |d| {
                                    d.bg(theme.active).font_weight(gpui::FontWeight::SEMIBOLD)
                                })
                                .when(!sel, |d| d.hover(|s| s.bg(theme.hover)))
                                .cursor(gpui::CursorStyle::PointingHand)
                                // 分组标签用固定宽度，让长列表里的标签列对齐
                                .child(
                                    div()
                                        .flex_none()
                                        .w(px(34.))
                                        .text_size(px(10.))
                                        .text_color(if sel { theme.accent } else { theme.muted })
                                        .child(group),
                                )
                                .child(label)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    window.listener_for(
                                        &this,
                                        move |pal: &mut CommandPalette,
                                              _ev: &MouseDownEvent,
                                              window,
                                              cx| {
                                            pal.selected = i;
                                            pal.on_submit(&Submit, window, cx);
                                        },
                                    ),
                                ),
                        )
                    })
                    .collect()
            }
        })
        .h(px((count as f32 * ITEM_H).min(LIST_MAX_H)))
        .w_full();

        ui::scrim(theme)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_pal: &mut CommandPalette, _ev: &MouseDownEvent, window, cx| {
                    window.dispatch_action(Box::new(Close), cx)
                }),
            )
            .key_context("CommandPalette")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_submit))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .child(
                ui::panel(theme, px(560.))
                    .max_h(px(440.))
                    // 面板内部不冒泡到遮罩：否则点输入框、点条目之间的空白都会把面板关掉
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_pal: &mut CommandPalette, _ev: &MouseDownEvent, _w, cx| {
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
                            .child(list),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_items_have_unique_labels_across_groups() {
        let items = default_items();
        // 上下键是按可见顺序选的，重名会让"选中的到底是哪个"变得不确定
        let mut labels: Vec<String> = items.iter().map(|i| i.label.to_string()).collect();
        let total = labels.len();
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), total, "命令面板里有重名条目");

        assert!(items.iter().any(|i| i.group == "AI"), "AI 分组不能为空");
        assert!(items.iter().any(|i| i.group == "命令"), "命令分组不能为空");
    }

    #[test]
    fn search_hits_label_group_and_keywords() {
        let items = default_items();

        // 中文标签
        let by_label = filter_indices(&items, "图谱");
        assert_eq!(items[by_label[0]].label, "关系图谱");

        // 英文别名：中文输入法下打 graph 也能找到
        let by_kw = filter_indices(&items, "graph");
        assert!(!by_kw.is_empty());
        assert_eq!(items[by_kw[0]].label, "关系图谱");

        // 拼音别名的前缀
        assert!(!filter_indices(&items, "muban").is_empty());

        // 分组名
        assert!(filter_indices(&items, "命令").len() >= 10);

        // 空白查询返回全部；无命中返回空
        assert_eq!(filter_indices(&items, "   ").len(), items.len());
        assert!(filter_indices(&items, "zzz不存在的命令").is_empty());
    }

    #[test]
    fn template_entries_land_in_their_own_group() {
        // 模板是运行时数据，构造条目时分组固定为「模板」
        let mut items = default_items();
        items.push(PaletteItem {
            label: "周会".into(),
            group: "模板",
            keywords: "template muban new",
            cmd: PaletteCmd::NewFromTemplate(PathBuf::from("templates/周会.md")),
        });
        let hit = filter_indices(&items, "周会");
        assert_eq!(hit.len(), 1);
        assert_eq!(items[hit[0]].group, "模板");
        // 也可以靠共用别名检索出来
        assert!(!filter_indices(&items, "template").is_empty());
    }
}
