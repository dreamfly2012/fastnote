//! 编辑区视图：即时渲染（WYSIWYG）+ 键鼠输入 + IME。
//!
//! 渲染策略是「块级即时渲染」：
//! - 光标所在块回退为**源码**，可直接编辑 Markdown 标记；
//! - 其余块显示**渲染结果**，标记符隐藏。
//!
//! 关键约束：块进出活动态时**几何位置不变**，只有标记符的可见性变化，
//! 否则光标移动会带来整页跳动。为此活动/非活动共用同一套字号与缩进。
//!
//! 布局只处理视口内的行（惰性视口解析），因此打开百兆文件与打开小文件的
//! 每帧开销一致，都只与窗口高度成正比。

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fastnote_ai::{AiConfig, AiEvent, Stream, Task};
use fastnote_core::block::{Block, BlockKind};
use fastnote_core::document::Document;
use fastnote_core::editor::Editor;
use fastnote_core::index::{SearchHit, VaultIndex, parse_query};
use gpui::{
    App, AsyncApp, Bounds, ClipboardItem, Context, ContentMask, Corners, CursorStyle, Element,
    ElementId, ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable,
    GlobalElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, Point, Render, RenderImage, ScrollDelta, ScrollWheelEvent,
    ShapedLine,
    SharedString, Style, TextAlign, UTF16Selection, Window, WrappedLine, actions, div, fill, point,
    prelude::*, px, size,
};

use image::{Frame, imageops};
use smallvec::smallvec;

use crate::render::{BlockStyle, block_style, body_font, inline_runs, source_runs};
use crate::theme::{Metrics, Theme};
use crate::{OpenNoteAt, OpenWikilink};

actions!(
    fastnote_editor,
    [
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        LineStart,
        LineEnd,
        SelectLineStart,
        SelectLineEnd,
        DocStart,
        DocEnd,
        PageUp,
        PageDown,
        Backspace,
        DeleteForward,
        Newline,
        Indent,
        Outdent,
        SelectAll,
        Undo,
        Redo,
        Copy,
        Cut,
        Paste,
        ToggleBold,
        ToggleItalic,
        ToggleCode,
        ToggleTask,
        AcceptGhost,
        DismissGhost,
    ]
);

/// 一行可见文本的布局结果。鼠标命中测试与光标定位都依赖它。
#[derive(Clone)]
struct VisualLine {
    /// 文本左上角（屏幕绝对坐标）
    origin: Point<Pixels>,
    line_height: Pixels,
    /// 软换行宽度，画选区时需要知道行尾在哪
    width: Pixels,
    shaped: WrappedLine,
    /// 该行覆盖的文档字节区间（不含行尾换行符）
    doc: Range<usize>,
    map: LineMap,
    /// 查询块结果行携带的跳转目标（笔记路径, 行号）。普通行为 `None`。
    link: Option<(std::path::PathBuf, usize)>,
}

/// 行内文本 ↔ 文档字节的换算方式。
#[derive(Clone)]
enum LineMap {
    /// 源码模式：显示的文本就是文档原文，偏移一一对应
    Source,
    /// 渲染模式：显示的是剥掉标记后的文本，需要经块的映射表换算
    Render {
        block: Rc<Block>,
        /// 该行文本在 `block.inline.text` 中的起点
        r_start: usize,
    },
}

impl VisualLine {
    /// 行内偏移 → 文档字节。
    fn doc_offset(&self, local: usize, doc: &Document) -> usize {
        match &self.map {
            LineMap::Source => self.doc.start + local.min(self.doc.len()),
            LineMap::Render { block, r_start } => {
                let (k, col) = block.inline_to_line_col(r_start + local);
                let line = (block.lines.start + k).min(block.lines.end.saturating_sub(1));
                let base = doc.line_to_byte(line);
                let max = base + doc.line_text(line).len();
                (base + col).min(max)
            }
        }
    }

    /// 文档字节 → 行内偏移。落在本行之外时返回 `None`。
    fn local_offset(&self, byte: usize, doc: &Document) -> Option<usize> {
        if byte < self.doc.start || byte > self.doc.end {
            return None;
        }
        match &self.map {
            LineMap::Source => Some(byte - self.doc.start),
            LineMap::Render { block, r_start } => {
                let (line, col) = doc.byte_to_line_col(byte);
                let k = line.checked_sub(block.lines.start)?;
                let r = block.line_col_to_inline(k, col);
                Some(r.saturating_sub(*r_start))
            }
        }
    }

    /// 该行占几个视觉行（软换行后的行数）。
    fn rows(&self) -> usize {
        self.shaped.wrap_boundaries().len() + 1
    }

    fn height(&self) -> Pixels {
        self.line_height * (self.rows() as f32)
    }
}

pub struct EditorView {
    pub editor: Editor,
    pub theme: Theme,
    focus: FocusHandle,

    /// 视口第一可见文档行。滚动以行为单位记录，
    /// 这样滚动开销与文档大小无关（不需要知道全文总高度）。
    scroll_line: usize,
    /// 首行内的像素偏移，用于平滑滚动
    scroll_px: Pixels,
    /// 上一帧的行布局，供鼠标命中测试复用
    lines: Vec<VisualLine>,
    /// 上一帧视口能显示的行数，用于 PageUp/PageDown 与光标跟随
    visible_rows: usize,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    /// IME 预编辑区间（文档字节）
    marked: Option<Range<usize>>,

    /// AI 行内续写的幽灵文本，Tab 采纳
    pub ghost: Option<SharedString>,
    /// AI 是否正在流式写入
    pub streaming: bool,

    /// AI 端点配置
    ai_cfg: AiConfig,
    /// 行内续写流（ghost text 来源）
    completion: Option<Stream>,

    /// 笔记库索引（查询块结果来源）。由 Workspace 在索引重建后写入。
    index: Option<VaultIndex>,
    /// 查询块结果缓存：查询正文 → 结果，避免每帧重跑查询。
    query_cache: HashMap<String, Vec<SearchHit>>,
    /// 续写累积文本
    completion_acc: String,
    /// 替换型 AI 动作的流
    ai_insert: Option<Stream>,
    /// 取消令牌：置位即中止当前流
    ai_token: Option<Arc<AtomicBool>>,

    /// 已解码的图片缓存：路径 → RenderImage。
    /// 渲染层在视口内按需解码并缓存，滚动回来时直接复用，避免重复读盘与 GPU 上传。
    image_cache: HashMap<PathBuf, Arc<RenderImage>>,
}

impl EditorView {
    pub fn new(editor: Editor, theme: Theme, ai_cfg: AiConfig, _cx: &mut Context<Self>) -> Self {
        Self {
            editor,
            theme,
            focus: _cx.focus_handle(),
            scroll_line: 0,
            scroll_px: px(0.),
            lines: Vec::new(),
            visible_rows: 30,
            last_bounds: None,
            is_selecting: false,
            marked: None,
            ghost: None,
            streaming: false,
            ai_cfg,
            completion: None,
            completion_acc: String::new(),
            ai_insert: None,
            ai_token: None,
            index: None,
            query_cache: HashMap::new(),
            image_cache: HashMap::new(),
        }
    }

    pub fn open(&mut self, doc: Document) {
        self.cancel_ai();
        self.editor = Editor::new(doc);
        self.scroll_line = 0;
        self.scroll_px = px(0.);
        self.lines.clear();
        self.ghost = None;
    }

    /// 设置变更后由 Workspace 写入新的 AI 端点配置。
    pub fn set_ai_config(&mut self, cfg: AiConfig) {
        self.ai_cfg = cfg;
    }

    /// 笔记库索引更新后由 Workspace 调用。索引变了，查询块缓存必须失效。
    pub fn set_index(&mut self, index: Option<VaultIndex>) {
        self.index = index;
        self.query_cache.clear();
    }

    /// 查询块（` ```query `）的当前结果，带缓存。
    ///
    /// 布局每帧都会调用，直接跑查询会拖垮渲染，所以按查询正文做记忆化；
    /// 索引更新时缓存由 [`Self::set_index`] 清空。
    fn query_hits_for(&mut self, blk: &Block) -> Option<Vec<SearchHit>> {
        let body = blk.query_body()?;
        let index = self.index.as_ref()?;
        if let Some(cached) = self.query_cache.get(body) {
            return Some(cached.clone());
        }
        let hits = index.query(&parse_query(body));
        self.query_cache.insert(body.to_string(), hits.clone());
        Some(hits)
    }

    // ---------- 滚动 ----------

    fn scroll_by_lines(&mut self, delta: f32) {
        if delta < 0. {
            self.scroll_line = self.scroll_line.saturating_add((-delta) as usize);
            let max = self.editor.doc.len_lines().saturating_sub(1);
            self.scroll_line = self.scroll_line.min(max);
        } else {
            self.scroll_line = self.scroll_line.saturating_sub(delta as usize);
        }
        self.scroll_px = px(0.);
    }

    /// 让光标保持在视口内。只用行号计算，不需要全文高度。
    fn reveal_cursor(&mut self) {
        let line = self.editor.doc.byte_to_line(self.editor.cursor());
        // 上下各留 2 行余量，避免光标贴边
        let pad = 2usize;
        if line < self.scroll_line + pad {
            self.scroll_line = line.saturating_sub(pad);
            self.scroll_px = px(0.);
        } else {
            let bottom = self.scroll_line + self.visible_rows.saturating_sub(pad + 1);
            if line > bottom {
                self.scroll_line = line + pad + 1 - self.visible_rows.max(pad + 2);
                self.scroll_px = px(0.);
            }
        }
    }

    /// 编辑或移动后的统一收尾：清幽灵文本、跟随光标、请求重绘。
    fn after_edit(&mut self, cx: &mut Context<Self>) {
        self.cancel_ai();
        self.reveal_cursor();
        cx.notify();
        self.maybe_request_completion(cx);
    }

    fn after_move(&mut self, cx: &mut Context<Self>) {
        self.cancel_ai();
        self.reveal_cursor();
        cx.notify();
        self.maybe_request_completion(cx);
    }

    // ---------- 命中测试 ----------

    fn offset_for_position(&self, pos: Point<Pixels>) -> usize {
        if self.lines.is_empty() {
            return self.editor.cursor();
        }
        // 先按 y 找行：落在行的垂直区间内即命中
        let mut best: Option<&VisualLine> = None;
        for vl in &self.lines {
            let top = vl.origin.y;
            let bottom = px(f32::from(top) + f32::from(vl.height()));
            if pos.y >= top && pos.y < bottom {
                best = Some(vl);
                break;
            }
        }
        // 没命中就取最近的一行（点在段落间距或上下留白里）
        let vl = match best {
            Some(v) => v,
            None => {
                let mut min = f32::MAX;
                let mut pick = &self.lines[0];
                for vl in &self.lines {
                    let center = f32::from(vl.origin.y) + f32::from(vl.height()) / 2.;
                    let d = (center - f32::from(pos.y)).abs();
                    if d < min {
                        min = d;
                        pick = vl;
                    }
                }
                pick
            }
        };

        let local = point(pos.x - vl.origin.x, pos.y - vl.origin.y);
        let idx = match vl
            .shaped
            .closest_index_for_position(local, vl.line_height)
        {
            Ok(i) => i,
            Err(i) => i,
        };
        vl.doc_offset(idx, &self.editor.doc)
    }

    /// 取纵向最近命中的可见行（与 `offset_for_position` 同逻辑，但只返回行本身）。
    fn line_at(&self, pos: Point<Pixels>) -> Option<&VisualLine> {
        let mut best: Option<&VisualLine> = None;
        for vl in &self.lines {
            let top = vl.origin.y;
            let bottom = px(f32::from(top) + f32::from(vl.height()));
            if pos.y >= top && pos.y < bottom {
                best = Some(vl);
                break;
            }
        }
        match best {
            Some(v) => Some(v),
            None => {
                let mut min = f32::MAX;
                let mut pick = &self.lines[0];
                for vl in &self.lines {
                    let center = f32::from(vl.origin.y) + f32::from(vl.height()) / 2.;
                    let d = (center - f32::from(pos.y)).abs();
                    if d < min {
                        min = d;
                        pick = vl;
                    }
                }
                Some(pick)
            }
        }
    }

    /// 命中测试：若点在一个渲染态块内的双向链接上，返回目标笔记名。
    ///
    /// 活动（源码态）块内的链接按普通文本处理，因此这里只在 `Render` 映射下查找。
    fn wikilink_at(&self, pos: Point<Pixels>) -> Option<String> {
        let vl = self.line_at(pos)?;
        let local = point(pos.x - vl.origin.x, pos.y - vl.origin.y);
        let idx = match vl
            .shaped
            .closest_index_for_position(local, vl.line_height)
        {
            Ok(i) => i,
            Err(i) => i,
        };
        if let LineMap::Render { block, r_start } = &vl.map {
            let rendered = *r_start + idx;
            if let Some(inline) = &block.inline {
                for (range, target) in &inline.wikilinks {
                    if range.contains(&rendered) {
                        return Some(target.clone());
                    }
                }
            }
        }
        None
    }

    /// 当前文档的标题大纲：`(级别, 标题文本, 行号)`。
    pub fn outline(&self) -> Vec<(u8, String, usize)> {
        let doc = &self.editor.doc;
        let mut out = Vec::new();
        for line in 0..doc.len_lines() {
            let text = doc.line_text(line);
            let t = text.trim_start();
            let n = t.bytes().take_while(|b| *b == b'#').count();
            if n >= 1 && n <= 6 {
                let rest = &t[n..];
                if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
                    out.push((n as u8, rest.trim().to_string(), line));
                }
            }
        }
        out
    }

    /// 跳转到指定文档行并把光标放到行首。
    pub fn goto_line(&mut self, line: usize, cx: &mut Context<Self>) {
        let line = line.min(self.editor.doc.len_lines().saturating_sub(1));
        let byte = self.editor.doc.line_to_byte(line);
        self.editor.set_cursor(byte);
        self.scroll_line = line.saturating_sub(2);
        self.scroll_px = px(0.);
        cx.notify();
    }

    // ---------- 动作 ----------

    fn move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_left(false);
        self.after_move(cx);
    }
    fn move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_right(false);
        self.after_move(cx);
    }
    fn move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_up(false);
        self.after_move(cx);
    }
    fn move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_down(false);
        self.after_move(cx);
    }
    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_left(true);
        self.after_move(cx);
    }
    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_right(true);
        self.after_move(cx);
    }
    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_up(true);
        self.after_move(cx);
    }
    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_down(true);
        self.after_move(cx);
    }
    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_word_left(false);
        self.after_move(cx);
    }
    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_word_right(false);
        self.after_move(cx);
    }
    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_word_left(true);
        self.after_move(cx);
    }
    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_word_right(true);
        self.after_move(cx);
    }
    fn line_start(&mut self, _: &LineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_line_start(false);
        self.after_move(cx);
    }
    fn line_end(&mut self, _: &LineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_line_end(false);
        self.after_move(cx);
    }
    fn select_line_start(&mut self, _: &SelectLineStart, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_line_start(true);
        self.after_move(cx);
    }
    fn select_line_end(&mut self, _: &SelectLineEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_line_end(true);
        self.after_move(cx);
    }
    fn doc_start(&mut self, _: &DocStart, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_doc_start(false);
        self.after_move(cx);
    }
    fn doc_end(&mut self, _: &DocEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.move_doc_end(false);
        self.after_move(cx);
    }
    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.visible_rows.saturating_sub(2).max(1);
        for _ in 0..n {
            self.editor.move_up(false);
        }
        self.after_move(cx);
    }
    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        let n = self.visible_rows.saturating_sub(2).max(1);
        for _ in 0..n {
            self.editor.move_down(false);
        }
        self.after_move(cx);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.backspace();
        self.after_edit(cx);
    }
    fn delete_forward(&mut self, _: &DeleteForward, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.delete_forward();
        self.after_edit(cx);
    }
    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.newline();
        self.after_edit(cx);
    }
    fn indent(&mut self, _: &Indent, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.indent();
        self.after_edit(cx);
    }
    fn outdent(&mut self, _: &Outdent, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.outdent();
        self.after_edit(cx);
    }
    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.select_all();
        cx.notify();
    }
    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.editor.undo() {
            self.after_edit(cx);
        }
    }
    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if self.editor.redo() {
            self.after_edit(cx);
        }
    }
    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let t = self.editor.selected_text();
        if !t.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(t));
        }
    }
    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        let t = self.editor.selected_text();
        if !t.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(t));
            self.editor.replace_selection("");
            self.after_edit(cx);
        }
    }
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = cx.read_from_clipboard().and_then(|i| i.text()) {
            // 粘贴统一换行，避免把 CRLF 带进 Rope 造成行尾混杂
            let t = t.replace("\r\n", "\n").replace('\r', "\n");
            self.editor.break_undo_group();
            self.editor.insert(&t);
            self.editor.break_undo_group();
            self.after_edit(cx);
        }
    }
    fn toggle_bold(&mut self, _: &ToggleBold, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.toggle_wrap("**");
        self.after_edit(cx);
    }
    fn toggle_italic(&mut self, _: &ToggleItalic, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.toggle_wrap("*");
        self.after_edit(cx);
    }
    fn toggle_code(&mut self, _: &ToggleCode, _: &mut Window, cx: &mut Context<Self>) {
        self.editor.toggle_wrap("`");
        self.after_edit(cx);
    }
    fn toggle_task(&mut self, _: &ToggleTask, _: &mut Window, cx: &mut Context<Self>) {
        if self.editor.toggle_task() {
            self.after_edit(cx);
        }
    }

    fn accept_ghost(&mut self, _: &AcceptGhost, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.completion_acc.clone();
        if !text.is_empty() {
            self.completion = None;
            self.ghost = None;
            self.completion_acc.clear();
            self.ai_token = None;
            self.editor.break_undo_group();
            self.editor.insert(&text);
            self.editor.break_undo_group();
            self.reveal_cursor();
            cx.notify();
        }
    }
    fn dismiss_ghost(&mut self, _: &DismissGhost, _: &mut Window, cx: &mut Context<Self>) {
        if self.ghost.take().is_some() || self.completion.is_some() {
            self.cancel_ai();
            cx.notify();
        }
    }

    /// 供外部（AI 流式回调）调用：追加一段流式文本。
    pub fn stream_delta(&mut self, delta: &str, cx: &mut Context<Self>) {
        self.streaming = true;
        self.editor.stream_insert(delta);
        self.reveal_cursor();
        cx.notify();
    }

    /// 取消一切进行中的 AI 流（续写/插入），并清幽灵文本。
    fn cancel_ai(&mut self) {
        self.ghost = None;
        self.completion = None;
        self.ai_insert = None;
        self.completion_acc.clear();
        self.streaming = false;
        if let Some(t) = &self.ai_token {
            t.store(true, Ordering::Relaxed);
        }
        self.ai_token = None;
    }

    /// 光标停顿后尝试行内续写：取光标上下文，debounce 600ms，发起 Complete。
    fn maybe_request_completion(&mut self, cx: &mut Context<Self>) {
        self.cancel_ai();
        if !self.ai_cfg.is_configured() {
            return;
        }
        let cur = self.editor.cursor();
        let doc = &self.editor.doc;
        if doc.len_bytes() == 0 {
            return;
        }
        let line = doc.byte_to_line(cur);
        let line_text = doc.line_text(line);
        let col = cur.saturating_sub(doc.line_to_byte(line));
        let after = &line_text[col.min(line_text.len())..];
        // 只在行尾续写，避免打断正在输入的行
        if !after.trim().is_empty() {
            return;
        }
        // 代码块内不续写
        let head = line_text.trim_start();
        if head.starts_with("```") || head.starts_with("~~~") {
            return;
        }
        let token = Arc::new(AtomicBool::new(false));
        self.ai_token = Some(token.clone());
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor()
                .timer(Duration::from_millis(600))
                .await;
            if token.load(Ordering::Relaxed) {
                return;
            }
            let ctx = this
                .update(cx, |v, _| {
                    let cur = v.editor.cursor();
                    let end = v.editor.doc.len_bytes();
                    let text = v.editor.doc.slice_to_string(0..end);
                    fastnote_ai::prompt::completion_context(&text, cur)
                })
                .unwrap_or_default();
            this.update(cx, |v, _| {
                v.completion = Some(fastnote_ai::run_task(&v.ai_cfg, &Task::Complete, &ctx));
            })
            .ok();
            loop {
                let events = this
                    .update(cx, |v, _| {
                        v.completion.as_mut().map(|s| s.drain()).unwrap_or_default()
                    })
                    .unwrap_or_default();
                let mut done = false;
                let mut acc = this
                    .update(cx, |v, _| v.completion_acc.clone())
                    .unwrap_or_default();
                for ev in events {
                    match ev {
                        AiEvent::Delta(d) => acc.push_str(&d),
                        AiEvent::Done => done = true,
                        AiEvent::Error(_) => {
                            this.update(cx, |v, cx| {
                                v.ghost = None;
                                v.streaming = false;
                                v.completion = None;
                                cx.notify();
                            })
                            .ok();
                            return;
                        }
                    }
                }
                if !acc.is_empty() {
                    this.update(cx, |v, cx| {
                        v.completion_acc = acc.clone();
                        v.ghost = Some(acc.into());
                        v.streaming = false;
                        cx.notify();
                    })
                    .ok();
                }
                if done {
                    this.update(cx, |v, _| v.completion = None).ok();
                    return;
                }
                if token.load(Ordering::Relaxed) {
                    this.update(cx, |v, _| {
                        v.completion = None;
                        v.ghost = None;
                    })
                    .ok();
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
            }
        })
        .detach();
    }

    /// 对选中文本（或光标所在行）执行替换型 AI 动作，结果流式写回。
    pub fn run_insert_task(&mut self, task: Task, cx: &mut Context<Self>) {
        if !self.ai_cfg.is_configured() {
            return;
        }
        let has_sel = self.editor.has_selection();
        let doc = &self.editor.doc;
        let cur = self.editor.cursor();
        let (target, original) = if has_sel {
            (self.editor.selection(), self.editor.selected_text())
        } else {
            let line = doc.byte_to_line(cur);
            let start = doc.line_to_byte(line);
            let text = doc.line_text(line);
            (start..start + text.len(), text.to_string())
        };

        self.cancel_ai();
        // 选中目标范围并清空，光标落到起点；后续流式插入即"替换"
        self.editor.select(target);
        self.editor.replace_selection("");

        let cfg = self.ai_cfg.clone();
        let token = Arc::new(AtomicBool::new(false));
        self.ai_token = Some(token.clone());
        self.ai_insert = Some(fastnote_ai::run_task(&cfg, &task, &original));

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            loop {
                let events = this
                    .update(cx, |v, _| {
                        v.ai_insert.as_mut().map(|s| s.drain()).unwrap_or_default()
                    })
                    .unwrap_or_default();
                let mut done = false;
                for ev in events {
                    match ev {
                        AiEvent::Delta(d) => {
                            this.update(cx, |v, cx| v.stream_delta(&d, cx)).ok();
                        }
                        AiEvent::Done => done = true,
                        AiEvent::Error(_) => {
                            this.update(cx, |v, cx| {
                                v.streaming = false;
                                v.ai_insert = None;
                                cx.notify();
                            })
                            .ok();
                            return;
                        }
                    }
                }
                if done {
                    this.update(cx, |v, cx| {
                        v.editor.finish_stream();
                        v.streaming = false;
                        v.ai_insert = None;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                if token.load(Ordering::Relaxed) {
                    this.update(cx, |v, cx| {
                        v.editor.finish_stream();
                        v.streaming = false;
                        v.ai_insert = None;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
            }
        })
        .detach();
    }

    // ---------- 鼠标 ----------

    fn on_mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        // 渲染态块里点击双向链接：导航到目标笔记，不移动光标。
        // 活动（源码态）块内的链接仍按普通点击处理，避免打断编辑。
        if !e.modifiers.shift && e.click_count < 2 {
            if let Some(target) = self.wikilink_at(e.position) {
                // 动作内部自带 defer，不会在鼠标回调里重入 Workspace
                window.dispatch_action(Box::new(OpenWikilink(target)), cx);
                return;
            }
            // 查询块结果行：跳转到命中的笔记与行
            if let Some((path, line)) = self.line_at(e.position).and_then(|vl| vl.link.clone()) {
                window.dispatch_action(Box::new(OpenNoteAt(path, line)), cx);
                return;
            }
        }
        let off = self.offset_for_position(e.position);
        if e.modifiers.shift {
            let anchor = self.editor.selection();
            let a = if off < anchor.start {
                anchor.end
            } else {
                anchor.start
            };
            self.editor.select(a.min(off)..a.max(off));
        } else if e.click_count >= 2 {
            // 双击选词：借用 core 的词边界移动
            self.editor.set_cursor(off);
            self.editor.move_word_left(false);
            self.editor.move_word_right(true);
        } else {
            self.editor.set_cursor(off);
            self.is_selecting = true;
        }
        self.after_move(cx);
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.is_selecting {
            return;
        }
        let off = self.offset_for_position(e.position);
        let cur = self.editor.selection();
        let anchor = if self.editor.cursor() == cur.start {
            cur.end
        } else {
            cur.start
        };
        self.editor.select(anchor.min(off)..anchor.max(off));
        cx.notify();
    }

    fn on_scroll(&mut self, e: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let lh = px(f32::from(Metrics::BODY) * Metrics::LINE_HEIGHT);
        let dy = match e.delta {
            ScrollDelta::Pixels(p) => f32::from(p.y) / f32::from(lh),
            ScrollDelta::Lines(l) => l.y,
        };
        if dy != 0. {
            self.scroll_by_lines(dy * 3.);
            cx.notify();
        }
        let _ = window;
    }

    // ---------- UTF-16 换算（IME / 系统输入法要求） ----------

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

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

// 打开双向链接 / 查询块结果都通过窗口动作派发（会被 Workspace 的
// `.on_action` 接住），这里不再需要事件订阅。
impl EntityInputHandler for EditorView {
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
        self.after_edit(cx);
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
            // 输入法给的是相对预编辑串的 UTF-16 偏移
            let s = start + sel.start.min(new_text.len());
            let e = start + sel.end.min(new_text.len());
            self.editor.select(s..e);
        }
        self.after_edit(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // 输入法候选窗定位：返回预编辑起点所在行的一小段矩形
        let r = self.range_from_utf16(&range_utf16);
        let doc = &self.editor.doc;
        for vl in &self.lines {
            if let Some(local) = vl.local_offset(r.start, doc) {
                let p = vl.shaped.position_for_index(local, vl.line_height)?;
                let end = vl
                    .local_offset(r.end, doc)
                    .and_then(|l| vl.shaped.position_for_index(l, vl.line_height))
                    .unwrap_or(p);
                let ex = end.x.max(px(f32::from(p.x) + 2.));
                return Some(Bounds::from_corners(
                    point(px(f32::from(vl.origin.x) + f32::from(p.x)), px(f32::from(vl.origin.y) + f32::from(p.y))),
                    point(
                        px(f32::from(vl.origin.x) + f32::from(ex)),
                        px(f32::from(vl.origin.y) + f32::from(p.y) + f32::from(vl.line_height)),
                    ),
                ));
            }
        }
        None
    }

    fn character_index_for_point(
        &mut self,
        p: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let off = self.offset_for_position(p);
        Some(self.byte_to_utf16(off))
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(self.theme.bg)
            .key_context("Editor")
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::move_left))
            .on_action(cx.listener(Self::move_right))
            .on_action(cx.listener(Self::move_up))
            .on_action(cx.listener(Self::move_down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::line_start))
            .on_action(cx.listener(Self::line_end))
            .on_action(cx.listener(Self::select_line_start))
            .on_action(cx.listener(Self::select_line_end))
            .on_action(cx.listener(Self::doc_start))
            .on_action(cx.listener(Self::doc_end))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete_forward))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::indent))
            .on_action(cx.listener(Self::outdent))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::toggle_bold))
            .on_action(cx.listener(Self::toggle_italic))
            .on_action(cx.listener(Self::toggle_code))
            .on_action(cx.listener(Self::toggle_task))
            .on_action(cx.listener(Self::accept_ghost))
            .on_action(cx.listener(Self::dismiss_ghost))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(EditorElement {
                view: cx.entity(),
            })
    }
}

// ============================ 自定义绘制元素 ============================

/// 编辑区绘制元素。
///
/// 走自定义 `Element` 而不是拼 `div` + `StyledText`，原因是 WYSIWYG 需要
/// 精确的「像素 ↔ 字节」双向映射（光标定位、选区、鼠标点击），
/// 这只能拿到 `WrappedLine` 的整形结果后自己算。
struct EditorElement {
    view: Entity<EditorView>,
}

struct Prepaint {
    lines: Vec<VisualLine>,
    /// 代码块底色、引用竖条、分割线，先于文本绘制
    decorations: Vec<PaintQuad>,
    selections: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
    /// 列表符号 / 任务框，不参与命中测试
    markers: Vec<(Point<Pixels>, ShapedLine)>,
    ghost: Option<(Point<Pixels>, ShapedLine)>,
    /// 图片精灵：位图直接绘制（不走文本整形），与文本不重叠，独立成块。
    image_sprites: Vec<(Bounds<Pixels>, Arc<RenderImage>)>,
}

impl IntoElement for EditorElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = gpui::relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        self.view
            .update(cx, |view, _| layout_viewport(view, bounds, window))
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        pre: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.view.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );

        let focused = focus.is_focused(window);
        let mask = ContentMask { bounds };
        window.with_content_mask(Some(mask), |window| {
            for q in pre.decorations.drain(..) {
                window.paint_quad(q);
            }
            for (bounds, img) in pre.image_sprites.drain(..) {
                let _ = window.paint_image(bounds, Corners::all(px(0.)), img, 0, false);
            }
            for q in pre.selections.drain(..) {
                window.paint_quad(q);
            }
            for (origin, line) in pre.markers.drain(..) {
                let _ = line.paint(origin, window.line_height(), window, cx);
            }
            for vl in &pre.lines {
                let _ = vl.shaped.paint(
                    vl.origin,
                    vl.line_height,
                    TextAlign::default(),
                    None,
                    window,
                    cx,
                );
            }
            if let Some((origin, line)) = pre.ghost.take() {
                let _ = line.paint(origin, window.line_height(), window, cx);
            }
            if focused {
                if let Some(c) = pre.cursor.take() {
                    window.paint_quad(c);
                }
            }
        });

        // 回写布局供鼠标命中测试使用
        let lines = std::mem::take(&mut pre.lines);
        self.view.update(cx, |view, _| {
            view.lines = lines;
            view.last_bounds = Some(bounds);
        });
    }
}

/// 构建视口内的全部行布局与装饰。
///
/// 只处理视口覆盖的行，因此耗时与文档大小无关。
fn layout_viewport(view: &mut EditorView, bounds: Bounds<Pixels>, window: &mut Window) -> Prepaint {
    let theme = view.theme;
    let mut out = Prepaint {
        lines: Vec::with_capacity(64),
        decorations: Vec::new(),
        selections: Vec::new(),
        cursor: None,
        markers: Vec::new(),
        ghost: None,
        image_sprites: Vec::new(),
    };

    // 正文宽度上限，超宽窗口时居中，避免长行难以阅读
    let avail = (f32::from(bounds.size.width) - f32::from(Metrics::PAD_X) * 2.).max(120.);
    let content_w = avail.min(f32::from(Metrics::MAX_WIDTH));
    let content_x = f32::from(bounds.left()) + (f32::from(bounds.size.width) - content_w) / 2.;

    let base_lh = f32::from(Metrics::BODY) * Metrics::LINE_HEIGHT;
    let rows = ((f32::from(bounds.size.height) / (base_lh * 0.8)).ceil() as usize).max(4) + 4;
    view.visible_rows = (f32::from(bounds.size.height) / base_lh).floor().max(1.) as usize;

    let total_lines = view.editor.doc.len_lines().max(1);
    if view.scroll_line >= total_lines {
        view.scroll_line = total_lines - 1;
    }
    let want = view.scroll_line..(view.scroll_line + rows).min(total_lines);

    // 惰性解析：只取覆盖视口的块。克隆是为了解开对 doc 的可变借用，
    // 视口内块数是常量级（几十个），代价可忽略。
    let blocks: Vec<Rc<Block>> = view
        .editor
        .doc
        .blocks_for_lines(want)
        .iter()
        .cloned()
        .map(Rc::new)
        .collect();
    if blocks.is_empty() {
        return out;
    }

    let cursor = view.editor.cursor();
    let active_idx = blocks.iter().position(|b| b.contains_byte(cursor));

    let start_idx = blocks
        .iter()
        .position(|b| b.contains_line(view.scroll_line))
        .unwrap_or(0);

    // 首块可能只露出下半部分，按行高把它顶出视口
    let first_style = block_style(&blocks[start_idx].kind, &theme, active_idx == Some(start_idx));
    let skipped = view.scroll_line.saturating_sub(blocks[start_idx].lines.start);
    let mut y = f32::from(bounds.top())
        - f32::from(view.scroll_px)
        - skipped as f32 * f32::from(first_style.line_height)
        + if view.scroll_line == 0 {
            f32::from(Metrics::PAD_Y)
        } else {
            0.
        };

    let bottom_slack = f32::from(bounds.bottom()) + base_lh * 2.;

    for (i, blk) in blocks.iter().enumerate().skip(start_idx) {
        let active = active_idx == Some(i);
        // 图片块：光标不在块内时解码并直接绘制位图，光标进入后回退为源码以便编辑。
        // 文件不存在或格式不支持时返回 None，自动退回源码显示，内容不会丢。
        let image = match &blk.kind {
            BlockKind::Image { src } if !active => {
                let base = view
                    .editor
                    .doc
                    .path()
                    .and_then(|p| p.parent())
                    .map(|p| p.to_path_buf());
                image_for(view, src, base.as_deref())
            }
            _ => None,
        };
        let st = block_style(&blk.kind, &theme, active);
        if i > start_idx {
            y = y + f32::from(st.gap_above);
        }
        let text_x = content_x + f32::from(st.indent);
        let wrap_w = px((content_w - f32::from(st.indent)).max(40.));
        let block_top = y;

        // 查询块（```query）：光标不在块内时用笔记库索引实时渲染结果列表，
        // 每行可点击跳转到对应笔记。光标进入后回退为源码以便编辑条件。
        let query_hits = if !active {
            view.query_hits_for(blk)
        } else {
            None
        };

        if let Some(hits) = query_hits {
            let mut qs = st.clone();
            qs.font = body_font();
            qs.font_size = px((f32::from(Metrics::BODY) - 1.).round());
            qs.line_height = px((f32::from(qs.font_size) * 1.7).round());
            qs.prefix = None;
            // 结果行独占一块区域，不再叠加代码块底纹，避免和行文本重复
            let fence = view.editor.doc.line_to_byte(blk.lines.start);

            let head = if hits.is_empty() {
                "⌕ 查询无匹配".to_string()
            } else {
                format!("⌕ 查询 · {} 篇", hits.len())
            };
            let mut hs = qs.clone();
            hs.color = theme.muted;
            let runs = source_runs(&head, &hs, &theme);
            let shaped = shape(window, head.clone(), hs.font_size, &runs, wrap_w);
            let vl = VisualLine {
                origin: point(px(text_x), px(y)),
                line_height: hs.line_height,
                width: wrap_w,
                shaped,
                doc: fence..fence,
                map: LineMap::Source,
                link: None,
            };
            y = y + f32::from(vl.height());
            out.lines.push(vl);

            for h in hits {
                let text = format!("· {} — {}", h.name, h.snippet);
                let mut rs = qs.clone();
                rs.color = theme.wikilink;
                let runs = source_runs(&text, &rs, &theme);
                let shaped = shape(window, text.clone(), rs.font_size, &runs, wrap_w);
                let vl = VisualLine {
                    origin: point(px(text_x), px(y)),
                    line_height: rs.line_height,
                    width: wrap_w,
                    shaped,
                    doc: fence..fence,
                    map: LineMap::Source,
                    link: Some((h.path.clone(), h.line)),
                };
                y = y + f32::from(vl.height());
                out.lines.push(vl);
            }
        } else if st.is_rule && !active {
            // 分割线：画一条水平线，同时留一个空行用于点击定位
            let mid = y + f32::from(st.line_height) / 2.;
            out.decorations.push(fill(
                Bounds::new(point(px(content_x), px(mid)), size(px(content_w), px(1.))),
                theme.rule,
            ));
            let ls = view.editor.doc.line_to_byte(blk.lines.start);
            out.lines.push(VisualLine {
                origin: point(px(text_x), px(y)),
                line_height: st.line_height,
                width: wrap_w,
                shaped: WrappedLine::default(),
                doc: ls..ls + blk.source.len(),
                map: LineMap::Source,
                link: None,
            });
            y = y + f32::from(st.line_height);
        } else if let Some(img) = image {
            // 图片块：整块就是一张图，按内容宽度等比缩放后交给 paint 阶段绘制。
            // 同时压一个不可见行（空 WrappedLine，行高=图高）参与命中测试，
            // 点图片任意处都能把光标送进块内，回退为源码编辑。
            let sz = img.size(0);
            let iw = sz.width.0 as f32;
            let ih = sz.height.0 as f32;
            let max_w = (content_w - f32::from(st.indent)).max(40.);
            let scale = if iw > max_w && iw > 0. { max_w / iw } else { 1. };
            let disp_h = ih * scale;
            let ls = view.editor.doc.line_to_byte(blk.lines.start);
            out.lines.push(VisualLine {
                origin: point(px(text_x), px(y)),
                line_height: px(disp_h),
                width: wrap_w,
                shaped: WrappedLine::default(),
                doc: ls..ls + blk.source.len(),
                map: LineMap::Source,
                link: None,
            });
            out.image_sprites.push((
                Bounds::new(point(px(text_x), px(y)), size(px(iw * scale), px(disp_h))),
                img,
            ));
            y = y + disp_h;
        } else if active || blk.inline.is_none() {
            // 源码模式：逐文档行显示原文，Markdown 标记可见可编辑
            for ln in blk.lines.clone() {
                let text = view.editor.doc.line_text(ln);
                let start = view.editor.doc.line_to_byte(ln);
                let len = text.len();
                let runs = source_runs(&text, &st, &theme);
                let shaped = shape(window, text, st.font_size, &runs, wrap_w);
                let vl = VisualLine {
                    origin: point(px(text_x), px(y)),
                    line_height: st.line_height,
                    width: wrap_w,
                    shaped,
                    doc: start..start + len,
                    map: LineMap::Source,
                    link: None,
                };
                y = y + f32::from(vl.height());
                out.lines.push(vl);
            }
        } else {
            // 渲染模式：显示剥掉标记后的文本
            let inline = blk.inline.as_ref().unwrap();
            let mut r_at = 0usize;
            for (k, seg) in inline.text.split('\n').enumerate() {
                let r_start = r_at;
                r_at = r_start + seg.len() + 1;
                let runs = inline_runs(inline, r_start..r_start + seg.len(), &st, &theme);
                let shaped = shape(window, seg.to_string(), st.font_size, &runs, wrap_w);

                let dl = (blk.lines.start + k).min(blk.lines.end.saturating_sub(1));
                let dstart = view.editor.doc.line_to_byte(dl);
                let dlen = view.editor.doc.line_text(dl).len();

                if k == 0 {
                    if let Some(p) = &st.prefix {
                        let m = shape_marker(window, p, &st, &theme);
                        out.markers.push((
                            point(
                                px(text_x - 22.),
                                px(y + (f32::from(st.line_height) - f32::from(st.font_size) * 1.3) / 2.),
                            ),
                            m,
                        ));
                    }
                }

                let vl = VisualLine {
                    origin: point(px(text_x), px(y)),
                    line_height: st.line_height,
                    width: wrap_w,
                    shaped,
                    doc: dstart..dstart + dlen,
                    map: LineMap::Render {
                        block: blk.clone(),
                        r_start,
                    },
                    link: None,
                };
                y = y + f32::from(vl.height());
                out.lines.push(vl);
            }
        }

        // 块级装饰：底色与引用竖条要覆盖整块高度，所以放在块渲染完之后算
        if let Some(bg) = st.bg {
            out.decorations.push(fill(
                Bounds::from_corners(
                    point(px(content_x), px(block_top - 6.)),
                    point(px(content_x + f32::from(content_w)), px(y + 6.)),
                ),
                bg,
            ));
        }
        if let Some(bar) = st.bar {
            out.decorations.push(fill(
                Bounds::from_corners(
                    point(px(content_x + 2.), px(block_top)),
                    point(px(content_x + 5.), px(y)),
                ),
                bar,
            ));
        }

        y = y + f32::from(st.gap_below);
        if y > bottom_slack {
            break;
        }
    }

    // 装饰要画在文本下面，但底色是块渲染后才追加的，这里统一按插入顺序绘制即可
    build_selection(view, &mut out, &theme);
    build_cursor(view, &mut out, window, &theme);
    out
}

/// 图片最长边的上限，超过就降采样。
///
/// 照片动辄几千万像素，原样上传会占满显存并拖慢每帧布局；正文区实际只需要
/// 一两千像素宽，降采样后观感无损。
const IMAGE_MAX_EDGE: u32 = 1600;

/// 取回图片块对应的位图，带缓存。
///
/// 相对路径按**笔记所在目录**解析（与 Obsidian 一致），绝对路径原样使用。
/// 缓存键是解析后的绝对路径，所以滚动来回不会重复读盘与解码。
/// 返回 `None` 时调用方会退回源码显示，内容不会丢失。
fn image_for(view: &mut EditorView, src: &str, base: Option<&Path>) -> Option<Arc<RenderImage>> {
    let src = strip_image_title(src);
    if src.is_empty() || src.starts_with("http://") || src.starts_with("https://") {
        return None;
    }
    let path = if Path::new(src).is_absolute() {
        PathBuf::from(src)
    } else {
        base?.join(src)
    };
    if let Some(hit) = view.image_cache.get(&path) {
        return Some(hit.clone());
    }
    let img = decode_image(&path)?;
    view.image_cache.insert(path, img.clone());
    Some(img)
}

/// 去掉 Markdown 图片目标里的标题与 `<>` 包裹，只留路径本身。
fn strip_image_title(src: &str) -> &str {
    let s = src.trim();
    let s = s
        .strip_prefix('<')
        .and_then(|r| r.strip_suffix('>'))
        .unwrap_or(s);
    // 标题必然收尾于闭合符（`path "title"` / `path 'title'` / `path (title)`），
    // 只有确认这一点才切，否则原样返回，避免误伤 `img (1).png` 这类文件名。
    for (open, close) in [('"', '"'), ('\'', '\''), ('(', ')')] {
        if s.ends_with(close) {
            if let Some(p) = s[..s.len() - 1].rfind(open) {
                if p > 0 && s.as_bytes()[p - 1] == b' ' {
                    return s[..p].trim_end();
                }
            }
        }
    }
    s
}

/// 解码图片为 GPUI 可直接绘制的位图。
///
/// GPUI 的精灵图集按 **BGRA** 上传，而 `image` 解出的是 RGBA，
/// 所以必须交换 R/B 两个通道，否则红蓝对调。
fn decode_image(path: &Path) -> Option<Arc<RenderImage>> {
    let mut data = image::open(path).ok()?.into_rgba8();
    let (w, h) = data.dimensions();
    if w > IMAGE_MAX_EDGE || h > IMAGE_MAX_EDGE {
        let scale = IMAGE_MAX_EDGE as f32 / w.max(h) as f32;
        let nw = (w as f32 * scale).round().max(1.) as u32;
        let nh = (h as f32 * scale).round().max(1.) as u32;
        data = imageops::resize(&data, nw, nh, imageops::FilterType::Triangle);
    }
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(smallvec![Frame::new(data)])))
}

fn shape(
    window: &mut Window,
    text: String,
    font_size: Pixels,
    runs: &[gpui::TextRun],
    wrap: Pixels,
) -> WrappedLine {
    if text.is_empty() {
        return WrappedLine::default();
    }
    match window
        .text_system()
        .shape_text(text.into(), font_size, runs, Some(wrap), None)
    {
        Ok(mut v) if !v.is_empty() => v.remove(0),
        _ => WrappedLine::default(),
    }
}

fn shape_marker(
    window: &mut Window,
    text: &str,
    st: &BlockStyle,
    theme: &Theme,
) -> ShapedLine {
    let run = gpui::TextRun {
        len: text.len(),
        font: body_font(),
        color: theme.marker,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(text.to_string().into(), st.font_size, &[run], None)
}

fn build_selection(view: &EditorView, out: &mut Prepaint, theme: &Theme) {
    let sel = view.editor.selection();
    if sel.is_empty() {
        return;
    }
    let doc = &view.editor.doc;
    for vl in &out.lines {
        if sel.end < vl.doc.start || sel.start > vl.doc.end {
            continue;
        }
        let s = sel.start.max(vl.doc.start);
        let e = sel.end.min(vl.doc.end);
        let Some(a) = vl.local_offset(s, doc) else {
            continue;
        };
        let Some(b) = vl.local_offset(e, doc) else {
            continue;
        };
        let Some(pa) = vl.shaped.position_for_index(a, vl.line_height) else {
            continue;
        };
        let Some(pb) = vl.shaped.position_for_index(b, vl.line_height) else {
            continue;
        };
        // 选区跨过行尾换行符时补一小段，视觉上体现「整行被选中」
        let tail = if sel.end > vl.doc.end { 6. } else { 0. };

        if (f32::from(pa.y) - f32::from(pb.y)).abs() < 0.5 {
            out.selections.push(fill(
                Bounds::from_corners(
                    point(px(f32::from(vl.origin.x) + f32::from(pa.x)), px(f32::from(vl.origin.y) + f32::from(pa.y))),
                    point(
                        px(f32::from(vl.origin.x) + f32::from(pb.x) + tail),
                        px(f32::from(vl.origin.y) + f32::from(pa.y) + f32::from(vl.line_height)),
                    ),
                ),
                theme.selection,
            ));
        } else {
            // 起始视觉行：从起点到行尾
            out.selections.push(fill(
                Bounds::from_corners(
                    point(
                        px(f32::from(vl.origin.x) + f32::from(pa.x)),
                        px(f32::from(vl.origin.y) + f32::from(pa.y)),
                    ),
                    point(
                        px(f32::from(vl.origin.x) + f32::from(vl.width)),
                        px(f32::from(vl.origin.y) + f32::from(pa.y) + f32::from(vl.line_height)),
                    ),
                ),
                theme.selection,
            ));
            // 中间整行
            if f32::from(pb.y) - f32::from(pa.y) > f32::from(vl.line_height) {
                out.selections.push(fill(
                    Bounds::from_corners(
                        point(
                            vl.origin.x,
                            px(f32::from(vl.origin.y) + f32::from(pa.y) + f32::from(vl.line_height)),
                        ),
                        point(
                            px(f32::from(vl.origin.x) + f32::from(vl.width)),
                            px(f32::from(vl.origin.y) + f32::from(pb.y)),
                        ),
                    ),
                    theme.selection,
                ));
            }
            // 末视觉行
            out.selections.push(fill(
                Bounds::from_corners(
                    point(vl.origin.x, px(f32::from(vl.origin.y) + f32::from(pb.y))),
                    point(
                        px(f32::from(vl.origin.x) + f32::from(pb.x) + tail),
                        px(f32::from(vl.origin.y) + f32::from(pb.y) + f32::from(vl.line_height)),
                    ),
                ),
                theme.selection,
            ));
        }
    }
}

fn build_cursor(view: &EditorView, out: &mut Prepaint, window: &mut Window, theme: &Theme) {
    let cur = view.editor.cursor();
    let doc = &view.editor.doc;
    // 光标只会落在活动块，活动块一定是源码模式，映射是恒等的
    let hit = out.lines.iter().find_map(|vl| {
        if !matches!(vl.map, LineMap::Source) {
            return None;
        }
        let local = vl.local_offset(cur, doc)?;
        let p = vl.shaped.position_for_index(local, vl.line_height)?;
        Some((vl.clone(), p))
    });
    let Some((vl, p)) = hit else { return };

    let x = f32::from(vl.origin.x) + f32::from(p.x);
    let y = f32::from(vl.origin.y) + f32::from(p.y);
    out.cursor = Some(fill(
        Bounds::new(point(px(x), px(y)), size(px(2.), vl.line_height)),
        theme.accent,
    ));

    // AI 幽灵文本紧跟光标，弱色斜体，Tab 采纳
    if let Some(g) = &view.ghost {
        if !g.is_empty() {
            let mut f = body_font();
            f.style = gpui::FontStyle::Italic;
            let run = gpui::TextRun {
                len: g.len(),
                font: f,
                color: theme.ghost,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            // 幽灵文本只显示第一行，多行提示会把布局撑乱
            let first: String = g.lines().next().unwrap_or("").to_string();
            if !first.is_empty() {
                let run = gpui::TextRun {
                    len: first.len(),
                    ..run
                };
                let line = window.text_system().shape_line(
                    first.into(),
                    Metrics::BODY,
                    &[run],
                    None,
                );
                out.ghost = Some((point(px(x + 2.), px(y)), line));
            }
        }
    }
}
