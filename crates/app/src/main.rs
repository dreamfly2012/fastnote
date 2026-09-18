//! fastnote 应用入口。
//!
//! 窗口结构：左侧笔记库（虚拟滚动）+ 右侧编辑区 + 底部状态栏。
//!
//! 为什么侧栏用 `uniform_list` 而不是普通 `div` 堆叠：笔记库可能上万文件，
//! 只渲染可见项才能让侧栏的开销与文件数无关。

// Windows release 构建不弹控制台窗口；debug 保留，方便看 panic。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod board_view;
mod chat_view;
mod command_palette;
mod editor_view;
mod graph_view;
mod help_view;
mod history_view;
mod render;
mod search_view;
mod settings_view;
mod theme;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use board_view::BoardView;
use chat_view::ChatView;
use command_palette::{AppCmd, Close, CommandPalette, PaletteDown, PaletteUp, Submit};
use editor_view::{
    AcceptGhost, Backspace, Copy, Cut, DeleteForward, DismissGhost, DocEnd, DocStart, EditorView,
    Indent, LineEnd, LineStart, MoveDown, MoveLeft, MoveRight, MoveUp, Newline, Outdent, PageDown,
    PageUp, Paste, Redo, SelectAll, SelectDown, SelectLeft, SelectLineEnd, SelectLineStart,
    SelectRight, SelectUp, SelectWordLeft, SelectWordRight, ToggleBold, ToggleCode, ToggleItalic,
    ToggleTask, Undo, WordLeft, WordRight,
};
use fastnote_ai::{AiConfig, Task};
use fastnote_core::date;
use fastnote_core::index::{LinkRef, VaultIndex};
use fastnote_core::{Document, Editor, Vault, board, history, template};
use graph_view::GraphView;
use help_view::HelpView;
use history_view::HistoryView;
use search_view::SearchView;
use settings_view::SettingsView;
use gpui::{
    App, AppContext, Application, Bounds, BoxShadow, Context, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement, Pixels,
    Render, SharedString, Styled, TitlebarOptions, Window, WindowBounds,
    WindowOptions, actions, div, point, px, size, uniform_list,
};
use theme::{Metrics, Theme};

/// gpui 0.2.2 的 `Div` / `Stateful` 没有 `when` / `when_some` 条件包装方法，这里补一个通用扩展。
pub trait When: Sized {
    fn when<F>(self, condition: bool, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        if condition {
            f(self)
        } else {
            self
        }
    }

    fn when_some<T, F>(self, value: Option<T>, f: F) -> Self
    where
        F: FnOnce(Self, T) -> Self,
    {
        match value {
            Some(v) => f(self, v),
            None => self,
        }
    }
}

impl<T> When for T {}

actions!(
    fastnote,
    [
        OpenFile,
        Save,
        SaveAs,
        NewNote,
        OpenVault,
        ToggleSidebar,
        ToggleTheme,
        FocusEditor,
        OpenSettings,
        OpenCommandPalette,
        OpenConfigDir,
        OpenHelp,
        OpenSearch,
        ToggleRightPanel,
        ToggleChrome,
        OpenGraph,
        OpenDaily,
        OpenChat,
        OpenBoard,
        OpenHistory,
    ]
);

/// 带负载的动作无法用 `actions!` 生成（gpui 0.2.2 的宏只接受裸标识符），
/// 改用 `Action` derive。`no_json` 表示不通过 JSON keymap 调用，省去 serde 约束。
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = fastnote, no_json)]
pub struct SubmitAi(pub Task);

/// 命令面板选中的界面命令。命令面板不认识 Workspace，只负责"报出名字"。
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = fastnote, no_json)]
pub struct RunAppCmd(pub AppCmd);

/// 命令面板里选中了某个模板 -> 按它新建笔记。
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = fastnote, no_json)]
pub struct NewNoteFromTemplate(pub PathBuf);

/// 白板改动要落盘：`(目标笔记, 白板 JSON)`。
///
/// 由白板浮层发出、Workspace 执行——因为写回必须基于**最新正文**替换代码块，
/// 而正文可能刚在编辑器里改过，浮层看不到那一份。
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = fastnote, no_json)]
pub struct BoardSync(pub PathBuf, pub String);

/// 恢复某个历史版本：`(笔记, 快照)`。
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = fastnote, no_json)]
pub struct RestoreSnapshot(pub PathBuf, pub PathBuf);

#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = fastnote, no_json)]
pub struct ApplyPreset(pub usize);

/// 点击渲染态块里的双向链接时由编辑器发出，Workspace 据此打开目标笔记。
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = fastnote, no_json)]
pub struct OpenWikilink(pub String);

/// 搜索结果被点击时由搜索面板发出，Workspace 据此打开对应笔记并跳转到行。
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = fastnote, no_json)]
pub struct OpenNoteAt(pub PathBuf, pub usize);

/// 根视图：持有笔记库与编辑器，负责布局与全局命令。
struct Workspace {
    editor: Entity<EditorView>,
    vault: Option<Vault>,
    theme: Theme,
    dark: bool,
    show_sidebar: bool,
    /// 顶部工具条是否被「钉住」。未钉住时只有鼠标移到窗口顶端才浮现，
    /// 默认不占版面 —— 打开就是干净的编辑区。
    chrome_pinned: bool,
    /// 鼠标是否停在顶部热区/工具条上（钉住与否之外的另一半条件）
    chrome_hovered: bool,
    /// 选中的侧栏条目下标
    selected: Option<usize>,
    /// 一次性提示（保存成功 / 出错），显示在状态栏
    notice: Option<SharedString>,
    focus: FocusHandle,
    /// AI 端点配置（克隆自文件 + 环境变量）
    ai_cfg: AiConfig,
    /// 右键弹出的 AI 动作菜单（屏幕坐标 + 是否有选中文本）
    context_menu: Option<AiMenu>,
    /// 命令面板
    command_palette: Option<Entity<CommandPalette>>,
    /// 设置面板
    settings: Option<Entity<SettingsView>>,
    /// 快捷键帮助面板
    help: Option<Entity<HelpView>>,

    /// 右侧知识面板（大纲 / 反链 / 标签）是否可见
    right_visible: bool,
    /// 右侧面板当前标签页
    right_tab: RightTab,
    /// 库索引：双向链接、反链、标签、搜索。按需重建。
    vault_index: Option<VaultIndex>,
    /// 索引是否过期（保存 / 新建 / 换库后置位，渲染时重建）
    index_dirty: bool,
    /// 搜索面板
    search: Option<Entity<SearchView>>,
    /// 关系图谱浮层
    graph: Option<Entity<GraphView>>,
    /// 库问答浮层（本地检索 + AI）
    chat: Option<Entity<ChatView>>,
    /// 白板浮层（笔记里的 ```board 代码块）
    board: Option<Entity<BoardView>>,
    /// 历史版本浮层
    history: Option<Entity<HistoryView>>,
    /// 上次看到的笔记库目录时间戳，用于发现"别的程序改了库"
    vault_mtime: Option<std::time::SystemTime>,
    /// 已经就"外部修改"提示过，避免每两秒刷一次同样的提示
    external_warned: bool,
}

/// 右侧知识面板的标签页。
#[derive(Clone, Copy, PartialEq, Eq)]
enum RightTab {
    Outline,
    Backlinks,
    Tags,
}

/// 右键 AI 菜单的状态。
#[derive(Clone, Copy)]
struct AiMenu {
    pos: gpui::Point<Pixels>,
}

impl Workspace {
    fn new(vault_root: Option<PathBuf>, open: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let dark = false;
        let theme = if dark { Theme::dark() } else { Theme::light() };

        let doc = match open.as_ref() {
            Some(p) => Document::open(p).unwrap_or_else(|e| {
                eprintln!("打开 {} 失败：{e}", p.display());
                Document::from_str(WELCOME)
            }),
            None => Document::from_str(WELCOME),
        };

        let ai_cfg = AiConfig::load();
        let editor = cx.new(|cx| EditorView::new(Editor::new(doc), theme, ai_cfg.clone(), cx));

        // 笔记库打不开不是致命错误（可能只是路径不存在），退化成"无侧栏内容"
        let vault = vault_root.and_then(|root| match Vault::open(&root) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("打开笔记库 {} 失败：{e}", root.display());
                None
            }
        });

        // 第一次打开一个没有 templates/ 的库时，放一份起步模板进去：
        // 否则"模板"这个功能在 UI 里根本没有可见入口，用户无从发现。
        if let Some(v) = vault.as_ref() {
            let _ = template::seed_if_missing(v.root());
        }

        // 首次构建库索引（若已有笔记库）
        let vault_index = vault.as_ref().map(|v| VaultIndex::build(v));
        editor.update(cx, |ev, _cx| ev.set_index(vault_index.clone()));

        let vault_mtime = vault
            .as_ref()
            .and_then(|v| std::fs::metadata(v.root()).ok())
            .and_then(|m| m.modified().ok());

        Self {
            editor,
            vault,
            theme,
            dark,
            show_sidebar: false,
            selected: None,
            notice: None,
            focus: cx.focus_handle(),
            ai_cfg,
            context_menu: None,
            command_palette: None,
            settings: None,
            help: None,
            right_visible: false,
            chrome_pinned: false,
            chrome_hovered: false,
            right_tab: RightTab::Outline,
            vault_index,
            index_dirty: false,
            search: None,
            graph: None,
            chat: None,
            board: None,
            history: None,
            vault_mtime,
            external_warned: false,
        }
    }

    fn set_notice(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notice = Some(msg.into());
        cx.notify();
    }

    // ---------- 文件命令 ----------

    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        let has_path = self
            .editor
            .read(cx)
            .editor
            .doc
            .path()
            .map(|p| p.to_path_buf());

        match has_path {
            Some(_) => {
                // 写盘之前先留一份旧内容：这样"保存"这个动作永远可以反悔，
                // 而反悔的成本只是库目录里多一个小文件。
                self.snapshot_current(cx);
                let res = self
                    .editor
                    .update(cx, |v, _| v.editor.doc.save().map_err(|e| e.to_string()));
                match res {
                    Ok(()) => {
                        self.set_notice("已保存", cx);
                        // 保存可能改变了当前笔记的链接 / 标签，标记索引过期
                        self.index_dirty = true;
                    }
                    Err(e) => self.set_notice(format!("保存失败：{e}"), cx),
                }
                self.editor.update(cx, |_, cx| cx.notify());
            }
            None => self.save_as(&SaveAs, window, cx),
        }
    }

    /// 给当前打开的笔记留一份历史快照（不在库内 / 无笔记库时静默跳过）。
    fn snapshot_current(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.vault.as_ref().map(|v| v.root().to_path_buf()) else {
            return;
        };
        let Some(path) = self
            .editor
            .read(cx)
            .editor
            .doc
            .path()
            .map(|p| p.to_path_buf())
        else {
            return;
        };
        let _ = history::snapshot(&root, &path);
    }

    fn save_as(&mut self, _: &SaveAs, _window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.editor.clone();
        let dir = self.vault.as_ref().map(|v| v.root().to_path_buf());

        // 文件对话框是阻塞的系统调用，必须放到后台任务里，否则会卡住整个 UI 线程
        cx.spawn(async move |this, cx| {
            let dir = dir.unwrap_or_else(|| PathBuf::from("."));
            let picked = cx.update(|cx| cx.prompt_for_new_path(&dir, Some("未命名.md")));
            let Ok(rx) = picked else { return };
            let Ok(Ok(Some(path))) = rx.await else { return };

            let res = cx.update(|cx| {
                editor.update(cx, |v, _| {
                    v.editor.doc.save_as(&path).map_err(|e| e.to_string())
                })
            });
            let msg = match res {
                Ok(Ok(())) => format!("已保存到 {}", path.display()),
                Ok(Err(e)) => format!("保存失败：{e}"),
                Err(e) => format!("保存失败：{e}"),
            };
            let _ = this.update(cx, |this, cx| {
                this.reload_vault();
                this.set_notice(msg, cx);
            });
        })
        .detach();
    }

    fn open_file(&mut self, _: &OpenFile, _window: &mut Window, cx: &mut Context<Self>) {
        let editor = self.editor.clone();
        cx.spawn(async move |this, cx| {
            let opts = gpui::PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: None,
            };
            let Ok(rx) = cx.update(|cx| cx.prompt_for_paths(opts)) else {
                return;
            };
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else {
                return;
            };

            match Document::open(&path) {
                Ok(doc) => {
                    let _ = cx.update(|cx| {
                        editor.update(cx, |v, cx| {
                            v.open(doc);
                            cx.notify();
                        })
                    });
                    let _ = this.update(cx, |this, cx| {
                        this.index_dirty = true;
                        this.set_notice(format!("已打开 {}", path.display()), cx)
                    });
                }
                Err(e) => {
                    let _ = this.update(cx, |this, cx| {
                        this.set_notice(format!("打开失败：{e}"), cx)
                    });
                }
            }
        })
        .detach();
    }

    fn open_vault(&mut self, _: &OpenVault, _window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let opts = gpui::PathPromptOptions {
                files: false,
                directories: true,
                multiple: false,
                prompt: None,
            };
            let Ok(rx) = cx.update(|cx| cx.prompt_for_paths(opts)) else {
                return;
            };
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(root) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| match Vault::open(&root) {
                Ok(v) => {
                    this.vault = Some(v);
                    this.selected = None;
                    this.show_sidebar = true;
                    this.set_notice(format!("笔记库：{}", root.display()), cx);
                }
                Err(e) => this.set_notice(format!("打开笔记库失败：{e}"), cx),
            });
        })
        .detach();
    }

    fn new_note(&mut self, _: &NewNote, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.as_mut() else {
            // 没有笔记库时退化为"新建未命名缓冲区"，保存时再选路径
            self.editor.update(cx, |v, cx| {
                v.open(Document::empty());
                cx.notify();
            });
            self.set_notice("新建（保存时选择位置）", cx);
            return;
        };

        match vault.create_note("未命名") {
            Ok(path) => match Document::open(&path) {
                Ok(doc) => {
                    self.editor.update(cx, |v, cx| {
                        v.open(doc);
                        cx.notify();
                    });
                    self.reload_vault();
                    self.index_dirty = true;
                    self.set_notice(format!("新建 {}", path.display()), cx);
                }
                Err(e) => self.set_notice(format!("新建后打开失败：{e}"), cx),
            },
            Err(e) => self.set_notice(format!("新建失败：{e}"), cx),
        }
    }

    fn reload_vault(&mut self) {
        if let Some(v) = self.vault.as_mut() {
            let _ = v.reload();
        }
    }

    // ---------- 知识面板 / 索引 ----------

    /// 索引过期时（保存 / 新建 / 换库后）重建。成本与笔记数线性相关，中等库毫秒级。
    fn ensure_index(&mut self, cx: &mut Context<Self>) {
        if self.index_dirty {
            if let Some(v) = self.vault.as_ref() {
                match self.vault_index.as_mut() {
                    Some(idx) => idx.rebuild(v),
                    None => self.vault_index = Some(VaultIndex::build(v)),
                }
                // 编辑器用它做查询块结果，索引换了必须同步并清缓存
                let index = self.vault_index.clone();
                self.editor.update(cx, |ev, cx| {
                    ev.set_index(index);
                    cx.notify();
                });
            }
            self.index_dirty = false;
        }
    }

    /// 点击渲染态块里的 `[[链接]]` 时由编辑器发出。解析目标并打开对应笔记。
    fn open_wikilink(&mut self, a: &OpenWikilink, window: &mut Window, cx: &mut Context<Self>) {
        let target_path = self
            .vault_index
            .as_ref()
            .and_then(|idx| idx.resolve(&a.0))
            .map(|p| p.to_path_buf());
        let Some(path) = target_path else {
            self.set_notice(format!("未找到笔记：{}", a.0), cx);
            return;
        };
        match Document::open(&path) {
            Ok(doc) => {
                self.editor.update(cx, |v, cx| {
                    v.open(doc);
                    cx.notify();
                });
                let handle = self.editor.read(cx).focus_handle(cx);
                window.focus(&handle);
                self.index_dirty = true;
                self.right_tab = RightTab::Backlinks;
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.set_notice(format!("打开链接 → {name}"), cx);
            }
            Err(e) => self.set_notice(format!("打开失败：{e}"), cx),
        }
    }

    /// 搜索结果被点击时由搜索面板发出：打开笔记并跳转到指定行。
    fn open_note_at(&mut self, a: &OpenNoteAt, window: &mut Window, cx: &mut Context<Self>) {
        match Document::open(&a.0) {
            Ok(doc) => {
                self.editor.update(cx, |v, cx| {
                    v.open(doc);
                    cx.notify();
                });
                self.editor.update(cx, |v, cx| v.goto_line(a.1, cx));
                let handle = self.editor.read(cx).focus_handle(cx);
                window.focus(&handle);
                self.search = None;
                // 图谱是整屏浮层，跳转后必须收起来，否则看不到刚打开的笔记
                self.graph = None;
                self.index_dirty = true;
                self.right_tab = RightTab::Backlinks;
                let name = a
                    .0
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                self.set_notice(format!("已打开 {name}（行 {}）", a.1 + 1), cx);
            }
            Err(e) => self.set_notice(format!("打开失败：{e}"), cx),
        }
    }

    /// 全局搜索入口（命令面板快捷键 / 顶栏按钮）。`initial` 为预填关键词。
    fn open_search(&mut self, initial: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.is_none() {
            let Some(index) = self.vault_index.clone() else {
                self.set_notice("未打开笔记库，无法搜索", cx);
                return;
            };
            let theme = self.theme;
            let sv = cx.new(|cx| SearchView::new(theme, index, initial, cx));
            let input_focus = sv.read(cx).input.read(cx).focus_handle(cx);
            window.focus(&input_focus);
            self.search = Some(sv);
            cx.notify();
        }
    }

    /// `OpenSearch` 动作的包装（该动作无负载，预填为空）。
    fn open_search_action(&mut self, _: &OpenSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(String::new(), window, cx);
    }

    /// 切换右侧知识面板可见性。
    fn toggle_right_panel(&mut self, _: &ToggleRightPanel, _w: &mut Window, cx: &mut Context<Self>) {
        self.right_visible = !self.right_visible;
        cx.notify();
    }

    /// 打开关系图谱浮层。布局在 core 里算好，这里只负责展示。
    fn open_graph(&mut self, _: &OpenGraph, window: &mut Window, cx: &mut Context<Self>) {
        if self.graph.is_some() {
            return;
        }
        // 图谱必须基于最新链接关系，先补一次索引重建
        self.ensure_index(cx);
        let Some(index) = self.vault_index.as_ref() else {
            self.set_notice("未打开笔记库，无法生成图谱", cx);
            return;
        };
        let graph = index.graph(GraphView::MAX_NODES);
        if graph.nodes.is_empty() {
            self.set_notice("笔记库里还没有笔记", cx);
            return;
        }
        let theme = self.theme;
        let gv = cx.new(|cx| GraphView::new(theme, graph, cx));
        let handle = gv.read(cx).focus_handle(cx);
        window.focus(&handle);
        self.graph = Some(gv);
        cx.notify();
    }

    /// 打开库问答浮层：先在本地检索，再把片段交给 AI。
    fn open_chat(&mut self, _: &OpenChat, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat.is_some() {
            return;
        }
        self.ensure_index(cx);
        let Some(index) = self.vault_index.clone() else {
            self.set_notice("未打开笔记库，库问答需要笔记库作为知识来源", cx);
            return;
        };
        let theme = self.theme;
        let cfg = self.ai_cfg.clone();
        let cv = cx.new(|cx| ChatView::new(theme, index, cfg, cx));
        let input_focus = cv.read(cx).input.read(cx).focus_handle(cx);
        window.focus(&input_focus);
        self.chat = Some(cv);
        cx.notify();
    }

    /// 打开今天的每日笔记（不存在则连同 `daily/` 目录一起创建）。
    fn open_daily(&mut self, _: &OpenDaily, window: &mut Window, cx: &mut Context<Self>) {
        let today = date::today();
        let rel = date::daily_note_rel(&today);

        // 每日笔记的内容优先级：`templates/每日.md` > 只有日期标题。
        // 有模板时 `{{date}}` 等变量在写入前展开，写进去的就是最终文本。
        let body = self
            .vault
            .as_ref()
            .and_then(|v| v.templates().into_iter().find(|t| t.name == "每日"))
            .map(|t| template::expand(&t.body, &template::Ctx::on(today.clone(), today.clone())))
            .unwrap_or_else(|| format!("# {today}\n\n"));

        let Some(vault) = self.vault.as_mut() else {
            self.set_notice("未打开笔记库，无法创建每日笔记", cx);
            return;
        };
        let is_new = !vault.root().join(&rel).exists();
        let path = match vault.ensure_note_at_with(&rel, &body) {
            Ok(p) => p,
            Err(e) => {
                self.set_notice(format!("创建每日笔记失败：{e}"), cx);
                return;
            }
        };

        match Document::open(&path) {
            Ok(doc) => {
                self.editor.update(cx, |v, cx| {
                    v.open(doc);
                    cx.notify();
                });
                let handle = self.editor.read(cx).focus_handle(cx);
                window.focus(&handle);
                self.reload_vault();
                self.index_dirty = true;
                self.set_notice(
                    if is_new {
                        format!("已创建每日笔记 {today}")
                    } else {
                        format!("打开每日笔记 {today}")
                    },
                    cx,
                );
            }
            Err(e) => self.set_notice(format!("打开失败：{e}"), cx),
        }
    }

    // ---------- 模板 / 白板 / 历史 ----------

    /// 按模板新建笔记：展开变量 -> 落到库里 -> 打开。
    fn new_note_from_template(
        &mut self,
        a: &NewNoteFromTemplate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let body = match std::fs::read_to_string(&a.0) {
            Ok(b) => b,
            Err(e) => {
                self.set_notice(format!("读取模板失败：{e}"), cx);
                return;
            }
        };
        let name = a
            .0
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名".into());

        let Some(vault) = self.vault.as_mut() else {
            // 没有笔记库时退化成"新建缓冲区"，但模板内容照样用上
            let text = template::expand(&body, &template::Ctx::now(name));
            self.editor.update(cx, |v, cx| {
                v.open(Document::from_str(&text));
                cx.notify();
            });
            self.set_notice("已套用模板（保存时选择位置）", cx);
            return;
        };

        let text = template::expand(&body, &template::Ctx::now(name.clone()));
        match vault.create_note_with(&name, &text) {
            Ok(path) => match Document::open(&path) {
                Ok(doc) => {
                    self.editor.update(cx, |v, cx| {
                        v.open(doc);
                        cx.notify();
                    });
                    let handle = self.editor.read(cx).focus_handle(cx);
                    window.focus(&handle);
                    self.reload_vault();
                    self.index_dirty = true;
                    let file = path
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    self.set_notice(format!("按模板新建 {file}"), cx);
                }
                Err(e) => self.set_notice(format!("新建后打开失败：{e}"), cx),
            },
            Err(e) => self.set_notice(format!("新建失败：{e}"), cx),
        }
    }

    /// 库里没有模板时的提示：把模板目录建出来并告诉用户往哪放。
    fn hint_templates(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.vault.as_ref().map(|v| v.root().to_path_buf()) else {
            self.set_notice("未打开笔记库，模板放在库内的 templates/ 目录", cx);
            return;
        };
        let _ = template::seed_if_missing(&root);
        let dir = root.join(template::TEMPLATE_DIR);
        self.open_path_in_shell(&dir);
        self.set_notice(
            format!("模板目录：{}　（放进去的 .md 会出现在命令面板的「模板」分组）", dir.display()),
            cx,
        );
    }

    /// 打开白板浮层：白板存在当前笔记的 ```board 代码块里。
    fn open_board(&mut self, _: &OpenBoard, window: &mut Window, cx: &mut Context<Self>) {
        if self.board.is_some() {
            return;
        }
        let Some(path) = self
            .editor
            .read(cx)
            .editor
            .doc
            .path()
            .map(|p| p.to_path_buf())
        else {
            self.set_notice("白板保存在笔记里：先打开或新建一篇笔记", cx);
            return;
        };
        let theme = self.theme;
        let bv = cx.new(|cx| BoardView::open(theme, path, cx));
        let handle = bv.read(cx).focus_handle(cx);
        window.focus(&handle);
        self.board = Some(bv);
        cx.notify();
    }

    /// 白板改动落盘。
    ///
    /// 正文取**编辑器里的最新版本**（如果编辑器打开的正是这篇），这样刚在编辑器里
    /// 改过还没保存的内容不会被白板的写回抹掉。
    fn board_sync(&mut self, a: &BoardSync, _window: &mut Window, cx: &mut Context<Self>) {
        let path = a.0.clone();
        let same = self
            .editor
            .read(cx)
            .editor
            .doc
            .path()
            .map(|p| p == path.as_path())
            .unwrap_or(false);

        let md = if same {
            let doc = &self.editor.read(cx).editor.doc;
            doc.slice_to_string(0..doc.len_bytes())
        } else {
            std::fs::read_to_string(&path).unwrap_or_default()
        };

        let board = board::Board::from_json(&a.1).unwrap_or_default();
        match std::fs::write(&path, board::splice(&md, &board)) {
            Ok(()) => {
                // 编辑器里那份还是旧的（没有 board 块），重新读盘保持一致 ——
                // 否则用户下次 Ctrl+S 会把刚写进去的白板冲掉。
                if same {
                    if let Ok(doc) = Document::open(&path) {
                        self.editor.update(cx, |v, cx| {
                            v.open(doc);
                            cx.notify();
                        });
                    }
                }
                self.index_dirty = true;
            }
            Err(e) => self.set_notice(format!("白板保存失败：{e}"), cx),
        }
    }

    /// 手动打一个存档点：把当前内容写成一份快照。
    fn snapshot_now(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.vault.as_ref().map(|v| v.root().to_path_buf()) else {
            self.set_notice("未打开笔记库，快照存在库内 .fastnote/history/", cx);
            return;
        };
        let Some(path) = self
            .editor
            .read(cx)
            .editor
            .doc
            .path()
            .map(|p| p.to_path_buf())
        else {
            self.set_notice("当前内容还没有落到文件，无法留快照", cx);
            return;
        };
        // 先把内存里的内容落盘，快照的才是"此刻"的样子
        self.editor
            .update(cx, |v, _| v.editor.doc.save().map_err(|e| e.to_string()).ok());
        match history::snapshot(&root, &path) {
            Ok(Some(_)) => self.set_notice("已创建快照", cx),
            Ok(None) => self.set_notice("内容与上一份快照相同，未重复留档", cx),
            Err(e) => self.set_notice(format!("快照失败：{e}"), cx),
        }
    }

    /// 打开历史版本浮层。
    fn open_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.history.is_some() {
            return;
        }
        let Some(root) = self.vault.as_ref().map(|v| v.root().to_path_buf()) else {
            self.set_notice("未打开笔记库，历史版本存在库内 .fastnote/history/", cx);
            return;
        };
        let Some(path) = self
            .editor
            .read(cx)
            .editor
            .doc
            .path()
            .map(|p| p.to_path_buf())
        else {
            self.set_notice("先打开一篇库内笔记，才能看它的历史", cx);
            return;
        };
        // 先给"此刻"留一份，保证列表里一定有现在这个状态可比
        self.snapshot_current(cx);

        let theme = self.theme;
        let hv = cx.new(|cx| HistoryView::open(theme, root, path, cx));
        let handle = hv.read(cx).focus_handle(cx);
        window.focus(&handle);
        self.history = Some(hv);
        cx.notify();
    }

    /// `OpenHistory` 动作的包装（该动作无负载）。
    fn open_history_action(&mut self, _: &OpenHistory, window: &mut Window, cx: &mut Context<Self>) {
        self.open_history(window, cx);
    }

    /// 恢复某个历史版本。
    fn restore_snapshot(&mut self, a: &RestoreSnapshot, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.vault.as_ref().map(|v| v.root().to_path_buf()) else {
            return;
        };
        match history::restore(&root, &a.0, &a.1) {
            Ok(()) => {
                let same = self
                    .editor
                    .read(cx)
                    .editor
                    .doc
                    .path()
                    .map(|p| p == a.0.as_path())
                    .unwrap_or(false);
                if same {
                    if let Ok(doc) = Document::open(&a.0) {
                        self.editor.update(cx, |v, cx| {
                            v.open(doc);
                            cx.notify();
                        });
                    }
                }
                self.index_dirty = true;
                if let Some(h) = self.history.clone() {
                    h.update(cx, |v, cx| {
                        v.after_restore("已恢复所选版本");
                        cx.notify();
                    });
                }
                self.set_notice("已恢复（恢复前的版本也留了档，可以再退回去）", cx);
            }
            Err(e) => self.set_notice(format!("恢复失败：{e}"), cx),
        }
    }

    /// 用系统文件管理器打开一个目录。
    fn open_path_in_shell(&self, dir: &std::path::Path) {
        let _ = std::fs::create_dir_all(dir);
        #[cfg(windows)]
        let _ = std::process::Command::new("explorer").arg(dir).status();
        #[cfg(not(windows))]
        let _ = std::process::Command::new("open").arg(dir).status();
    }

    /// 命令面板选中的界面命令统一在这里分发。
    fn run_app_cmd(&mut self, a: &RunAppCmd, window: &mut Window, cx: &mut Context<Self>) {
        self.command_palette = None;
        match a.0 {
            AppCmd::NewNote => self.new_note(&NewNote, window, cx),
            AppCmd::NewFromTemplate => self.hint_templates(cx),
            AppCmd::OpenFile => self.open_file(&OpenFile, window, cx),
            AppCmd::OpenVault => self.open_vault(&OpenVault, window, cx),
            AppCmd::SaveAs => self.save_as(&SaveAs, window, cx),
            AppCmd::ToggleSidebar => self.toggle_sidebar(&ToggleSidebar, window, cx),
            AppCmd::ToggleTheme => self.toggle_theme(&ToggleTheme, window, cx),
            AppCmd::ToggleRightPanel => self.toggle_right_panel(&ToggleRightPanel, window, cx),
            AppCmd::FocusEditor => self.focus_editor(&FocusEditor, window, cx),
            AppCmd::OpenSearch => self.open_search(String::new(), window, cx),
            AppCmd::OpenGraph => self.open_graph(&OpenGraph, window, cx),
            AppCmd::OpenDaily => self.open_daily(&OpenDaily, window, cx),
            AppCmd::OpenChat => self.open_chat(&OpenChat, window, cx),
            AppCmd::OpenBoard => self.open_board(&OpenBoard, window, cx),
            AppCmd::Snapshot => self.snapshot_now(cx),
            AppCmd::OpenHistory => self.open_history(window, cx),
            AppCmd::OpenSettings => self.open_settings(&OpenSettings, window, cx),
            AppCmd::OpenHelp => self.open_help(&OpenHelp, window, cx),
            AppCmd::OpenConfigDir => self.open_config_dir(&OpenConfigDir, window, cx),
        }
        cx.notify();
    }

    // ---------- 外部变更 ----------

    /// 每两秒看一眼：当前文件是不是被别的程序改了，库目录有没有增删文件。
    ///
    /// 用轮询而不是 `notify`：这里要的是"别的程序改了库"，轮询一次
    /// metadata + 读一个小文件就够了，而文件监听要引入依赖、还要处理
    /// 编辑器自己写盘带来的回声。
    fn poll_external_changes(&mut self, cx: &mut Context<Self>) {
        // 1) 当前文档：磁盘内容与内存不一致，且内存没有未保存改动 -> 外部改了它
        let path = self
            .editor
            .read(cx)
            .editor
            .doc
            .path()
            .map(|p| p.to_path_buf());
        if let Some(path) = path {
            let dirty = self.editor.read(cx).editor.doc.is_dirty();
            if let Ok(disk) = std::fs::read_to_string(&path) {
                let mem = {
                    let doc = &self.editor.read(cx).editor.doc;
                    doc.slice_to_string(0..doc.len_bytes())
                };
                if disk != mem {
                    if dirty {
                        if !self.external_warned {
                            self.external_warned = true;
                            self.set_notice("磁盘上的文件已被外部修改；先保存或撤销你的改动再重载", cx);
                        }
                    } else if let Ok(doc) = Document::open(&path) {
                        self.editor.update(cx, |v, cx| {
                            v.open(doc);
                            cx.notify();
                        });
                        self.index_dirty = true;
                        self.external_warned = false;
                        let name = path
                            .file_name()
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_default();
                        self.set_notice(format!("{name} 已被外部修改，已重新载入"), cx);
                    }
                }
            }
        }

        // 2) 库目录：目录 mtime 变了说明有文件增删（目录里改内容不会动它）
        let Some(root) = self.vault.as_ref().map(|v| v.root().to_path_buf()) else {
            return;
        };
        let now = std::fs::metadata(&root).ok().and_then(|m| m.modified().ok());
        if now.is_some() && now != self.vault_mtime {
            self.vault_mtime = now;
            self.reload_vault();
            self.index_dirty = true;
            cx.notify();
        }
    }

    /// 启动外部变更轮询。
    fn start_watch(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_secs(2))
                .await;
            if this
                .update(cx, |ws: &mut Workspace, cx| ws.poll_external_changes(cx))
                .is_err()
            {
                // 窗口没了，循环自行退出
                return;
            }
        })
        .detach();
    }

    /// 渲染右侧知识面板：大纲 / 反链 / 标签 三个标签页。
    fn render_right_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let this = cx.entity();
        let tab = self.right_tab;

        let outline = std::rc::Rc::new(self.editor.read(cx).outline());
        let cur_path = self.editor.read(cx).editor.doc.path().map(|p| p.to_path_buf());
        let backlinks: std::rc::Rc<Vec<LinkRef>> = std::rc::Rc::new(
            match (cur_path.as_ref(), self.vault_index.as_ref()) {
                (Some(p), Some(idx)) => idx.backlinks(p),
                _ => Vec::new(),
            },
        );
        let tags: std::rc::Rc<Vec<(String, usize)>> = std::rc::Rc::new(
            self.vault_index
                .as_ref()
                .map(|idx| idx.all_tags())
                .unwrap_or_default(),
        );

        let count = match tab {
            RightTab::Outline => outline.len(),
            RightTab::Backlinks => backlinks.len(),
            RightTab::Tags => tags.len(),
        };

        // 标签页：不再整块铺底色，改成「底部 2px 强调条 + 字重」区分当前页，
        // 色觉差异用户靠「加粗 + 下划线」也能分辨。
        macro_rules! tab_cell {
            ($id:expr, $label:expr, $active:expr, $handler:expr) => {
                div()
                    .id($id)
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(34.))
                    .text_size(px(12.))
                    .border_b_2()
                    .when($active, |d| {
                        d.border_color(theme.accent)
                            .text_color(theme.heading)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                    })
                    .when(!$active, |d| {
                        d.border_color(theme.rule)
                            .text_color(theme.muted)
                            .hover(|s| s.text_color(theme.text))
                    })
                    .child($label)
                    .on_mouse_down(MouseButton::Left, $handler)
            };
        }

        div()
            .flex_none()
            .w(Metrics::RIGHT_PANEL)
            .h_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .border_l_1()
            .border_color(theme.rule)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .pr(px(6.))
                    .border_b_1()
                    .border_color(theme.rule)
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .child(tab_cell!(
                                "tab-outline",
                                "大纲",
                                tab == RightTab::Outline,
                                cx.listener(move |ws: &mut Workspace, _ev, _w, cx| {
                                    ws.right_tab = RightTab::Outline;
                                    cx.notify();
                                })
                            ))
                            .child(tab_cell!(
                                "tab-backlinks",
                                "反链",
                                tab == RightTab::Backlinks,
                                cx.listener(move |ws: &mut Workspace, _ev, _w, cx| {
                                    ws.right_tab = RightTab::Backlinks;
                                    cx.notify();
                                })
                            ))
                            .child(tab_cell!(
                                "tab-tags",
                                "标签",
                                tab == RightTab::Tags,
                                cx.listener(move |ws: &mut Workspace, _ev, _w, cx| {
                                    ws.right_tab = RightTab::Tags;
                                    cx.notify();
                                })
                            )),
                    )
                    .child(ui::icon_btn(
                        theme,
                        "btn-close-right",
                        "×",
                        cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, _w, cx| {
                            ws.right_visible = false;
                            cx.notify();
                        })
                    )),
            )
            .child(
                if count == 0 {
                    let msg = match tab {
                        RightTab::Outline => "这篇笔记还没有标题",
                        RightTab::Backlinks => {
                            if self.vault_index.is_some() {
                                "还没有其他笔记链接到这里"
                            } else {
                                "未打开笔记库"
                            }
                        }
                        RightTab::Tags => {
                            if self.vault_index.is_some() {
                                "还没有标签"
                            } else {
                                "未打开笔记库"
                            }
                        }
                    };
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .p_3()
                        .text_size(px(12.))
                        .text_color(theme.muted)
                        .child(msg)
                } else {
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .w_full()
                        .child(uniform_list("right-body", count, move |range, window, cx| {
                        let ws = this.read(cx);
                        let theme = ws.theme;
                        match tab {
                            RightTab::Outline => range
                                .filter_map(|i| {
                                    let (lvl, text, line) = outline.get(i)?;
                                    let lvl = *lvl;
                                    let line = *line;
                                    let text = text.clone();
                                    let indent = px(8. + (lvl as f32 - 1.) * 12.);
                                    Some(
                                        div()
                                            .id(i)
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .w_full()
                                            .pl(indent)
                                            .pr_2()
                                            .py_1()
                                            .text_size(px(13.))
                                            .text_color(theme.text)
                                            .when(
                                                lvl <= 2,
                                                |d| d.font_weight(gpui::FontWeight::SEMIBOLD),
                                            )
                                            .child(div().truncate().child(text))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                window.listener_for(
                                                    &this,
                                                    move |ws, _ev, _w, cx| {
                                                        ws.editor.update(cx, |v, cx| {
                                                            v.goto_line(line, cx)
                                                        });
                                                    },
                                                ),
                                            ),
                                    )
                                })
                                .collect(),
                            RightTab::Backlinks => range
                                .filter_map(|i| {
                                    let link = backlinks.get(i)?;
                                    let src = link.source.clone();
                                    let ln = link.line;
                                    let name = link.source_name.clone();
                                    let snippet = link.snippet.clone();
                                    Some(
                                    div()
                                        .id(i)
                                        .flex_col()
                                        .gap_1()
                                        .mx(px(6.))
                                        .px(px(8.))
                                        .py(px(6.))
                                        .rounded(Metrics::RADIUS_BTN)
                                        .hover(|s| s.bg(theme.hover))
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .text_color(theme.text)
                                                    .child(name),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(theme.muted)
                                                    .child(format!(
                                                        "{}:{}",
                                                        src.file_name()
                                                            .map(|s| s.to_string_lossy().to_string())
                                                            .unwrap_or_default(),
                                                        ln + 1
                                                    )),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(theme.muted)
                                                    .child(snippet),
                                            )
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                window.listener_for(
                                                    &this,
                                                    move |ws, _ev, window, cx| {
                                                        ws.open_note_at(
                                                            &OpenNoteAt(src.clone(), ln),
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                ),
                                            ),
                                    )
                                })
                                .collect(),
                            RightTab::Tags => range
                                .filter_map(|i| {
                                    let (tag, cnt) = tags.get(i)?;
                                    let tag = tag.clone();
                                    let cnt = *cnt;
                                    Some(
                                    div()
                                        .id(i)
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .mx(px(6.))
                                        .px(px(8.))
                                        .py(px(6.))
                                        .rounded(Metrics::RADIUS_BTN)
                                        .hover(|s| s.bg(theme.hover))
                                            .child(ui::chip(theme, format!("#{tag}")))
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(theme.muted)
                                                    .child(format!("{cnt} 篇")),
                                            )
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                window.listener_for(
                                                    &this,
                                                    move |ws, _ev, window, cx| {
                                                        ws.open_search(
                                                            format!("#{tag}"),
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                ),
                                            ),
                                    )
                                })
                                .collect(),
                        }
                        })
                        .flex_1()
                        .w_full()
                    )
                },
            )
    }

    // ---------- AI 交互 ----------

    /// 右键编辑器时弹出 AI 动作菜单。
    fn show_ai_menu(&mut self, pos: gpui::Point<Pixels>, _window: &mut Window, cx: &mut Context<Self>) {
        self.context_menu = Some(AiMenu { pos });
        cx.notify();
    }

    /// 执行一个替换型 AI 动作（选中文本或光标行），结果流式写回编辑器。
    fn do_ai_task(&mut self, task: Task, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.ai_cfg.is_configured() {
            self.set_notice("尚未配置 AI：按 Ctrl+Shift+, 选择端点，或用环境变量 FASTNOTE_API_KEY", cx);
            self.context_menu = None;
            return;
        }
        self.context_menu = None;
        let editor = self.editor.clone();
        editor.update(cx, |v, cx| v.run_insert_task(task, cx));
    }

    /// 命令面板提交的指令（含自定义指令）。
    fn submit_ai(&mut self, task: &SubmitAi, _window: &mut Window, cx: &mut Context<Self>) {
        let t = task.0.clone();
        self.command_palette = None;
        self.do_ai_task(t, _window, cx);
    }

    fn open_command_palette(&mut self, _: &OpenCommandPalette, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.is_none() {
            // 模板列表每次都重新扫：用户可能刚在 templates/ 里加了文件
            let templates: Vec<(String, PathBuf)> = self
                .vault
                .as_ref()
                .map(|v| {
                    v.templates()
                        .into_iter()
                        .map(|t| (t.name, t.path))
                        .collect()
                })
                .unwrap_or_default();
            let palette = cx.new(|cx| CommandPalette::new(self.theme, templates, cx));
            // 打开即把焦点落到输入框，用户可直接键入指令
            let input_focus = palette.read(cx).input.read(cx).focus_handle(cx);
            window.focus(&input_focus);
            self.command_palette = Some(palette);
            cx.notify();
        }
    }

    fn open_settings(&mut self, _: &OpenSettings, _window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_none() {
            let cfg = self.ai_cfg.clone();
            self.settings = Some(cx.new(|cx| SettingsView::new(self.theme, cfg, cx)));
            cx.notify();
        }
    }

    fn open_help(&mut self, _: &OpenHelp, window: &mut Window, cx: &mut Context<Self>) {
        if self.help.is_none() {
            let help = cx.new(|cx| HelpView::new(self.theme, cx));
            let handle = help.read(cx).focus_handle(cx);
            window.focus(&handle);
            self.help = Some(help);
            cx.notify();
        }
    }

    fn apply_preset(&mut self, p: &ApplyPreset, _window: &mut Window, cx: &mut Context<Self>) {
        let list = settings_view::presets();
        if let Some((_, cfg)) = list.get(p.0) {
            let cfg = cfg.clone();
            match cfg.save() {
                Ok(()) => {
                    self.ai_cfg = cfg.clone();
                    self.editor.update(cx, |v, _| v.set_ai_config(cfg));
                    self.set_notice("AI 端点已更新，开始使用吧", cx);
                }
                Err(e) => self.set_notice(format!("保存配置失败：{e}"), cx),
            }
        }
        self.settings = None;
    }

    fn open_config_dir(&mut self, _: &OpenConfigDir, _window: &mut Window, cx: &mut Context<Self>) {
        let dir = AiConfig::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        #[cfg(windows)]
        let _ = std::process::Command::new("explorer").arg(dir.clone()).status();
        #[cfg(not(windows))]
        let _ = std::process::Command::new("open").arg(dir.clone()).status();
        self.settings = None;
        self.set_notice(format!("配置目录：{}", dir.display()), cx);
    }

    /// 关闭一切浮层（Esc 或点击空白）。
    fn close_overlays(&mut self, _: &Close, window: &mut Window, cx: &mut Context<Self>) {
        let changed = self.command_palette.is_some()
            || self.settings.is_some()
            || self.context_menu.is_some()
            || self.help.is_some()
            || self.search.is_some()
            || self.graph.is_some()
            || self.chat.is_some()
            || self.board.is_some()
            || self.history.is_some();
        // 白板浮层关掉时，它的内容已经写进笔记了，索引得跟着刷新
        let board_closed = self.board.is_some();
        self.command_palette = None;
        self.settings = None;
        self.context_menu = None;
        self.help = None;
        self.search = None;
        self.graph = None;
        self.chat = None;
        self.board = None;
        self.history = None;
        if board_closed {
            self.index_dirty = true;
        }
        if changed {
            // 关闭任意浮层后把焦点交还编辑区，避免焦点悬空
            let handle = self.editor.read(cx).focus_handle(cx);
            window.focus(&handle);
            cx.notify();
        }
    }

    /// 渲染右键 AI 动作菜单。
    fn render_ai_menu(&self, menu: AiMenu, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let pos = menu.pos;
        let items: [(&'static str, Task); 5] = [
            ("润色", Task::Polish),
            ("翻译为英文", Task::Translate { target: "英文".into() }),
            ("翻译为中文", Task::Translate { target: "中文".into() }),
            ("摘要", Task::Summarize),
            ("解释", Task::Explain),
        ];
        div()
            .absolute()
            .inset_0()
            .on_mouse_down(MouseButton::Left, cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, _w, cx| {
                ws.context_menu = None;
                cx.notify();
            }))
            .child(
                div()
                    .absolute()
                    .top(pos.y)
                    .left(pos.x)
                    .flex_none()
                    .flex_col()
                    .min_w(px(132.))
                    .py(px(4.))
                    .bg(theme.bg)
                    .border_1()
                    .border_color(theme.border)
                    // 与 ui::panel 同一枚圆角令牌（原先硬编码 10，其它浮层都是 12，切换时能看出差别）。
                    // 投影比浮层面板更收紧（8/22 vs 12/32）：菜单只有 132px 宽，大扩散会显脏。
                    .rounded(Metrics::RADIUS_PANEL)
                    .overflow_hidden()
                    .shadow(vec![BoxShadow {
                        color: theme.shadow,
                        offset: point(px(0.), px(8.)),
                        blur_radius: px(22.),
                        spread_radius: px(-6.),
                    }])
                    // 菜单内部按下不冒泡：否则点菜单项会连带触发遮罩的「关闭」
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_ws: &mut Workspace, _ev: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation()
                        }),
                    )
                    .children(items.iter().enumerate().map(|(i, (label, task))| {
                        let t = task.clone();
                        div()
                            .id(i)
                            .mx(px(4.))
                            .px(px(9.))
                            .py(px(6.))
                            .rounded(Metrics::RADIUS_BTN)
                            .text_size(px(13.))
                            .text_color(theme.text)
                            .cursor(gpui::CursorStyle::PointingHand)
                            .hover(|s| s.bg(theme.hover))
                            .child(label.to_string())
                            .on_mouse_down(MouseButton::Left, cx.listener(move |ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                                ws.do_ai_task(t.clone(), window, cx);
                            }))
                    })),
            )
    }

    // ---------- 视图命令 ----------

    fn toggle_sidebar(&mut self, _: &ToggleSidebar, _w: &mut Window, cx: &mut Context<Self>) {
        self.show_sidebar = !self.show_sidebar;
        cx.notify();
    }

    /// 顶栏默认只靠鼠标移到顶端浮现；这个命令把它「钉住」常显。
    fn toggle_chrome(&mut self, _: &ToggleChrome, _w: &mut Window, cx: &mut Context<Self>) {
        self.chrome_pinned = !self.chrome_pinned;
        if !self.chrome_pinned {
            self.chrome_hovered = false;
        }
        cx.notify();
    }

    /// 顶栏当前是否可见：钉住，或鼠标停在热区/工具条上。
    fn chrome_visible(&self) -> bool {
        self.chrome_pinned || self.chrome_hovered
    }

    /// 是否有浮层占着屏幕（有的话顶部工具条就收起来）。
    fn overlay_open(&self) -> bool {
        self.command_palette.is_some()
            || self.settings.is_some()
            || self.help.is_some()
            || self.search.is_some()
            || self.graph.is_some()
            || self.chat.is_some()
            || self.board.is_some()
            || self.history.is_some()
    }

    fn toggle_theme(&mut self, _: &ToggleTheme, _w: &mut Window, cx: &mut Context<Self>) {
        self.dark = !self.dark;
        self.theme = if self.dark {
            Theme::dark()
        } else {
            Theme::light()
        };
        let t = self.theme;
        self.editor.update(cx, |v, cx| {
            v.theme = t;
            cx.notify();
        });
        cx.notify();
    }

    fn focus_editor(&mut self, _: &FocusEditor, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.editor.read(cx).focus_handle(cx);
        window.focus(&handle);
    }

    /// 点击侧栏条目：目录则展开/收起，笔记则打开。
    fn click_entry(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(vault) = self.vault.as_mut() else { return };
        let Some(entry) = vault.entries().get(index) else {
            return;
        };

        if entry.is_dir {
            let _ = vault.toggle(index);
            cx.notify();
            return;
        }

        let path = entry.path.clone();
        match Document::open(&path) {
            Ok(doc) => {
                self.selected = Some(index);
                self.editor.update(cx, |v, cx| {
                    v.open(doc);
                    cx.notify();
                });
                let handle = self.editor.read(cx).focus_handle(cx);
                window.focus(&handle);
                self.notice = None;
                cx.notify();
            }
            Err(e) => self.set_notice(format!("打开失败：{e}"), cx),
        }
    }

    // ---------- 各区域渲染 ----------

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let count = self.vault.as_ref().map(|v| v.entries().len()).unwrap_or(0);
        let selected = self.selected;
        let root_name = self
            .vault
            .as_ref()
            .and_then(|v| v.root().file_name().map(|s| s.to_string_lossy().to_string()))
            .unwrap_or_else(|| "未选择笔记库".to_string());

        let this = cx.entity();

        div()
            .flex()
            .flex_col()
            .w(Metrics::SIDEBAR)
            .h_full()
            .bg(theme.surface)
            .border_r_1()
            .border_color(theme.rule)
            .child(
                // 库名标题栏 + 收起按钮（不再只靠快捷键）
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .h(Metrics::CHROME_H)
                    .pl(px(14.))
                    .pr(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme.heading)
                                    .child(root_name),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.muted)
                                    .child(format!("{count} 个条目")),
                            ),
                    )
                    .child(ui::icon_btn(
                        theme,
                        "btn-collapse",
                        "‹",
                        cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                            ws.toggle_sidebar(&ToggleSidebar, window, cx);
                        }),
                    )),
            )
            .when(count == 0, |d| {
                d.child(
                    div()
                        .px(px(14.))
                        .py(px(16.))
                        .text_size(px(12.))
                        .text_color(theme.muted)
                        .child("还没有笔记库 · Ctrl+Shift+O 打开一个目录"),
                )
            })
            .child(
                uniform_list("vault-entries", count, move |range, window, cx| {
                    let ws = this.read(cx);
                    let Some(vault) = ws.vault.as_ref() else {
                        return Vec::new();
                    };
                    let entries = vault.entries();

                    range
                        .filter_map(|i| {
                            let e = entries.get(i)?;
                            let is_sel = selected == Some(i);
                            // 目录用 ▸/▾ 表示折叠状态，笔记用居中圆点占位，
                            // 靠符号而非颜色区分类型（对色觉差异友好）
                            let glyph = if e.is_dir {
                                if e.expanded { "▾" } else { "▸" }
                            } else {
                                "·"
                            };
                            let name = e.name.clone();
                            let indent = px(8. + e.depth as f32 * 12.);

                            Some(
                                div()
                                    .id(i)
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .h(Metrics::ROW_H)
                                    .mx(px(6.))
                                    .pl(indent)
                                    .pr(px(8.))
                                    .rounded(Metrics::RADIUS_BTN)
                                    .text_size(px(13.))
                                    .text_color(if e.is_dir { theme.heading } else { theme.text })
                                    .when(e.is_dir, |d| d.font_weight(gpui::FontWeight::SEMIBOLD))
                                    .when(is_sel, |d| d.bg(theme.active))
                                    .when(!is_sel, |d| d.hover(|s| s.bg(theme.hover)))
                                    .child(
                                        div()
                                            .w(px(12.))
                                            .flex_none()
                                            .text_color(theme.muted)
                                            .child(glyph),
                                    )
                                    .child(div().truncate().child(name))
                                    .on_mouse_down(
                                        gpui::MouseButton::Left,
                                        window.listener_for(&this, move |ws, _ev, window, cx| {
                                            ws.click_entry(i, window, cx)
                                        }),
                                    ),
                            )
                        })
                        .collect()
                })
                .flex_1()
                .w_full(),
            )
    }

    /// 侧栏收起时，左侧留一条几乎看不见的窄条：hover 才显出一个圆角抓手，
    /// 点击展开。比常驻一根 22px 的灰条干净，也不会挡住正文。
    fn render_expand_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        div()
            .id("expand-sidebar")
            .flex_none()
            .relative()
            .w(px(10.))
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .cursor(gpui::CursorStyle::PointingHand)
            .child(
                div()
                    .id("expand-grip")
                    .w(px(3.))
                    .h(px(46.))
                    .rounded(px(2.))
                    .bg(theme.rule)
                    .hover(|s| s.bg(theme.accent)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.toggle_sidebar(&ToggleSidebar, window, cx);
                }),
            )
    }

    /// 顶部工具条：浮动在编辑区之上，默认不显示，鼠标移到窗口顶端才浮现。
    ///
    /// 之前是「常驻一行 + 每个按钮各带一圈描边」，一屏十几个描边显得很吵；
    /// 现在按功能分组、去掉描边（层次只靠 hover 底色与文字色），并整体下沉成一条浮动条。
    fn render_top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let dark = self.dark;
        let sidebar_on = self.show_sidebar;
        let panel_on = self.right_visible;
        let pinned = self.chrome_pinned;

        div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.))
            .h(Metrics::CHROME_H)
            .px(px(8.))
            .bg(theme.bg)
            .border_1()
            .border_color(theme.border)
            .rounded(px(10.))
            .shadow(vec![BoxShadow {
                color: theme.shadow,
                offset: point(px(0.), px(10.)),
                blur_radius: px(28.),
                spread_radius: px(-6.),
            }])
            // 工具条浮在正文之上：点击不能穿透到编辑区，否则点按钮会顺手挪走光标
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_ws: &mut Workspace, _ev: &MouseDownEvent, _w, cx| cx.stop_propagation()),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|_ws: &mut Workspace, _ev: &MouseDownEvent, _w, cx| cx.stop_propagation()),
            )
            // ---- 品牌 ----
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl(px(5.))
                    .pr(px(9.))
                    .child(
                        div()
                            .flex_none()
                            .size(px(7.))
                            .rounded(px(2.))
                            .bg(theme.accent),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(theme.heading)
                            .child("fastnote"),
                    ),
            )
            .child(ui::v_divider(theme))
            // ---- 文件 ----
            .child(ui::btn(
                theme,
                "tb-new",
                "新建",
                ui::Kind::Primary,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.new_note(&NewNote, window, cx);
                }),
            ))
            .child(ui::btn(
                theme,
                "tb-open-file",
                "打开",
                ui::Kind::Plain,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.open_file(&OpenFile, window, cx);
                }),
            ))
            .child(ui::btn(
                theme,
                "tb-open-vault",
                "打开库",
                ui::Kind::Plain,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.open_vault(&OpenVault, window, cx);
                }),
            ))
            .child(ui::btn(
                theme,
                "tb-save",
                "保存",
                ui::Kind::Plain,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.save(&Save, window, cx);
                }),
            ))
            .child(ui::btn(
                theme,
                "tb-save-as",
                "另存为",
                ui::Kind::Quiet,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.save_as(&SaveAs, window, cx);
                }),
            ))
            .child(ui::v_divider(theme))
            // ---- 视图 ----
            .child(ui::btn(
                theme,
                "tb-sidebar",
                "侧栏",
                if sidebar_on { ui::Kind::On } else { ui::Kind::Plain },
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.toggle_sidebar(&ToggleSidebar, window, cx);
                }),
            ))
            .child(ui::btn(
                theme,
                "tb-right",
                "知识面板",
                if panel_on { ui::Kind::On } else { ui::Kind::Plain },
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.toggle_right_panel(&ToggleRightPanel, window, cx);
                }),
            ))
            .child(ui::btn(
                theme,
                "tb-palette",
                "命令面板",
                ui::Kind::Plain,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.open_command_palette(&OpenCommandPalette, window, cx);
                }),
            ))
            .child(ui::v_divider(theme))
            // ---- 知识库 ----
            .child(ui::btn(
                theme,
                "tb-graph",
                "图谱",
                ui::Kind::Plain,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.open_graph(&OpenGraph, window, cx);
                }),
            ))
            .child(ui::btn(
                theme,
                "tb-daily",
                "今日",
                ui::Kind::Plain,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.open_daily(&OpenDaily, window, cx);
                }),
            ))
            .child(ui::btn(
                theme,
                "tb-chat",
                "问答",
                ui::Kind::Plain,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.open_chat(&OpenChat, window, cx);
                }),
            ))
            .child(ui::btn(
                theme,
                "tb-board",
                "白板",
                ui::Kind::Plain,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.open_board(&OpenBoard, window, cx);
                }),
            ))
            .child(ui::btn(
                theme,
                "tb-history",
                "历史",
                ui::Kind::Plain,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.open_history(window, cx);
                }),
            ))
            .child(ui::v_divider(theme))
            // ---- 系统 ----
            .child(ui::btn(
                theme,
                "tb-theme",
                if dark { "浅色" } else { "深色" },
                ui::Kind::Quiet,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.toggle_theme(&ToggleTheme, window, cx);
                }),
            ))
            .child(ui::btn(
                theme,
                "tb-help",
                "帮助",
                ui::Kind::Quiet,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.open_help(&OpenHelp, window, cx);
                }),
            ))
            .child(ui::btn(
                theme,
                "tb-settings",
                "设置",
                ui::Kind::Quiet,
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.open_settings(&OpenSettings, window, cx);
                }),
            ))
            .child(ui::btn(
                theme,
                "tb-pin",
                "常显",
                if pinned { ui::Kind::On } else { ui::Kind::Quiet },
                cx.listener(|ws: &mut Workspace, _ev: &MouseDownEvent, window, cx| {
                    ws.toggle_chrome(&ToggleChrome, window, cx);
                }),
            ))
    }

    /// 顶部热区 + 浮动工具条。
    ///
    /// 收起时这里只剩一根 4px 细把手：鼠标一进窗口顶端就展开，移开自动收起。
    /// 热区与被展开的工具条写在**同一个** hover 元素里，鼠标从把手移到工具条的
    /// 路径始终落在这个 hitbox 内，不会中途闪一下。
    fn render_top_zone(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let visible = self.chrome_visible();

        div()
            .id("top-zone")
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(6.))
            .when(visible, |d| d.child(self.render_top_bar(cx)))
            .when(!visible, |d| {
                d.child(
                    div()
                        .flex_none()
                        .w(px(46.))
                        .h(px(4.))
                        .rounded(px(2.))
                        .bg(theme.rule),
                )
            })
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let ev = self.editor.read(cx);
        let doc = &ev.editor.doc;

        let title = doc.title();
        let dirty = doc.is_dirty();
        let (line, col) = ev.editor.cursor_line_col();
        let lines = doc.len_lines();
        let bytes = doc.len_bytes();
        let streaming = ev.streaming;

        let size_text = if bytes >= 1024 * 1024 {
            format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
        } else if bytes >= 1024 {
            format!("{:.1} KB", bytes as f64 / 1024.0)
        } else {
            format!("{bytes} B")
        };

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(px(28.))
            .px(px(18.))
            .bg(theme.bg)
            .border_t_1()
            .border_color(theme.rule)
            .text_size(px(11.5))
            .text_color(theme.muted)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    // 未保存用「●+文字」双重表达，不单靠颜色
                    .when(dirty, |d| {
                        d.child(
                            div()
                                .text_color(theme.warn)
                                .font_weight(gpui::FontWeight::BOLD)
                                .child("● 未保存"),
                        )
                    })
                    .child(
                        div()
                            .text_color(theme.heading)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .when_some(self.notice.clone(), |d, n| {
                        d.child(div().text_color(theme.accent).child(n))
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(streaming, |d| {
                        d.child(
                            div()
                                .text_color(theme.accent)
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("AI 写入中…"),
                        )
                    })
                    .child(format!("{}:{}", line + 1, col + 1))
                    .child(div().text_color(theme.rule).child("·"))
                    .child(format!("{lines} 行"))
                    .child(div().text_color(theme.rule).child("·"))
                    .child(size_text),
            )
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        // 索引过期则先重建（保存 / 新建 / 换库后）
        self.ensure_index(cx);
        let sidebar = if self.show_sidebar {
            Some(self.render_sidebar(cx))
        } else {
            None
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.bg)
            .text_color(theme.text)
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::save_as))
            .on_action(cx.listener(Self::open_file))
            .on_action(cx.listener(Self::open_vault))
            .on_action(cx.listener(Self::new_note))
            .on_action(cx.listener(Self::toggle_sidebar))
            .on_action(cx.listener(Self::toggle_chrome))
            .on_action(cx.listener(Self::toggle_theme))
            .on_action(cx.listener(Self::focus_editor))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::open_command_palette))
            .on_action(cx.listener(Self::submit_ai))
            .on_action(cx.listener(Self::apply_preset))
            .on_action(cx.listener(Self::open_config_dir))
            .on_action(cx.listener(Self::open_help))
            .on_action(cx.listener(Self::open_search_action))
            .on_action(cx.listener(Self::toggle_right_panel))
            .on_action(cx.listener(Self::open_wikilink))
            .on_action(cx.listener(Self::open_note_at))
            .on_action(cx.listener(Self::open_graph))
            .on_action(cx.listener(Self::open_daily))
            .on_action(cx.listener(Self::open_chat))
            .on_action(cx.listener(Self::open_board))
            .on_action(cx.listener(Self::open_history_action))
            .on_action(cx.listener(Self::run_app_cmd))
            .on_action(cx.listener(Self::new_note_from_template))
            .on_action(cx.listener(Self::board_sync))
            .on_action(cx.listener(Self::restore_snapshot))
            .on_action(cx.listener(Self::close_overlays))
            // 顶部工具条的显隐：挂在根节点上按鼠标 y 判定。
            // 不用元素级 hover —— 元素级 hover 要求指针真正「进入」那个 hitbox，
            // 而热区在顶部 10px 内，指针从窗口外移进来时常常只在改过尺寸的帧里
            // 命中一次，容易漏。根节点铺满窗口，任何一次移动都能收到，判定更稳。
            // 46px 是热区底线，56px 是「已经展开后」的滞回底线，避免贴边来回闪。
            .on_mouse_move(cx.listener(
                |ws: &mut Workspace, ev: &MouseMoveEvent, _w, cx| {
                    if ws.overlay_open() {
                        return;
                    }
                    let y = ev.position.y / px(1.);
                    let limit = if ws.chrome_hovered { 56. } else { 46. };
                    let want = y <= limit;
                    if ws.chrome_hovered != want {
                        ws.chrome_hovered = want;
                        cx.notify();
                    }
                },
            ))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .overflow_hidden()
                    .when_some(sidebar, |d, s| d.child(s))
                    .when(!self.show_sidebar, |d| d.child(self.render_expand_button(cx)))
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .child(self.editor.clone())
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(|ws, ev: &MouseDownEvent, window, cx| {
                                    ws.show_ai_menu(ev.position, window, cx);
                                }),
                            ),
                    )
                    .when(self.right_visible, |d| d.child(self.render_right_panel(cx))),
            )
            .child(self.render_status_bar(cx))
            // 顶部浮动工具条：压在正文之上，所以放在浮层之前、内容之后。
            // 有浮层时收起来，免得遮罩后面的工具条还在响应悬停。
            .when(!self.overlay_open(), |d| d.child(self.render_top_zone(cx)))
            .when_some(self.context_menu.clone(), |d, menu| {
                d.child(self.render_ai_menu(menu, cx))
            })
            .when_some(self.command_palette.clone(), |d, p| {
                d.child(p)
            })
            .when_some(self.settings.clone(), |d, s| {
                d.child(s)
            })
            .when_some(self.help.clone(), |d, h| {
                d.child(h)
            })
            .when_some(self.search.clone(), |d, s| {
                d.child(s)
            })
            .when_some(self.graph.clone(), |d, g| {
                d.child(g)
            })
            .when_some(self.chat.clone(), |d, c| {
                d.child(c)
            })
            .when_some(self.board.clone(), |d, b| {
                d.child(b)
            })
            .when_some(self.history.clone(), |d, h| {
                d.child(h)
            })
    }
}

const WELCOME: &str = r#"# fastnote

即时渲染的 Markdown 笔记。光标所在的块显示**源码**，其他块显示渲染结果 —— 位置不跳。

## 快捷键

- `Ctrl-O` 打开文件，`Ctrl-Shift-O` 打开笔记库
- `Ctrl-N` 新建笔记，`Ctrl-S` 保存
- `Ctrl-B` 加粗，`Ctrl-I` 斜体，`Ctrl-\`` 行内代码
- `Ctrl-\` 收起侧栏，`Ctrl-Shift-L` 切换深浅色
- `Ctrl-Shift-F` 全库搜索，`Ctrl-Shift-G` 关系图谱
- `Ctrl-D` 今日笔记，`Ctrl-Shift-Q` 库问答
- `Ctrl-Shift-B` 白板，`Ctrl-Shift-H` 历史版本
- `Ctrl-K` 命令面板：所有命令与模板都在里面
- `Tab` 采纳 AI 续写，`Esc` 忽略

## 试试这些

> 引用块有左侧竖条，缩进与正文对齐。

- [ ] 待办项，`Ctrl-Enter` 切换勾选
- [x] 已完成的项

行内代码 `cargo run` 与**粗体**、*斜体*、~~删除线~~ 都是即时渲染的。

```rust
fn main() {
    println!("代码块不做行内解析");
}
```

---

把光标移到任意一行，看它变回源码。
"#;

fn main() {
    // 命令行：fastnote [文件或目录]
    let arg = std::env::args().nth(1).map(PathBuf::from);
    let (vault_root, open_file) = match arg {
        Some(p) if p.is_dir() => (Some(p), None),
        Some(p) => {
            let parent = p.parent().map(|d| d.to_path_buf());
            (parent, Some(p))
        }
        None => (std::env::current_dir().ok(), None),
    };

    Application::new().run(move |cx: &mut App| {
        cx.bind_keys(bindings());

        let bounds = Bounds::centered(None, size(px(1180.), px(800.)), cx);
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("fastnote".into()),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            window_min_size: Some(size(px(480.), px(360.))),
            app_id: Some("fastnote".into()),
            ..Default::default()
        };

        cx.open_window(opts, |window, cx| {
            let ws = cx.new(|cx| Workspace::new(vault_root.clone(), open_file.clone(), cx));
            // 启动即让编辑区拿到焦点，省掉用户多点一次
            let handle = ws.read(cx).editor.read(cx).focus_handle(cx);
            window.focus(&handle);
            // 开始盯着"别的程序改了库 / 改了当前文件"这件事
            let _ = ws.update(cx, |ws, cx| ws.start_watch(cx));
            ws
        })
        .expect("创建窗口失败");

        // 窗口关掉就退出进程，避免留下后台进程
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        cx.activate(true);
    });
}

/// 键绑定。编辑类动作限定在 `Editor` 上下文，避免和全局命令抢键。
fn bindings() -> Vec<KeyBinding> {
    let e = Some("Editor");
    vec![
        // 光标移动
        KeyBinding::new("left", MoveLeft, e),
        KeyBinding::new("right", MoveRight, e),
        KeyBinding::new("up", MoveUp, e),
        KeyBinding::new("down", MoveDown, e),
        KeyBinding::new("shift-left", SelectLeft, e),
        KeyBinding::new("shift-right", SelectRight, e),
        KeyBinding::new("shift-up", SelectUp, e),
        KeyBinding::new("shift-down", SelectDown, e),
        KeyBinding::new("ctrl-left", WordLeft, e),
        KeyBinding::new("ctrl-right", WordRight, e),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, e),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, e),
        KeyBinding::new("home", LineStart, e),
        KeyBinding::new("end", LineEnd, e),
        KeyBinding::new("shift-home", SelectLineStart, e),
        KeyBinding::new("shift-end", SelectLineEnd, e),
        KeyBinding::new("ctrl-home", DocStart, e),
        KeyBinding::new("ctrl-end", DocEnd, e),
        KeyBinding::new("pageup", PageUp, e),
        KeyBinding::new("pagedown", PageDown, e),
        // 编辑
        KeyBinding::new("backspace", Backspace, e),
        KeyBinding::new("delete", DeleteForward, e),
        KeyBinding::new("enter", Newline, e),
        KeyBinding::new("tab", Indent, e),
        KeyBinding::new("shift-tab", Outdent, e),
        KeyBinding::new("ctrl-a", SelectAll, e),
        KeyBinding::new("ctrl-z", Undo, e),
        KeyBinding::new("ctrl-shift-z", Redo, e),
        KeyBinding::new("ctrl-y", Redo, e),
        KeyBinding::new("ctrl-c", Copy, e),
        KeyBinding::new("ctrl-x", Cut, e),
        KeyBinding::new("ctrl-v", Paste, e),
        // Markdown 格式
        KeyBinding::new("ctrl-b", ToggleBold, e),
        KeyBinding::new("ctrl-i", ToggleItalic, e),
        KeyBinding::new("ctrl-`", ToggleCode, e),
        // `Ctrl+`` 与 `Ctrl+,` 同病：平台层吞掉，且事件侧会把 shift 折叠进字符
        // （到达形状 key="~" mods=ctrl），所以按字符绑 `ctrl-~`
        KeyBinding::new("ctrl-~", ToggleCode, e),
        KeyBinding::new("ctrl-enter", ToggleTask, e),
        // AI 续写：Tab 采纳会和缩进冲突，靠 EditorView 内部判断有无 ghost
        KeyBinding::new("alt-tab", AcceptGhost, e),
        KeyBinding::new("escape", DismissGhost, e),
        // ---- 全局命令 ----
        // Esc 关浮层：**全局**绑一条，不挂在具体浮层的上下文上。
        // 早期版本按浮层上下文（"Help" / "GraphView" / "BoardView" …）分别绑定，
        // 但实测这些上下文在派发时匹配不到，Esc 在浮层里是死的（点空白处能关，Esc 不能）。
        // 全局绑定不依赖上下文，实测有效；编辑器里的 escape 由更深的 Editor 上下文接住
        //（忽略 AI 续写），互不影响。
        KeyBinding::new("escape", Close, None),
        KeyBinding::new("ctrl-s", Save, None),
        KeyBinding::new("ctrl-shift-s", SaveAs, None),
        KeyBinding::new("ctrl-o", OpenFile, None),
        KeyBinding::new("ctrl-shift-o", OpenVault, None),
        KeyBinding::new("ctrl-n", NewNote, None),
        KeyBinding::new("ctrl-\\", ToggleSidebar, None),
        KeyBinding::new("ctrl-shift-m", ToggleChrome, None),
        KeyBinding::new("ctrl-shift-l", ToggleTheme, None),
        KeyBinding::new("ctrl-shift-e", FocusEditor, None),
        // 知识库：库内搜索 / 切换右侧面板
        KeyBinding::new("ctrl-shift-f", OpenSearch, None),
        KeyBinding::new("ctrl-shift-r", ToggleRightPanel, None),
        KeyBinding::new("ctrl-shift-g", OpenGraph, None),
        KeyBinding::new("ctrl-d", OpenDaily, None),
        KeyBinding::new("ctrl-shift-q", OpenChat, None),
        // 白板与历史版本
        KeyBinding::new("ctrl-shift-b", OpenBoard, None),
        KeyBinding::new("ctrl-shift-h", OpenHistory, None),
        // AI 入口
        // 关于 `Ctrl+,` / `Ctrl+`` 这两个键（踩了很久的坑，结论如下）：
        // 1) 本机 `Ctrl+,` 和 `Ctrl+`` 的 WM_KEYDOWN 在**平台层就被吞掉**了
        //    （gpui 的 parse_normal_key 返回 None，按键根本到不了 app），任何绑定都救不回来；
        // 2) 能送达的是 `Ctrl+Shift+,` / `Ctrl+Shift+``，但事件侧会把 shift **折叠进字符并清掉
        //    shift 修饰**（get_keystroke_key 走 get_shifted_key），到达形状分别是
        //    key="<" mods=ctrl、key="~" mods=ctrl；而绑定侧不做同样的折叠。
        // 所以必须**按字符**绑：`ctrl-<`，而不是 `ctrl-shift-,`。
        // 旧的 `ctrl-,` 保留着，留给没有这个平台问题的机器/键盘布局。
        KeyBinding::new("ctrl-,", OpenSettings, None),
        KeyBinding::new("ctrl-<", OpenSettings, None),
        // 中文输入法下 Ctrl+, 会发全角逗号，单独再绑一个全角版本，保证中英文都能开设置
        KeyBinding::new("ctrl-，", OpenSettings, None),
        KeyBinding::new("ctrl-k", OpenCommandPalette, None),
        // 快捷键帮助：`Ctrl+?` 在 Windows 上实为 `Ctrl+Shift+/`，按键字符带 shift，
        // 两种写法都绑上以兼容 gpui 的匹配行为
        KeyBinding::new("ctrl-?", OpenHelp, None),
        KeyBinding::new("ctrl-shift-?", OpenHelp, None),
        // 命令面板 / 输入框（自建，gpui 0.2.2 无内置文本输入组件）
        KeyBinding::new("enter", Submit, Some("CmdInput")),
        KeyBinding::new("backspace", command_palette::Backspace, Some("CmdInput")),
        KeyBinding::new("delete", command_palette::Delete, Some("CmdInput")),
        KeyBinding::new("left", command_palette::Left, Some("CmdInput")),
        KeyBinding::new("right", command_palette::Right, Some("CmdInput")),
        KeyBinding::new("home", command_palette::Home, Some("CmdInput")),
        KeyBinding::new("end", command_palette::End, Some("CmdInput")),
        KeyBinding::new("up", PaletteUp, Some("CmdInput")),
        KeyBinding::new("down", PaletteDown, Some("CmdInput")),
    ]
}
