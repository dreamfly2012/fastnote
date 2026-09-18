//! 编辑器状态机：光标、选区、撤销栈、Markdown 智能输入。
//!
//! 刻意不依赖任何 GUI 框架。编辑逻辑是最容易出 bug 的部分，
//! 放在这里可以脱离窗口环境做单元测试；GPUI 层只负责「画」和「转发按键」。

use std::ops::Range;
use std::time::{Duration, Instant};

use crate::block::{BlockKind, ListMarker};
use crate::document::Document;

/// 连续输入在此时间内视为同一次撤销组。
const UNDO_MERGE_WINDOW: Duration = Duration::from_millis(700);

#[derive(Clone, Debug)]
enum Edit {
    Insert { at: usize, text: String },
    Delete { at: usize, text: String },
}

impl Edit {
    fn invert(&self) -> Edit {
        match self {
            Edit::Insert { at, text } => Edit::Delete {
                at: *at,
                text: text.clone(),
            },
            Edit::Delete { at, text } => Edit::Insert {
                at: *at,
                text: text.clone(),
            },
        }
    }
}

#[derive(Clone, Debug)]
struct UndoGroup {
    /// 已经取反的操作，按逆序 apply 即可回退
    inverted: Vec<Edit>,
    cursor_before: usize,
    cursor_after: usize,
}

/// 光标上下移动时期望停靠的列。存起来才能实现「长行→短行→长行」列位不丢失。
#[derive(Clone, Copy, Debug)]
struct Goal(usize);

pub struct Editor {
    pub doc: Document,
    cursor: usize,
    /// 选区另一端。等于 cursor 时表示没有选区。
    anchor: usize,
    goal: Option<Goal>,
    undo: Vec<UndoGroup>,
    redo: Vec<UndoGroup>,
    /// 当前撤销组，None 表示尚未开组
    pending: Option<UndoGroup>,
    last_edit_at: Option<Instant>,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new(Document::empty())
    }
}

impl Editor {
    pub fn new(doc: Document) -> Self {
        Self {
            doc,
            cursor: 0,
            anchor: 0,
            goal: None,
            undo: Vec::new(),
            redo: Vec::new(),
            pending: None,
            last_edit_at: None,
        }
    }

    // ---------- 光标与选区 ----------

    #[inline]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    #[inline]
    pub fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    pub fn selection(&self) -> Range<usize> {
        if self.cursor <= self.anchor {
            self.cursor..self.anchor
        } else {
            self.anchor..self.cursor
        }
    }

    pub fn selected_text(&self) -> String {
        let r = self.selection();
        if r.is_empty() {
            String::new()
        } else {
            self.doc.slice_to_string(r)
        }
    }

    pub fn set_cursor(&mut self, byte: usize) {
        let b = byte.min(self.doc.len_bytes());
        self.cursor = b;
        self.anchor = b;
        self.goal = None;
    }

    pub fn select(&mut self, range: Range<usize>) {
        let len = self.doc.len_bytes();
        self.anchor = range.start.min(len);
        self.cursor = range.end.min(len);
        self.goal = None;
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.doc.len_bytes();
    }

    pub fn collapse_selection(&mut self) {
        self.anchor = self.cursor;
    }

    fn place(&mut self, byte: usize, extend: bool) {
        self.cursor = byte.min(self.doc.len_bytes());
        if !extend {
            self.anchor = self.cursor;
        }
    }

    pub fn move_left(&mut self, extend: bool) {
        // 有选区且不扩展时，左移应落到选区左端而不是从光标再退一格
        if !extend && self.has_selection() {
            let s = self.selection().start;
            self.place(s, false);
        } else {
            let p = self.doc.prev_char_boundary(self.cursor);
            self.place(p, extend);
        }
        self.goal = None;
    }

    pub fn move_right(&mut self, extend: bool) {
        if !extend && self.has_selection() {
            let e = self.selection().end;
            self.place(e, false);
        } else {
            let p = self.doc.next_char_boundary(self.cursor);
            self.place(p, extend);
        }
        self.goal = None;
    }

    pub fn move_up(&mut self, extend: bool) {
        let (line, col) = self.doc.byte_to_line_col(self.cursor);
        let goal = self.goal.map(|g| g.0).unwrap_or(col);
        if line == 0 {
            self.place(0, extend);
        } else {
            let target = self.doc.line_col_to_byte(line - 1, goal);
            self.place(target, extend);
        }
        self.goal = Some(Goal(goal));
    }

    pub fn move_down(&mut self, extend: bool) {
        let (line, col) = self.doc.byte_to_line_col(self.cursor);
        let goal = self.goal.map(|g| g.0).unwrap_or(col);
        let last = self.doc.len_lines().saturating_sub(1);
        if line >= last {
            self.place(self.doc.len_bytes(), extend);
        } else {
            let target = self.doc.line_col_to_byte(line + 1, goal);
            self.place(target, extend);
        }
        self.goal = Some(Goal(goal));
    }

    pub fn move_line_start(&mut self, extend: bool) {
        let (line, _) = self.doc.byte_to_line_col(self.cursor);
        let start = self.doc.line_to_byte(line);
        let text = self.doc.line_text(line);
        // 先跳到首个非空白处（更常用），已在该处则跳到真正行首
        let indent = text.len() - text.trim_start().len();
        let smart = start + indent;
        let target = if self.cursor == smart { start } else { smart };
        self.place(target, extend);
        self.goal = None;
    }

    pub fn move_line_end(&mut self, extend: bool) {
        let (line, _) = self.doc.byte_to_line_col(self.cursor);
        let end = self.doc.line_to_byte(line) + self.doc.line_text(line).len();
        self.place(end, extend);
        self.goal = None;
    }

    pub fn move_doc_start(&mut self, extend: bool) {
        self.place(0, extend);
        self.goal = None;
    }

    pub fn move_doc_end(&mut self, extend: bool) {
        self.place(self.doc.len_bytes(), extend);
        self.goal = None;
    }

    /// 按词移动。词的定义：连续的字母数字/下划线，或连续的 CJK 字符。
    pub fn move_word_left(&mut self, extend: bool) {
        let mut p = self.cursor;
        // 先跳过左侧空白
        while p > 0 {
            let prev = self.doc.prev_char_boundary(p);
            let c = self.char_at(prev);
            if c.map(|c| c.is_whitespace()).unwrap_or(false) {
                p = prev;
            } else {
                break;
            }
        }
        while p > 0 {
            let prev = self.doc.prev_char_boundary(p);
            match self.char_at(prev) {
                Some(c) if is_word_char(c) => p = prev,
                _ => break,
            }
        }
        self.place(p, extend);
        self.goal = None;
    }

    pub fn move_word_right(&mut self, extend: bool) {
        let len = self.doc.len_bytes();
        let mut p = self.cursor;
        while p < len {
            match self.char_at(p) {
                Some(c) if c.is_whitespace() => p = self.doc.next_char_boundary(p),
                _ => break,
            }
        }
        while p < len {
            match self.char_at(p) {
                Some(c) if is_word_char(c) => p = self.doc.next_char_boundary(p),
                _ => break,
            }
        }
        self.place(p, extend);
        self.goal = None;
    }

    fn char_at(&self, byte: usize) -> Option<char> {
        if byte >= self.doc.len_bytes() {
            return None;
        }
        let end = self.doc.next_char_boundary(byte);
        self.doc.slice_to_string(byte..end).chars().next()
    }

    // ---------- 编辑原语 ----------

    fn begin_group(&mut self, mergeable: bool) {
        let now = Instant::now();
        let can_merge = mergeable
            && self.pending.is_some()
            && self
                .last_edit_at
                .map(|t| now.duration_since(t) < UNDO_MERGE_WINDOW)
                .unwrap_or(false);

        if !can_merge {
            self.flush_group();
            self.pending = Some(UndoGroup {
                inverted: Vec::new(),
                cursor_before: self.cursor,
                cursor_after: self.cursor,
            });
        }
        self.last_edit_at = Some(now);
        self.redo.clear();
    }

    fn flush_group(&mut self) {
        if let Some(mut g) = self.pending.take() {
            if !g.inverted.is_empty() {
                g.cursor_after = self.cursor;
                self.undo.push(g);
                // 撤销栈无上限会在长时间编辑大文件时吃内存
                if self.undo.len() > 2000 {
                    self.undo.remove(0);
                }
            }
        }
    }

    /// 显式收尾当前撤销组。移动光标或失焦时调用，避免把两次无关编辑并成一组。
    pub fn break_undo_group(&mut self) {
        self.flush_group();
        self.last_edit_at = None;
    }

    fn record(&mut self, edit: Edit) {
        if let Some(g) = self.pending.as_mut() {
            g.inverted.push(edit.invert());
        }
    }

    fn do_insert(&mut self, at: usize, text: &str) {
        if text.is_empty() {
            return;
        }
        self.doc.insert(at, text);
        self.record(Edit::Insert {
            at,
            text: text.to_string(),
        });
    }

    fn do_delete(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let text = self.doc.slice_to_string(range.clone());
        self.doc.remove(range.clone());
        self.record(Edit::Delete {
            at: range.start,
            text,
        });
    }

    fn delete_selection_inner(&mut self) -> bool {
        let sel = self.selection();
        if sel.is_empty() {
            return false;
        }
        self.do_delete(sel.clone());
        self.cursor = sel.start;
        self.anchor = sel.start;
        true
    }

    // ---------- 对外编辑动作 ----------

    /// 插入文本（普通打字、粘贴）。
    pub fn insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        // 单字符输入才合并撤销组；粘贴一大段应该独立成组
        let mergeable = text.chars().count() == 1 && !text.contains('\n');
        self.begin_group(mergeable);
        self.delete_selection_inner();
        let at = self.cursor;
        self.do_insert(at, text);
        self.cursor = at + text.len();
        self.anchor = self.cursor;
        self.goal = None;
        if !mergeable {
            self.flush_group();
        }
    }

    pub fn backspace(&mut self) {
        self.begin_group(true);
        if self.delete_selection_inner() {
            self.flush_group();
            return;
        }
        if self.cursor == 0 {
            return;
        }
        let prev = self.doc.prev_char_boundary(self.cursor);
        // 行首退格若前面是换行，要连 \r\n 一起删掉，否则会留下孤立的 \r
        let start = if self.doc.slice_to_string(prev..self.cursor) == "\n" {
            let p2 = self.doc.prev_char_boundary(prev);
            if p2 < prev && self.doc.slice_to_string(p2..prev) == "\r" {
                p2
            } else {
                prev
            }
        } else {
            prev
        };
        self.do_delete(start..self.cursor);
        self.cursor = start;
        self.anchor = start;
        self.goal = None;
    }

    pub fn delete_forward(&mut self) {
        self.begin_group(true);
        if self.delete_selection_inner() {
            self.flush_group();
            return;
        }
        let len = self.doc.len_bytes();
        if self.cursor >= len {
            return;
        }
        let mut end = self.doc.next_char_boundary(self.cursor);
        if self.doc.slice_to_string(self.cursor..end) == "\r" {
            let e2 = self.doc.next_char_boundary(end);
            if self.doc.slice_to_string(end..e2) == "\n" {
                end = e2;
            }
        }
        self.do_delete(self.cursor..end);
        self.goal = None;
    }

    /// 回车。带 Markdown 智能续行：列表、任务项、引用会自动带出标记符。
    pub fn newline(&mut self) {
        self.begin_group(false);
        self.delete_selection_inner();

        let nl = self.doc.newline().as_str().to_string();
        let (line, col) = self.doc.byte_to_line_col(self.cursor);
        let text = self.doc.line_text(line);
        let prefix = continuation_prefix(&text);

        // 光标在行尾、且当前行只有列表标记没有正文 → 认为用户想退出列表
        let body_empty = match &prefix {
            Some(p) => text.trim_end().len() <= p.trimmed_marker_len,
            None => false,
        };

        if body_empty && col >= text.trim_end().len() {
            // 清掉本行的空标记，只留换行
            let line_start = self.doc.line_to_byte(line);
            let line_end = line_start + text.len();
            self.do_delete(line_start..line_end);
            self.cursor = line_start;
            self.anchor = line_start;
            self.do_insert(self.cursor, &nl);
            self.cursor += nl.len();
            self.anchor = self.cursor;
            self.goal = None;
            self.flush_group();
            return;
        }

        let ins = match &prefix {
            Some(p) => format!("{nl}{}", p.next),
            None => nl,
        };
        let at = self.cursor;
        self.do_insert(at, &ins);
        self.cursor = at + ins.len();
        self.anchor = self.cursor;
        self.goal = None;
        self.flush_group();
    }

    /// Tab：有选区则整块缩进，否则插入两个空格。
    pub fn indent(&mut self) {
        self.begin_group(false);
        if self.has_selection() {
            self.shift_lines(true);
        } else {
            let at = self.cursor;
            self.do_insert(at, "  ");
            self.cursor = at + 2;
            self.anchor = self.cursor;
        }
        self.flush_group();
    }

    pub fn outdent(&mut self) {
        self.begin_group(false);
        self.shift_lines(false);
        self.flush_group();
    }

    fn shift_lines(&mut self, add: bool) {
        let sel = self.selection();
        let first = self.doc.byte_to_line(sel.start);
        let last = self
            .doc
            .byte_to_line(sel.end.max(sel.start).saturating_sub(0));
        let mut delta_first = 0isize;
        let mut delta_total = 0isize;

        // 从后往前改，避免前面的编辑挪动后面的偏移
        for line in (first..=last).rev() {
            let start = self.doc.line_to_byte(line);
            let text = self.doc.line_text(line);
            if add {
                if text.trim().is_empty() {
                    continue;
                }
                self.do_insert(start, "  ");
                delta_total += 2;
                if line == first {
                    delta_first = 2;
                }
            } else {
                let spaces = text.len() - text.trim_start_matches(' ').len();
                let cut = spaces.min(2);
                if cut > 0 {
                    self.do_delete(start..start + cut);
                    delta_total -= cut as isize;
                    if line == first {
                        delta_first = -(cut as isize);
                    }
                }
            }
        }

        let new_start = (sel.start as isize + delta_first).max(0) as usize;
        let new_end = (sel.end as isize + delta_total).max(new_start as isize) as usize;
        self.anchor = new_start.min(self.doc.len_bytes());
        self.cursor = new_end.min(self.doc.len_bytes());
    }

    /// 用标记符包裹选区（Ctrl+B / Ctrl+I 等）。已包裹则去掉，实现开关效果。
    pub fn toggle_wrap(&mut self, marker: &str) {
        self.begin_group(false);
        let sel = self.selection();
        if sel.is_empty() {
            // 无选区时插入一对标记并把光标放中间
            let at = self.cursor;
            let pair = format!("{marker}{marker}");
            self.do_insert(at, &pair);
            self.cursor = at + marker.len();
            self.anchor = self.cursor;
            self.flush_group();
            return;
        }

        let text = self.doc.slice_to_string(sel.clone());
        let m = marker.len();
        if text.len() >= m * 2 && text.starts_with(marker) && text.ends_with(marker) {
            let inner = text[m..text.len() - m].to_string();
            self.do_delete(sel.clone());
            self.do_insert(sel.start, &inner);
            self.anchor = sel.start;
            self.cursor = sel.start + inner.len();
        } else {
            let wrapped = format!("{marker}{text}{marker}");
            self.do_delete(sel.clone());
            self.do_insert(sel.start, &wrapped);
            self.anchor = sel.start;
            self.cursor = sel.start + wrapped.len();
        }
        self.flush_group();
    }

    /// 切换当前行的标题级别。`level` 为 0 时降为普通段落。
    pub fn set_heading(&mut self, level: u8) {
        self.begin_group(false);
        let (line, _) = self.doc.byte_to_line_col(self.cursor);
        let start = self.doc.line_to_byte(line);
        let text = self.doc.line_text(line);
        let stripped = text.trim_start_matches('#').trim_start();
        let new = if level == 0 {
            stripped.to_string()
        } else {
            format!("{} {}", "#".repeat(level as usize), stripped)
        };
        self.do_delete(start..start + text.len());
        self.do_insert(start, &new);
        let c = start + new.len();
        self.cursor = c;
        self.anchor = c;
        self.flush_group();
    }

    /// 切换当前行任务项的勾选状态。
    pub fn toggle_task(&mut self) -> bool {
        let (line, _) = self.doc.byte_to_line_col(self.cursor);
        let start = self.doc.line_to_byte(line);
        let text = self.doc.line_text(line);
        let t = text.trim_start();
        let indent = text.len() - t.len();

        let (marker_len, checked) = if let Some(rest) = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("+ "))
        {
            if rest.starts_with("[ ] ") {
                (2, false)
            } else if rest.starts_with("[x] ") || rest.starts_with("[X] ") {
                (2, true)
            } else {
                return false;
            }
        } else {
            return false;
        };

        self.begin_group(false);
        let box_at = start + indent + marker_len;
        self.do_delete(box_at..box_at + 3);
        self.do_insert(box_at, if checked { "[ ]" } else { "[x]" });
        self.flush_group();
        true
    }

    pub fn replace_selection(&mut self, text: &str) {
        self.begin_group(false);
        self.delete_selection_inner();
        let at = self.cursor;
        self.do_insert(at, text);
        self.cursor = at + text.len();
        self.anchor = self.cursor;
        self.flush_group();
    }

    /// AI 流式写入：把增量追加到光标处并推进光标，不打断撤销组的合并。
    pub fn stream_insert(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if self.pending.is_none() {
            self.begin_group(false);
        }
        let at = self.cursor;
        self.do_insert(at, delta);
        self.cursor = at + delta.len();
        self.anchor = self.cursor;
    }

    /// 结束一次 AI 流式写入，把整段生成收成一个可撤销单元。
    pub fn finish_stream(&mut self) {
        self.flush_group();
    }

    // ---------- 撤销 / 重做 ----------

    pub fn undo(&mut self) -> bool {
        self.flush_group();
        let Some(g) = self.undo.pop() else {
            return false;
        };
        for e in g.inverted.iter().rev() {
            self.apply_raw(e);
        }
        self.cursor = g.cursor_before.min(self.doc.len_bytes());
        self.anchor = self.cursor;
        self.redo.push(UndoGroup {
            inverted: g.inverted.iter().map(|e| e.invert()).collect(),
            cursor_before: g.cursor_after,
            cursor_after: g.cursor_before,
        });
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(g) = self.redo.pop() else {
            return false;
        };
        for e in g.inverted.iter().rev() {
            self.apply_raw(e);
        }
        self.cursor = g.cursor_before.min(self.doc.len_bytes());
        self.anchor = self.cursor;
        self.undo.push(UndoGroup {
            inverted: g.inverted.iter().map(|e| e.invert()).collect(),
            cursor_before: g.cursor_after,
            cursor_after: g.cursor_before,
        });
        true
    }

    fn apply_raw(&mut self, e: &Edit) {
        match e {
            Edit::Insert { at, text } => self.doc.insert(*at, text),
            Edit::Delete { at, text } => self.doc.remove(*at..*at + text.len()),
        }
    }

    // ---------- 渲染辅助 ----------

    /// 光标所在块的行区间。渲染层据此决定哪个块显示源码。
    pub fn active_block_lines(&mut self) -> Option<Range<usize>> {
        let c = self.cursor;
        self.doc.block_at_byte(c).map(|b| b.lines.clone())
    }

    pub fn cursor_line_col(&self) -> (usize, usize) {
        self.doc.byte_to_line_col(self.cursor)
    }
}

#[inline]
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-'
}

struct Continuation {
    /// 下一行应自动带出的前缀
    next: String,
    /// 当前行标记符长度（用于判断是否空条目）
    trimmed_marker_len: usize,
}

/// 根据当前行推断回车后应自动补上的前缀。
fn continuation_prefix(line: &str) -> Option<Continuation> {
    let t = line.trim_start();
    let indent_len = line.len() - t.len();
    let indent = &line[..indent_len];

    // 引用
    if t.starts_with('>') {
        return Some(Continuation {
            next: format!("{indent}> "),
            trimmed_marker_len: indent_len + 2,
        });
    }

    // 任务项 / 无序列表
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(m) {
            if rest.starts_with("[ ] ") || rest.starts_with("[x] ") || rest.starts_with("[X] ") {
                return Some(Continuation {
                    next: format!("{indent}{m}[ ] "),
                    trimmed_marker_len: indent_len + m.len() + 4,
                });
            }
            return Some(Continuation {
                next: format!("{indent}{m}"),
                trimmed_marker_len: indent_len + m.len(),
            });
        }
    }

    // 有序列表：序号自增
    let digits = t.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits > 0 && digits <= 9 {
        let after = &t[digits..];
        for sep in [". ", ") "] {
            if after.starts_with(sep) {
                let n: u64 = t[..digits].parse().unwrap_or(1);
                let sep_char = &sep[..1];
                return Some(Continuation {
                    next: format!("{indent}{}{sep_char} ", n + 1),
                    trimmed_marker_len: indent_len + digits + sep.len(),
                });
            }
        }
    }

    None
}

/// 供渲染层查询块类型的便捷方法。
impl Editor {
    pub fn active_block_kind(&mut self) -> Option<BlockKind> {
        let c = self.cursor;
        self.doc.block_at_byte(c).map(|b| b.kind.clone())
    }

    /// 当前行若是有序列表，返回其序号（用于渲染层显示）。
    pub fn current_list_number(&mut self) -> Option<u64> {
        match self.active_block_kind()? {
            BlockKind::List {
                marker: ListMarker::Ordered(n),
                ..
            } => Some(n),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ed(src: &str) -> Editor {
        Editor::new(Document::from_str(src))
    }

    fn text(e: &Editor) -> String {
        e.doc.rope().to_string()
    }

    #[test]
    fn insert_and_backspace_multibyte() {
        let mut e = ed("");
        e.insert("中");
        e.insert("文");
        assert_eq!(text(&e), "中文");
        assert_eq!(e.cursor(), 6);
        e.backspace();
        assert_eq!(text(&e), "中");
        assert_eq!(e.cursor(), 3);
    }

    #[test]
    fn selection_replace() {
        let mut e = ed("hello world");
        e.select(0..5);
        assert_eq!(e.selected_text(), "hello");
        e.replace_selection("你好");
        assert_eq!(text(&e), "你好 world");
        assert_eq!(e.cursor(), 6);
    }

    #[test]
    fn undo_merges_consecutive_typing_into_one_group() {
        let mut e = ed("");
        e.insert("a");
        e.insert("b");
        e.insert("c");
        assert_eq!(text(&e), "abc");
        assert!(e.undo());
        // 三次连续输入应一并撤销
        assert_eq!(text(&e), "");
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut e = ed("base");
        e.set_cursor(4);
        e.insert(" more text");
        let after = text(&e);
        assert!(e.undo());
        assert_eq!(text(&e), "base");
        assert!(e.redo());
        assert_eq!(text(&e), after);
    }

    #[test]
    fn paste_is_its_own_undo_group() {
        let mut e = ed("");
        e.insert("x");
        e.insert("一大段粘贴内容");
        assert!(e.undo());
        assert_eq!(text(&e), "x", "粘贴不应和前面的输入合并");
    }

    #[test]
    fn newline_continues_bullet_list() {
        let mut e = ed("- 第一项");
        e.move_doc_end(false);
        e.newline();
        assert_eq!(text(&e), "- 第一项\n- ");
    }

    #[test]
    fn newline_increments_ordered_list() {
        let mut e = ed("3. 三");
        e.move_doc_end(false);
        e.newline();
        assert!(text(&e).ends_with("\n4. "), "实际: {:?}", text(&e));
    }

    #[test]
    fn newline_continues_task_item_unchecked() {
        let mut e = ed("- [x] 完成的事");
        e.move_doc_end(false);
        e.newline();
        assert!(text(&e).ends_with("\n- [ ] "), "实际: {:?}", text(&e));
    }

    #[test]
    fn newline_on_empty_list_item_exits_list() {
        let mut e = ed("- 一\n- ");
        e.move_doc_end(false);
        e.newline();
        assert_eq!(text(&e), "- 一\n\n", "空列表项回车应退出列表");
    }

    #[test]
    fn newline_preserves_indent_for_nested_list() {
        let mut e = ed("  - 缩进项");
        e.move_doc_end(false);
        e.newline();
        assert!(text(&e).ends_with("\n  - "), "实际: {:?}", text(&e));
    }

    #[test]
    fn newline_continues_quote() {
        let mut e = ed("> 引用");
        e.move_doc_end(false);
        e.newline();
        assert!(text(&e).ends_with("\n> "));
    }

    #[test]
    fn toggle_wrap_bold_on_and_off() {
        let mut e = ed("加粗我");
        e.select_all();
        e.toggle_wrap("**");
        assert_eq!(text(&e), "**加粗我**");
        e.select(0..text(&e).len());
        e.toggle_wrap("**");
        assert_eq!(text(&e), "加粗我");
    }

    #[test]
    fn toggle_wrap_without_selection_places_cursor_inside() {
        let mut e = ed("");
        e.toggle_wrap("**");
        assert_eq!(text(&e), "****");
        assert_eq!(e.cursor(), 2);
    }

    #[test]
    fn set_heading_and_demote() {
        let mut e = ed("标题行");
        e.set_heading(2);
        assert_eq!(text(&e), "## 标题行");
        e.set_heading(0);
        assert_eq!(text(&e), "标题行");
    }

    #[test]
    fn toggle_task_flips_checkbox() {
        let mut e = ed("- [ ] 待办");
        assert!(e.toggle_task());
        assert_eq!(text(&e), "- [x] 待办");
        assert!(e.toggle_task());
        assert_eq!(text(&e), "- [ ] 待办");
    }

    #[test]
    fn toggle_task_returns_false_on_plain_list() {
        let mut e = ed("- 普通项");
        assert!(!e.toggle_task());
    }

    #[test]
    fn indent_and_outdent_selection() {
        let mut e = ed("a\nb\n");
        e.select(0..3);
        e.indent();
        assert_eq!(text(&e), "  a\n  b\n");
        e.outdent();
        assert_eq!(text(&e), "a\nb\n");
    }

    #[test]
    fn vertical_motion_keeps_goal_column() {
        let mut e = ed("长长长长的一行\n短\n另一条长行内容\n");
        e.set_cursor(12); // 第一行中部
        let (_, col) = e.cursor_line_col();
        e.move_down(false); // 到短行，列被压缩
        e.move_down(false); // 回到长行，应恢复原列
        let (line, c2) = e.cursor_line_col();
        assert_eq!(line, 2);
        assert_eq!(c2, col, "上下移动应保持目标列");
    }

    #[test]
    fn word_motion_stops_at_boundaries() {
        let mut e = ed("foo bar baz");
        e.move_doc_end(false);
        e.move_word_left(false);
        assert_eq!(e.cursor(), 8);
        e.move_word_left(false);
        assert_eq!(e.cursor(), 4);
    }

    #[test]
    fn home_toggles_between_indent_and_line_start() {
        let mut e = ed("    缩进内容");
        e.move_doc_end(false);
        e.move_line_start(false);
        assert_eq!(e.cursor(), 4, "先到首个非空白");
        e.move_line_start(false);
        assert_eq!(e.cursor(), 0, "再按到真正行首");
    }

    #[test]
    fn left_with_selection_collapses_to_start() {
        let mut e = ed("abcdef");
        e.select(2..5);
        e.move_left(false);
        assert_eq!(e.cursor(), 2);
        assert!(!e.has_selection());
    }

    #[test]
    fn stream_insert_is_one_undo_unit() {
        let mut e = ed("题目：");
        e.move_doc_end(false);
        for d in ["这", "是", "AI", "写的"] {
            e.stream_insert(d);
        }
        e.finish_stream();
        assert_eq!(text(&e), "题目：这是AI写的");
        assert!(e.undo());
        assert_eq!(text(&e), "题目：", "整段生成应一次撤销干净");
    }

    #[test]
    fn active_block_tracks_cursor() {
        let mut e = ed("# 标题\n\n段落内容\n");
        e.set_cursor(0);
        assert!(matches!(
            e.active_block_kind(),
            Some(BlockKind::Heading { level: 1 })
        ));
        let off = e.doc.line_to_byte(2);
        e.set_cursor(off);
        assert!(matches!(e.active_block_kind(), Some(BlockKind::Paragraph)));
    }

    #[test]
    fn crlf_backspace_removes_whole_line_break() {
        let mut e = ed("a\r\nb");
        e.set_cursor(3); // 紧跟在 \r\n 之后
        e.backspace();
        assert_eq!(text(&e), "ab", "不应留下孤立的 \\r");
    }

    #[test]
    fn delete_forward_at_end_is_noop() {
        let mut e = ed("ab");
        e.move_doc_end(false);
        e.delete_forward();
        assert_eq!(text(&e), "ab");
    }
}
