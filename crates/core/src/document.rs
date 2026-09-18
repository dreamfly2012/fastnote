//! 文档模型：Rope 存储 + 惰性视口块解析。
//!
//! # 为什么不做全量增量 AST
//!
//! 常见做法是打开文件时解析整篇 AST，编辑后用 diff 打补丁。但对 100MB 文档而言，
//! 全量解析本身就要几百毫秒，这笔开销无论怎么增量都省不掉。
//!
//! 这里换一个思路：**只解析视口**。屏幕上最多几十行，从"安全边界"开始扫描到视口底部，
//! 成本与文件大小无关（微秒级）。于是每次按键后直接重解析视口即可，
//! 连增量 diff 的复杂度都不需要，正确性反而更好。
//!
//! 打开文件的耗时因此只剩下"读盘 + 构建 Rope 行索引"，与解析器无关。
//!
//! # 偏移约定
//!
//! 对外 API 统一使用**字节偏移**（ropey 内部用 char 偏移，本层负责转换）。
//! 理由：Markdown 解析、语法高亮、AI 请求都按字节切片，统一成字节可以避免来回换算出错。

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::ops::Range;
use std::path::{Path, PathBuf};

use std::cell::Cell;

use anyhow::{Context, Result};
use ropey::Rope;

use crate::block::{scan_blocks, Block, LineInput};

/// 向上寻找安全解析起点时最多回看的行数。
const UP_SCAN: usize = 256;
/// 校正代码围栏奇偶性时最多回看的行数。
const FENCE_SCAN: usize = 2048;
/// 向下扩展到块边界时最多前看的行数。
const DOWN_SCAN: usize = 256;
/// 视口解析时上下额外多解析的行数。
///
/// 这不是为了显示，而是为了**缓存命中**：只解析视口那 60 行的话，
/// 用户每滚一行就 miss 一次，又要重做一遍 `safe_start` 的围栏回溯。
/// 多解析几屏的成本很低（块扫描是线性的），换来滚动时几乎全部命中。
const VIEW_PAD: usize = 256;

struct ViewCache {
    revision: u64,
    /// 本次解析实际覆盖的行区间
    covered: Range<usize>,
    blocks: Vec<Block>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Newline {
    Lf,
    Crlf,
}

impl Newline {
    #[inline]
    pub fn as_str(self) -> &'static str {
        match self {
            Newline::Lf => "\n",
            Newline::Crlf => "\r\n",
        }
    }
}

pub struct Document {
    text: Rope,
    path: Option<PathBuf>,
    dirty: bool,
    revision: u64,
    newline: Newline,
    cache: Option<ViewCache>,
    /// 代码围栏结构版本。只在编辑可能改变围栏分布时才递增
    /// （即插入/删除的文本含 '`'、'~' 或换行）。
    /// 普通打字不动它，于是 `fence_fix` 的回溯结果可以一直复用。
    fence_rev: u64,
    /// `fence_fix` 的记忆：(fence_rev, 输入行, 校正后的行)
    fence_cache: Cell<Option<(u64, usize, usize)>>,
}

impl Default for Document {
    fn default() -> Self {
        Self::empty()
    }
}

impl Document {
    pub fn empty() -> Self {
        Self {
            text: Rope::new(),
            path: None,
            dirty: false,
            revision: 1,
            newline: if cfg!(windows) {
                Newline::Crlf
            } else {
                Newline::Lf
            },
            cache: None,
            fence_rev: 1,
            fence_cache: Cell::new(None),
        }
    }

    pub fn from_str(src: &str) -> Self {
        let newline = if src.contains("\r\n") {
            Newline::Crlf
        } else {
            Newline::Lf
        };
        Self {
            text: Rope::from_str(src),
            path: None,
            dirty: false,
            revision: 1,
            newline,
            cache: None,
            fence_rev: 1,
            fence_cache: Cell::new(None),
        }
    }

    /// 打开文件。流式构建 Rope，不做任何 Markdown 解析。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path).with_context(|| format!("打开文件失败: {}", path.display()))?;
        // 64KB 缓冲：实测比默认 8KB 在大文件上快一截，且内存开销可忽略
        let text = Rope::from_reader(BufReader::with_capacity(64 * 1024, file))
            .with_context(|| format!("读取文件失败: {}", path.display()))?;

        // 只嗅探开头若干行判断换行风格，避免为此扫全文
        let probe_end = text.len_chars().min(4096);
        let probe = text.slice(..probe_end).to_string();
        let newline = if probe.contains("\r\n") {
            Newline::Crlf
        } else {
            Newline::Lf
        };

        Ok(Self {
            text,
            path: Some(path.to_path_buf()),
            dirty: false,
            revision: 1,
            newline,
            cache: None,
            fence_rev: 1,
            fence_cache: Cell::new(None),
        })
    }

    pub fn save(&mut self) -> Result<()> {
        let path = self
            .path
            .clone()
            .context("文档没有关联路径，请先另存为")?;
        self.save_as(path)
    }

    pub fn save_as(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let file =
            File::create(path).with_context(|| format!("写入文件失败: {}", path.display()))?;
        self.text
            .write_to(BufWriter::with_capacity(64 * 1024, file))
            .with_context(|| format!("写入文件失败: {}", path.display()))?;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        Ok(())
    }

    // ---------- 基本信息 ----------

    #[inline]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    #[inline]
    pub fn set_path(&mut self, path: PathBuf) {
        self.path = Some(path);
    }

    #[inline]
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    #[inline]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    #[inline]
    pub fn newline(&self) -> Newline {
        self.newline
    }

    #[inline]
    pub fn len_bytes(&self) -> usize {
        self.text.len_bytes()
    }

    #[inline]
    pub fn len_lines(&self) -> usize {
        self.text.len_lines()
    }

    #[inline]
    pub fn rope(&self) -> &Rope {
        &self.text
    }

    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名".to_string())
    }

    /// 取一行文本（不含换行符）。
    ///
    /// 视口每帧要取几十行，所以只做一次分配：
    /// 直接按 chunks 拼进预分配好的 String，避免再套一层临时 String。
    pub fn line_text(&self, line: usize) -> String {
        if line >= self.text.len_lines() {
            return String::new();
        }
        let s = self.text.line(line);
        let mut out = String::with_capacity(s.len_bytes());
        for chunk in s.chunks() {
            out.push_str(chunk);
        }
        // 只剥掉一个行尾（"\n" 或 "\r\n"），行内的 \r 要留着
        if out.ends_with('\n') {
            out.pop();
            if out.ends_with('\r') {
                out.pop();
            }
        }
        out
    }

    fn line_eol_width(&self, line: usize) -> u8 {
        if line >= self.text.len_lines() {
            return 0;
        }
        let s = self.text.line(line);
        let n = s.len_chars();
        if n == 0 {
            return 0;
        }
        let last = s.char(n - 1);
        if last != '\n' {
            return 0;
        }
        if n >= 2 && s.char(n - 2) == '\r' {
            2
        } else {
            1
        }
    }

    fn is_blank_line(&self, line: usize) -> bool {
        if line >= self.text.len_lines() {
            return true;
        }
        self.text
            .line(line)
            .chars()
            .all(|c| c == ' ' || c == '\t' || c == '\r' || c == '\n')
    }

    /// 该行是否是代码围栏行（``` 或 ~~~）。
    ///
    /// 热路径：`safe_start` 每次视口解析都要连续调用上千次，
    /// 所以这里必须**零分配** —— 直接在字符流上判断，不构造中间 String。
    fn line_starts_fence(&self, line: usize) -> bool {
        if line >= self.text.len_lines() {
            return false;
        }
        let s = self.text.line(line);
        let mut it = s.chars();

        // Markdown 规定围栏最多缩进 3 空格，超了就不是围栏
        let mut indent = 0u8;
        let mut fence_ch = None;
        for c in it.by_ref() {
            match c {
                ' ' | '\t' => {
                    indent += 1;
                    if indent > 3 {
                        return false;
                    }
                }
                '`' | '~' => {
                    fence_ch = Some(c);
                    break;
                }
                _ => return false,
            }
        }
        let Some(f) = fence_ch else { return false };

        // 需要连续三个相同的围栏字符
        let mut n = 1u8;
        for c in it {
            if c == f {
                n += 1;
                if n >= 3 {
                    return true;
                }
            } else {
                break;
            }
        }
        false
    }

    // ---------- 偏移换算 ----------

    #[inline]
    pub fn byte_to_line(&self, byte: usize) -> usize {
        self.text.byte_to_line(byte.min(self.text.len_bytes()))
    }

    #[inline]
    pub fn line_to_byte(&self, line: usize) -> usize {
        self.text.line_to_byte(line.min(self.text.len_lines()))
    }

    /// 字节偏移 → (行号, 行内字节列)
    pub fn byte_to_line_col(&self, byte: usize) -> (usize, usize) {
        let byte = byte.min(self.text.len_bytes());
        let line = self.text.byte_to_line(byte);
        (line, byte - self.text.line_to_byte(line))
    }

    pub fn line_col_to_byte(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.text.len_lines().saturating_sub(1));
        let start = self.text.line_to_byte(line);
        let len = self.line_text(line).len();
        start + col.min(len)
    }

    /// 前一个字符边界（按 Unicode 标量，中文/emoji 不会被切半）。
    pub fn prev_char_boundary(&self, byte: usize) -> usize {
        let byte = byte.min(self.text.len_bytes());
        if byte == 0 {
            return 0;
        }
        let ch = self.text.byte_to_char(byte);
        if ch == 0 {
            return 0;
        }
        self.text.char_to_byte(ch - 1)
    }

    pub fn next_char_boundary(&self, byte: usize) -> usize {
        let len = self.text.len_bytes();
        if byte >= len {
            return len;
        }
        let ch = self.text.byte_to_char(byte);
        if ch + 1 >= self.text.len_chars() {
            return len;
        }
        self.text.char_to_byte(ch + 1)
    }

    pub fn slice_to_string(&self, range: Range<usize>) -> String {
        let len = self.text.len_bytes();
        let start = range.start.min(len);
        let end = range.end.min(len).max(start);
        let cs = self.text.byte_to_char(start);
        let ce = self.text.byte_to_char(end);
        self.text.slice(cs..ce).to_string()
    }

    // ---------- 编辑 ----------

    pub fn insert(&mut self, byte: usize, s: &str) {
        let byte = byte.min(self.text.len_bytes());
        let ch = self.text.byte_to_char(byte);
        self.text.insert(ch, s);
        self.bump(affects_fences(s));
    }

    pub fn remove(&mut self, range: Range<usize>) {
        let len = self.text.len_bytes();
        let start = range.start.min(len);
        let end = range.end.min(len);
        if start >= end {
            return;
        }
        let cs = self.text.byte_to_char(start);
        let ce = self.text.byte_to_char(end);
        // 删掉的内容含围栏字符/换行才算结构性改动。
        // 删除区间通常只有一两个字符，这个检查比无脑失效便宜得多。
        let structural = self.text.slice(cs..ce).chunks().any(affects_fences);
        self.text.remove(cs..ce);
        self.bump(structural);
    }

    pub fn replace(&mut self, range: Range<usize>, s: &str) {
        self.remove(range.clone());
        self.insert(range.start, s);
    }

    /// 记一次编辑。
    ///
    /// `structural` 表示这次改动可能改变代码围栏分布 —— 只有这时才让
    /// `fence_fix` 的回溯缓存失效。普通打字（不含 ` ~ 与换行）不触发，
    /// 于是省掉每次按键上千行的围栏回溯。
    #[inline]
    fn bump(&mut self, structural: bool) {
        self.revision = self.revision.wrapping_add(1);
        self.dirty = true;
        // 视口块缓存与文本强相关，任何编辑都失效
        self.cache = None;
        if structural {
            self.fence_rev = self.fence_rev.wrapping_add(1);
            self.fence_cache.set(None);
        }
    }

    // ---------- 惰性块解析 ----------

    /// 找到可以安全开始解析的行号。
    ///
    /// 先向上找最近空行作为块边界，再校正代码围栏奇偶性——
    /// 若起点落在未闭合围栏内部，回退到围栏起始行，否则会把代码内容误判成标题/列表。
    fn safe_start(&self, target: usize) -> usize {
        if target == 0 {
            return 0;
        }
        let floor = target.saturating_sub(UP_SCAN);
        let mut cand = floor;
        let mut l = target;
        while l > floor {
            l -= 1;
            if self.is_blank_line(l) {
                cand = l;
                break;
            }
        }
        self.fence_fix(cand)
    }

    /// 校正围栏奇偶性：`cand` 若落在未闭合的代码围栏内部，
    /// 必须回退到围栏起始行，否则会把代码内容误判成标题/列表。
    ///
    /// 这是热路径上最贵的一步（要回看 `FENCE_SCAN` 行），所以按
    /// 「围栏结构版本 + 输入行」记忆结果。普通打字不会 bump `fence_rev`，
    /// 因此连续输入时这里一次都不用重算。
    fn fence_fix(&self, cand: usize) -> usize {
        if let Some((rev, key, val)) = self.fence_cache.get() {
            if rev == self.fence_rev && key == cand {
                return val;
            }
        }

        let fence_from = cand.saturating_sub(FENCE_SCAN);
        let mut last_fence = None;
        let mut count = 0usize;
        for i in fence_from..cand {
            if self.line_starts_fence(i) {
                count += 1;
                last_fence = Some(i);
            }
        }

        let out = match (count % 2 == 1, last_fence) {
            (true, Some(f)) => f,
            _ => cand,
        };
        self.fence_cache.set(Some((self.fence_rev, cand, out)));
        out
    }

    fn safe_end(&self, target: usize) -> usize {
        let max = self.text.len_lines();
        let limit = (target + DOWN_SCAN).min(max);
        let mut l = target.min(max);
        while l < limit {
            if self.is_blank_line(l) {
                return (l + 1).min(max);
            }
            l += 1;
        }
        limit
    }

    /// 解析并返回覆盖 `want` 行区间的块。
    ///
    /// 返回的切片可能包含 `want` 之外的块（解析必须从块边界开始），
    /// 调用方按 `Block::lines` 自行裁剪即可。
    pub fn blocks_for_lines(&mut self, want: Range<usize>) -> &[Block] {
        let max_line = self.text.len_lines();
        let want_start = want.start.min(max_line);
        let want_end = want.end.clamp(want_start, max_line);

        if let Some(c) = &self.cache {
            if c.revision == self.revision
                && c.covered.start <= want_start
                && c.covered.end >= want_end
            {
                return &self.cache.as_ref().unwrap().blocks;
            }
        }

        // 上下留余量，让连续滚动能持续命中缓存（见 VIEW_PAD 注释）
        let pad_start = want_start.saturating_sub(VIEW_PAD);
        let pad_end = (want_end + VIEW_PAD).min(max_line);

        let start = self.safe_start(pad_start);
        let end = self.safe_end(pad_end).max(start);

        let mut lines = Vec::with_capacity(end - start + 1);
        let mut eols = Vec::with_capacity(end - start + 1);
        for i in start..end {
            lines.push(self.line_text(i));
            eols.push(self.line_eol_width(i));
        }

        let blocks = scan_blocks(&LineInput {
            lines: &lines,
            first_line: start,
            first_byte: self.line_to_byte(start),
            eol_widths: &eols,
        });

        self.cache = Some(ViewCache {
            revision: self.revision,
            covered: start..end,
            blocks,
        });
        &self.cache.as_ref().unwrap().blocks
    }

    /// 光标所在块。用于「活动块显示源码、其余块渲染样式」。
    pub fn block_at_byte(&mut self, byte: usize) -> Option<&Block> {
        let line = self.byte_to_line(byte);
        let blocks = self.blocks_for_lines(line..line + 1);
        blocks.iter().find(|b| b.contains_line(line))
    }

    /// 整篇解析。仅用于导出 / 全库索引等离线场景，UI 路径不要调用。
    pub fn blocks_all(&self) -> Vec<Block> {
        let n = self.text.len_lines();
        let mut lines = Vec::with_capacity(n);
        let mut eols = Vec::with_capacity(n);
        for i in 0..n {
            lines.push(self.line_text(i));
            eols.push(self.line_eol_width(i));
        }
        scan_blocks(&LineInput {
            lines: &lines,
            first_line: 0,
            first_byte: 0,
            eol_widths: &eols,
        })
    }
}

/// 这段文本被插入/删除后，代码围栏的分布是否可能变化。
///
/// 围栏只由行首的 ``` / ~~~ 构成，所以只有围栏字符本身、
/// 或改变行数的换行符，才可能影响判定。
/// 判定放宽一点是安全的（多失效一次只是慢），放严则会出错。
#[inline]
fn affects_fences(s: &str) -> bool {
    s.bytes().any(|b| matches!(b, b'`' | b'~' | b'\n' | b'\r'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BlockKind;

    #[test]
    fn open_and_edit_roundtrip() {
        let mut d = Document::from_str("# 标题\n\n正文\n");
        assert!(!d.is_dirty());
        d.insert(0, "x");
        assert!(d.is_dirty());
        assert_eq!(d.rope().to_string(), "x# 标题\n\n正文\n");
    }

    #[test]
    fn byte_offsets_handle_multibyte() {
        let d = Document::from_str("中文abc\n第二行\n");
        // "中文abc" = 3+3+3 = 9 bytes
        assert_eq!(d.byte_to_line_col(9), (0, 9));
        assert_eq!(d.byte_to_line(10), 1);
        // 从 "文" 之后退一个字符应回到 "中" 之后（3 字节），不能切半
        assert_eq!(d.prev_char_boundary(6), 3);
        assert_eq!(d.next_char_boundary(3), 6);
    }

    #[test]
    fn viewport_parse_only_covers_requested_window() {
        let mut src = String::new();
        for i in 0..5000 {
            src.push_str(&format!("段落 {i}\n\n"));
        }
        let mut d = Document::from_str(&src);
        let total_lines = d.len_lines();
        let blocks = d.blocks_for_lines(4000..4050);
        // 关键是"远小于全文"，而不是某个具体数字：
        // 解析窗口 = 视口 60 行 + 上下各 VIEW_PAD，再加 safe_start/safe_end 的对齐余量
        assert!(
            blocks.len() < total_lines / 4,
            "视口解析块数 {} 相对全文 {} 行过多，惰性解析没生效",
            blocks.len(),
            total_lines
        );
        assert!(blocks.iter().any(|b| b.source == "段落 2000"));
    }

    #[test]
    fn fence_cache_survives_plain_typing() {
        // 围栏之后的行：解析起点必须落在围栏内容之外
        let mut src = String::new();
        src.push_str("段首\n\n```rust\nfn a() {}\n```\n\n");
        for i in 0..600 {
            src.push_str(&format!("正文 {i}\n\n"));
        }
        let mut d = Document::from_str(&src);

        let line = d.len_lines() - 4;
        let before = d.blocks_for_lines(line..line + 2).len();
        let fence_rev = d.fence_rev;

        // 普通字符不含 ` ~ 与换行 → 围栏结构没变，缓存应保留
        d.insert(d.len_bytes(), "字");
        assert_eq!(d.fence_rev, fence_rev, "普通打字不应让围栏缓存失效");
        let after = d.blocks_for_lines(line..line + 2).len();
        assert_eq!(before, after);
    }

    #[test]
    fn fence_cache_invalidated_by_structural_edit() {
        let mut d = Document::from_str("a\n\n```\ncode\n```\n\nb\n");
        let rev = d.fence_rev;

        // 插入反引号：围栏分布可能变
        d.insert(0, "`");
        assert_ne!(d.fence_rev, rev, "插入反引号必须让围栏缓存失效");

        let rev2 = d.fence_rev;
        // 插入换行：行号错位，同样是结构性改动
        d.insert(0, "\n");
        assert_ne!(d.fence_rev, rev2, "插入换行必须让围栏缓存失效");

        // 删除含围栏的区间也要失效
        let rev3 = d.fence_rev;
        let s = d.rope().to_string();
        let at = s.find("```").unwrap();
        d.remove(at..at + 3);
        assert_ne!(d.fence_rev, rev3, "删除围栏必须让缓存失效");
    }

    #[test]
    fn typing_after_unclosed_fence_still_parses_as_code() {
        // 未闭合围栏之后的行必须继续算代码，普通打字不能让这个判定失准
        let mut src = String::from("intro\n\n```\n");
        for i in 0..80 {
            src.push_str(&format!("let x{i} = {i};\n"));
        }
        let mut d = Document::from_str(&src);
        let last = d.len_lines().saturating_sub(2);

        let in_fence = |d: &mut Document, line: usize| {
            d.blocks_for_lines(line..line + 1)
                .iter()
                .any(|b| b.contains_line(line) && matches!(b.kind, BlockKind::CodeFence { .. }))
        };

        assert!(in_fence(&mut d, last), "未闭合围栏内的行应判为代码");
        d.insert(d.len_bytes(), "x");
        assert!(in_fence(&mut d, last), "打字后仍应判为代码");
    }

    #[test]
    fn cache_is_reused_within_same_revision() {
        let mut d = Document::from_str("a\n\nb\n\nc\n");
        let rev = d.revision();
        let n1 = d.blocks_for_lines(0..5).len();
        let n2 = d.blocks_for_lines(1..3).len();
        assert_eq!(rev, d.revision());
        assert_eq!(n1, n2, "子区间请求应命中缓存返回同一批块");
    }

    #[test]
    fn cache_invalidated_after_edit() {
        let mut d = Document::from_str("# A\n");
        assert_eq!(
            d.blocks_for_lines(0..1)[0].kind,
            BlockKind::Heading { level: 1 }
        );
        // 把 "# A" 改成 "A"
        d.remove(0..2);
        assert_eq!(d.blocks_for_lines(0..1)[0].kind, BlockKind::Paragraph);
    }

    #[test]
    fn safe_start_backs_out_of_open_code_fence() {
        // 视口落在代码块中间时，必须从围栏起始行开始解析，
        // 否则 "# 注释" 会被误判成标题
        let mut src = String::from("intro\n\n```rust\n");
        for i in 0..40 {
            src.push_str(&format!("// # 注释 {i}\n"));
        }
        src.push_str("```\n\nend\n");
        let mut d = Document::from_str(&src);
        let blocks = d.blocks_for_lines(30..35);
        let hit = blocks
            .iter()
            .find(|b| b.contains_line(32))
            .expect("视口内必须有块");
        assert!(
            matches!(hit.kind, BlockKind::CodeFence { .. }),
            "实际是 {:?}",
            hit.kind
        );
    }

    #[test]
    fn block_at_byte_locates_active_block() {
        let mut d = Document::from_str("# H\n\npara text\n");
        let off = d.line_to_byte(2) + 2;
        let b = d.block_at_byte(off).unwrap();
        assert_eq!(b.source, "para text");
    }

    #[test]
    fn crlf_document_offsets_are_consistent() {
        let mut d = Document::from_str("# H\r\n\r\npara\r\n");
        assert_eq!(d.newline(), Newline::Crlf);
        let blocks = d.blocks_for_lines(0..3).to_vec();
        for b in &blocks {
            assert_eq!(d.slice_to_string(b.bytes.clone()), b.source);
        }
    }

    #[test]
    fn save_and_reopen(
    ) -> Result<()> {
        let dir = std::env::temp_dir().join("fastnote-test");
        std::fs::create_dir_all(&dir)?;
        let p = dir.join("t.md");
        let mut d = Document::from_str("# 保存测试\n\n内容\n");
        d.save_as(&p)?;
        assert!(!d.is_dirty());
        let d2 = Document::open(&p)?;
        assert_eq!(d2.rope().to_string(), "# 保存测试\n\n内容\n");
        std::fs::remove_file(&p).ok();
        Ok(())
    }
}
