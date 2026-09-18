//! 块级（block-level）Markdown 扫描。
//!
//! 手写按行扫描，而不是直接把整个文档丢给 pulldown-cmark。原因有两个：
//! 1. 需要「行 → 块」的双向映射，才能实现光标所在块回退为源码的即时渲染；
//! 2. 需要能从文档任意位置开始解析（惰性视口解析），pulldown-cmark 只能从头解析。
//!
//! 行内内容仍交给 pulldown-cmark（见 `inline`），兼顾正确性与可控性。

use std::ops::Range;

use crate::inline::{parse_inline, InlineText};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListMarker {
    Bullet,
    /// 有序列表，携带原始序号
    Ordered(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// 空行。保留为独立块，这样光标停在空行时布局不会跳动。
    Blank,
    Heading {
        level: u8,
    },
    Paragraph,
    Quote {
        depth: u8,
    },
    List {
        marker: ListMarker,
        indent: u8,
        /// `- [ ]` / `- [x]` 任务项
        task: Option<bool>,
    },
    CodeFence {
        lang: Option<String>,
        /// 围栏是否正常闭合。未闭合时渲染层仍按代码块处理，但不吞掉后续内容。
        closed: bool,
    },
    /// 分割线 `---` / `***` / `___`
    Rule,
    Table,
    /// 图片块：单独成行的 `![alt](path)`。渲染层据此绘制位图，
    /// 其余块级（标题/引用…）的「剥标记」逻辑对它不适用，故 `inline` 为 `None`。
    Image { src: String },
}

impl BlockKind {
    /// 该块是否需要行内解析（代码块和分割线不需要）。
    fn needs_inline(&self) -> bool {
        !matches!(
            self,
            BlockKind::CodeFence { .. }
                | BlockKind::Rule
                | BlockKind::Blank
                | BlockKind::Image { .. }
        )
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub kind: BlockKind,
    /// 行区间 `[start, end)`
    pub lines: Range<usize>,
    /// 字节区间（不含块末尾换行符）
    pub bytes: Range<usize>,
    /// 块的原始 Markdown 文本（不含末尾换行），行间统一用 `\n` 连接
    pub source: String,
    /// 渲染用的行内结果。代码块 / 分割线为 `None`。
    pub inline: Option<InlineText>,
    /// 每行在构造 `inline` 源文本时从行首剥掉的字节数。
    ///
    /// `inline` 解析的是「剥掉块级标记后的文本」（标题去掉 `## `、
    /// 引用去掉 `> `、列表去掉 `- [ ] `），所以把行内偏移换算回文档位置时
    /// 必须把这段前缀加回来，否则光标一进入块就会横向跳一段距离。
    pub strip: Vec<u32>,
}

impl Block {
    #[inline]
    pub fn contains_byte(&self, off: usize) -> bool {
        // 用闭区间右端，让光标停在块末尾时仍归属该块
        off >= self.bytes.start && off <= self.bytes.end
    }

    #[inline]
    pub fn contains_line(&self, line: usize) -> bool {
        self.lines.contains(&line)
    }

    /// 块内行数。
    #[inline]
    pub fn line_count(&self) -> usize {
        self.lines.end - self.lines.start
    }

    /// 渲染文本偏移 → （块内行号，该行的源码列偏移）。
    ///
    /// 列偏移已加回被剥掉的块级标记前缀，因此可以直接与
    /// `Document::line_to_byte(block.lines.start + k) + col` 组合成文档字节位置。
    /// 这条链路是「点击渲染后的文本，光标精确落到源码对应位置」的实现。
    pub fn inline_to_line_col(&self, rendered: usize) -> (usize, usize) {
        let Some(inline) = &self.inline else {
            return (0, 0);
        };
        let src_off = inline.source_offset(rendered);
        let mut acc = 0usize;
        let mut last = (0usize, 0usize);
        for (k, l) in self.source.split('\n').enumerate() {
            let strip = self.strip.get(k).copied().unwrap_or(0) as usize;
            let stripped_len = l.len().saturating_sub(strip);
            if src_off <= acc + stripped_len {
                return (k, strip + (src_off - acc));
            }
            acc += stripped_len + 1; // 计回连接用的 '\n'
            last = (k, l.len());
        }
        last
    }

    /// （块内行号，源码列偏移）→ 渲染文本偏移。用于在渲染态的块上画选区。
    pub fn line_col_to_inline(&self, line_in_block: usize, col: usize) -> usize {
        let Some(inline) = &self.inline else {
            return 0;
        };
        let mut acc = 0usize;
        for (k, l) in self.source.split('\n').enumerate() {
            let strip = self.strip.get(k).copied().unwrap_or(0) as usize;
            let stripped_len = l.len().saturating_sub(strip);
            if k == line_in_block {
                let c = col.saturating_sub(strip).min(stripped_len);
                return inline.rendered_offset(acc + c);
            }
            acc += stripped_len + 1;
        }
        inline.text.len()
    }

    /// 代码块的正文（剥掉围栏行）。
    pub fn code_body(&self) -> Option<&str> {
        match &self.kind {
            BlockKind::CodeFence { .. } => {
                let mut it = self.source.split_inclusive('\n');
                let first = it.next()?;
                let rest = &self.source[first.len()..];
                // 去掉结尾的闭合围栏行
                let rest = match rest.rfind('\n') {
                    Some(i) => {
                        let last = rest[i + 1..].trim_start();
                        if last.starts_with("```") || last.starts_with("~~~") {
                            &rest[..i]
                        } else {
                            rest
                        }
                    }
                    None => {
                        let t = rest.trim_start();
                        if t.starts_with("```") || t.starts_with("~~~") {
                            ""
                        } else {
                            rest
                        }
                    }
                };
                Some(rest)
            }
            _ => None,
        }
    }

    /// 查询块（`` ```query ``）的正文。非查询块返回 `None`。
    ///
    /// 查询块在编辑态按普通代码块显示源码，离开光标后由渲染层
    /// 用笔记库索引把它替换成实时的结果列表。
    pub fn query_body(&self) -> Option<&str> {
        match &self.kind {
            BlockKind::CodeFence {
                lang: Some(lang), ..
            } if lang.eq_ignore_ascii_case("query") => self.code_body(),
            _ => None,
        }
    }
}

/// 逐行输入。调用方（`Document`）从 Rope 中按需取行喂进来，
/// 因此本模块不依赖 ropey，可独立测试。
pub struct LineInput<'a> {
    /// 每项为一行文本，不含换行符
    pub lines: &'a [String],
    /// `lines[0]` 对应的文档行号
    pub first_line: usize,
    /// `lines[0]` 起始处的文档字节偏移
    pub first_byte: usize,
    /// 各行的换行符字节宽度（`\n` 为 1，`\r\n` 为 2）
    pub eol_widths: &'a [u8],
}

fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

fn heading_level(s: &str) -> Option<u8> {
    let t = s.trim_start();
    let n = t.bytes().take_while(|b| *b == b'#').count();
    if n >= 1 && n <= 6 {
        let rest = &t[n..];
        if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
            return Some(n as u8);
        }
    }
    None
}

fn fence_info(s: &str) -> Option<Option<String>> {
    let t = s.trim_start();
    for marker in ["```", "~~~"] {
        if let Some(rest) = t.strip_prefix(marker) {
            if rest.starts_with(marker.chars().next().unwrap()) && marker == "```" {
                // ```` 也是合法围栏，语言信息取剩余部分
            }
            let lang = rest.trim();
            let lang = lang.split_whitespace().next().unwrap_or("");
            return Some(if lang.is_empty() {
                None
            } else {
                Some(lang.to_string())
            });
        }
    }
    None
}

fn is_fence_close(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with("```") || t.starts_with("~~~")
}

fn is_rule(s: &str) -> bool {
    let t = s.trim();
    if t.len() < 3 {
        return false;
    }
    for c in ['-', '*', '_'] {
        if t.chars().all(|x| x == c || x == ' ') && t.chars().filter(|x| *x == c).count() >= 3 {
            return true;
        }
    }
    false
}

fn quote_depth(s: &str) -> Option<u8> {
    let t = s.trim_start();
    if !t.starts_with('>') {
        return None;
    }
    let mut depth = 0u8;
    let mut rest = t;
    loop {
        match rest.strip_prefix('>') {
            Some(r) => {
                depth = depth.saturating_add(1);
                rest = r.strip_prefix(' ').unwrap_or(r);
            }
            None => break,
        }
    }
    Some(depth)
}

/// 识别列表项，返回 `(marker, 缩进空格数, 任务态, 标记符总长度)`
fn list_item(s: &str) -> Option<(ListMarker, u8, Option<bool>, usize)> {
    let indent = s.len() - s.trim_start().len();
    let t = s.trim_start();

    let (marker, marker_len) = if let Some(rest) = t
        .strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("+ "))
    {
        let _ = rest;
        (ListMarker::Bullet, 2)
    } else {
        let digits = t.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 || digits > 9 {
            return None;
        }
        let after = &t[digits..];
        let sep = if after.starts_with(". ") {
            2
        } else if after.starts_with(") ") {
            2
        } else {
            return None;
        };
        let n: u64 = t[..digits].parse().ok()?;
        (ListMarker::Ordered(n), digits + sep)
    };

    let body = &t[marker_len..];
    let task = if let Some(r) = body.strip_prefix("[ ] ") {
        let _ = r;
        Some(false)
    } else if body.starts_with("[x] ") || body.starts_with("[X] ") {
        Some(true)
    } else {
        None
    };

    Some((marker, indent.min(u8::MAX as usize) as u8, task, marker_len))
}

fn is_table_row(s: &str) -> bool {
    let t = s.trim();
    t.len() >= 2 && t.starts_with('|') && t.contains('|')
}

fn is_table_delim(s: &str) -> bool {
    let t = s.trim();
    if !t.starts_with('|') {
        return false;
    }
    t.chars()
        .all(|c| matches!(c, '|' | '-' | ':' | ' '))
        && t.contains('-')
}

/// 一行是否会打断段落。
fn interrupts_paragraph(s: &str) -> bool {
    is_blank(s)
        || heading_level(s).is_some()
        || fence_info(s).is_some()
        || is_rule(s)
        || quote_depth(s).is_some()
        || list_item(s).is_some()
        || image_src(s).is_some()
}

/// 识别单独一行的图片语法 `![alt](path)`，返回图片地址（path 部分）。
///
/// 仅识别「整行就是图片语法」的情况：图片作为块级元素独占一行，
/// 混在段落文本里的 `![](x)` 不被识别（保持为纯文本/链接）。
/// 这样与即时渲染的「整块渲染/编辑」模型一致，也避免段落被意外拆断。
fn image_src(s: &str) -> Option<String> {
    let t = s.trim();
    let rest = t.strip_prefix("![")?; // 去掉 "!["，剩下 "alt](path)"
    let close = rest.find(']')?; // alt 内的第一个 ']'（简单情况 alt 不含 ']'）
    let after = &rest[close + 1..];
    let path = after.strip_prefix('(')?; // 去掉 '('，剩下 "path)"
    let end = path.find(')')?;
    let path = &path[..end];
    let path = path.trim();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

/// 把若干行扫描成块序列。
pub fn scan_blocks(input: &LineInput<'_>) -> Vec<Block> {
    let LineInput {
        lines,
        first_line,
        first_byte,
        eol_widths,
    } = *input;

    let mut blocks = Vec::with_capacity(lines.len() / 2 + 1);
    // 每行起始字节偏移
    let mut offsets = Vec::with_capacity(lines.len() + 1);
    let mut acc = first_byte;
    for (i, l) in lines.iter().enumerate() {
        offsets.push(acc);
        acc += l.len() + eol_widths.get(i).copied().unwrap_or(1) as usize;
    }
    offsets.push(acc);

    let mut i = 0usize;
    while i < lines.len() {
        let line = &lines[i];
        let start = i;

        let (kind, end) = if is_blank(line) {
            (BlockKind::Blank, i + 1)
        } else if let Some(level) = heading_level(line) {
            (BlockKind::Heading { level }, i + 1)
        } else if let Some(lang) = fence_info(line) {
            // 围栏代码块：吃到闭合围栏为止
            let mut j = i + 1;
            let mut closed = false;
            while j < lines.len() {
                if is_fence_close(&lines[j]) {
                    closed = true;
                    j += 1;
                    break;
                }
                j += 1;
            }
            (BlockKind::CodeFence { lang, closed }, j)
        } else if is_rule(line) {
            (BlockKind::Rule, i + 1)
        } else if let Some(depth) = quote_depth(line) {
            let mut j = i + 1;
            while j < lines.len() && quote_depth(&lines[j]).is_some() {
                j += 1;
            }
            (BlockKind::Quote { depth }, j)
        } else if let Some((marker, indent, task, _)) = list_item(line) {
            // 列表项：本行 + 后续更深缩进的续行（不含新列表项）
            let mut j = i + 1;
            while j < lines.len() {
                let nxt = &lines[j];
                if is_blank(nxt) || list_item(nxt).is_some() || interrupts_paragraph(nxt) {
                    break;
                }
                let nxt_indent = nxt.len() - nxt.trim_start().len();
                if nxt_indent <= indent as usize {
                    break;
                }
                j += 1;
            }
            (
                BlockKind::List {
                    marker,
                    indent,
                    task,
                },
                j,
            )
        } else if is_table_row(line)
            && lines
                .get(i + 1)
                .map(|l| is_table_delim(l))
                .unwrap_or(false)
        {
            let mut j = i + 2;
            while j < lines.len() && is_table_row(&lines[j]) {
                j += 1;
            }
            (BlockKind::Table, j)
        } else if let Some(src) = image_src(line) {
            (BlockKind::Image { src }, i + 1)
        } else {
            // 段落：连续非打断行
            let mut j = i + 1;
            while j < lines.len() && !interrupts_paragraph(&lines[j]) {
                j += 1;
            }
            (BlockKind::Paragraph, j)
        };

        // 拼接块源码（不含末尾换行）
        let mut source = String::new();
        for (k, l) in lines[start..end].iter().enumerate() {
            if k > 0 {
                source.push('\n');
            }
            source.push_str(l);
        }

        // 块末字节 = 最后一行的起始偏移 + 该行长度。
        // 不能用 `offsets[start] + source.len()`：source 用 '\n' 连接，
        // 而 CRLF 文档里每个换行占 2 字节，那样算会短 (行数-1) 个字节。
        let bytes = offsets[start]..(offsets[end - 1] + lines[end - 1].len());

        let (inline, strip) = if kind.needs_inline() {
            let (text, strip) = inline_source(&kind, &source);
            (Some(parse_inline(&text)), strip)
        } else {
            (None, Vec::new())
        };

        blocks.push(Block {
            kind,
            lines: (first_line + start)..(first_line + end),
            bytes,
            source,
            inline,
            strip,
        });

        i = end;
    }

    blocks
}

/// 剥掉块级标记符，得到需要做行内解析的部分。
///
/// 同时返回每行被剥掉的前缀字节数，供 `Block::inline_to_line_col` 把
/// 行内偏移换算回源码位置。所有剥离都只作用于行首（只用 `trim_start`
/// 系列），这保证「剥掉的长度 = 原长 - 剩余长」这个换算成立。
fn inline_source(kind: &BlockKind, source: &str) -> (String, Vec<u32>) {
    let line_count = source.split('\n').count();
    match kind {
        BlockKind::Heading { .. } => {
            let t = source.trim_start().trim_start_matches('#').trim_start();
            (t.to_string(), vec![(source.len() - t.len()) as u32])
        }
        BlockKind::Quote { .. } => {
            let mut strip = Vec::with_capacity(line_count);
            let out = source
                .split('\n')
                .map(|l| {
                    let t = l.trim_start();
                    let t = t.trim_start_matches('>');
                    let t = t.strip_prefix(' ').unwrap_or(t);
                    strip.push((l.len() - t.len()) as u32);
                    t
                })
                .collect::<Vec<_>>()
                .join("\n");
            (out, strip)
        }
        BlockKind::List { task, .. } => {
            let t = source.trim_start();
            let lead = source.len() - t.len();
            let mut cut = match list_item(source) {
                Some((_, _, _, len)) => len,
                None => 0,
            };
            if task.is_some() {
                // 任务框 "[x] " 固定 4 字节
                cut += 4;
            }
            let body = t.get(cut..).unwrap_or("");
            let mut strip = vec![(lead + cut) as u32];
            // 续行原样保留，未剥前缀
            strip.resize(line_count, 0);
            (body.to_string(), strip)
        }
        _ => (source.to_string(), vec![0; line_count]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(src: &str) -> Vec<Block> {
        let lines: Vec<String> = src.split('\n').map(|s| s.to_string()).collect();
        let eols = vec![1u8; lines.len()];
        scan_blocks(&LineInput {
            lines: &lines,
            first_line: 0,
            first_byte: 0,
            eol_widths: &eols,
        })
    }

    #[test]
    fn heading_and_paragraph() {
        let b = scan("# 标题\n\n正文一\n正文二\n");
        assert_eq!(b[0].kind, BlockKind::Heading { level: 1 });
        assert_eq!(b[0].inline.as_ref().unwrap().text, "标题");
        assert_eq!(b[1].kind, BlockKind::Blank);
        assert_eq!(b[2].kind, BlockKind::Paragraph);
        // 连续两行合并为一个段落块
        assert_eq!(b[2].lines, 2..4);
    }

    #[test]
    fn hash_without_space_is_not_heading() {
        let b = scan("#标签不是标题\n");
        assert_eq!(b[0].kind, BlockKind::Paragraph);
    }

    #[test]
    fn code_fence_captures_body_and_lang() {
        let b = scan("```rust\nfn main() {}\n```\n");
        match &b[0].kind {
            BlockKind::CodeFence { lang, closed } => {
                assert_eq!(lang.as_deref(), Some("rust"));
                assert!(closed);
            }
            k => panic!("unexpected {k:?}"),
        }
        assert_eq!(b[0].code_body().unwrap(), "fn main() {}");
        assert!(b[0].inline.is_none(), "代码块不做行内解析");
    }

    #[test]
    fn unclosed_fence_is_marked() {
        let b = scan("```\nstill code\n");
        match &b[0].kind {
            BlockKind::CodeFence { closed, .. } => assert!(!closed),
            k => panic!("unexpected {k:?}"),
        }
    }

    #[test]
    fn markdown_inside_fence_is_not_parsed_as_heading() {
        // 末尾 '\n' 经 split 会多出一个空串行，因而尾部还有一个 Blank 块
        let b = scan("```\n# 这是代码里的井号\n```\ntail\n");
        // 围栏必须把中间的 '# ...' 整行吞进去
        assert!(matches!(b[0].kind, BlockKind::CodeFence { closed: true, .. }));
        assert_eq!(b[0].lines, 0..3);
        assert_eq!(b[1].kind, BlockKind::Paragraph);
        // 关键：整篇不该出现任何标题块
        assert!(
            !b.iter()
                .any(|x| matches!(x.kind, BlockKind::Heading { .. })),
            "围栏内的 # 被误判成标题了"
        );
    }

    #[test]
    fn list_items_bullet_ordered_task() {
        let b = scan("- 一\n- [x] 二\n3. 三\n");
        assert!(matches!(
            b[0].kind,
            BlockKind::List {
                marker: ListMarker::Bullet,
                task: None,
                ..
            }
        ));
        assert!(matches!(
            b[1].kind,
            BlockKind::List {
                task: Some(true),
                ..
            }
        ));
        assert_eq!(b[1].inline.as_ref().unwrap().text, "二");
        assert!(matches!(
            b[2].kind,
            BlockKind::List {
                marker: ListMarker::Ordered(3),
                ..
            }
        ));
    }

    #[test]
    fn quote_merges_consecutive_lines_and_strips_marker() {
        let b = scan("> 引用一\n> 引用二\n\n后面\n");
        assert!(matches!(b[0].kind, BlockKind::Quote { depth: 1 }));
        assert_eq!(b[0].inline.as_ref().unwrap().text, "引用一\n引用二");
    }

    #[test]
    fn rule_vs_setext_like_dashes() {
        assert_eq!(scan("---\n")[0].kind, BlockKind::Rule);
        assert_eq!(scan("***\n")[0].kind, BlockKind::Rule);
        assert_eq!(scan("--\n")[0].kind, BlockKind::Paragraph);
    }

    #[test]
    fn table_detection() {
        let b = scan("| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert_eq!(b[0].kind, BlockKind::Table);
        assert_eq!(b[0].lines, 0..3);
    }

    #[test]
    fn byte_ranges_are_contiguous_and_map_back_to_source() {
        let src = "# H\n\npara\n";
        let b = scan(src);
        for blk in &b {
            assert_eq!(&src[blk.bytes.clone()], blk.source, "字节区间必须能取回原文");
        }
    }

    #[test]
    fn offsets_respect_crlf_width() {
        let lines: Vec<String> = vec!["# H".into(), "p".into()];
        let eols = vec![2u8, 2u8]; // \r\n
        let b = scan_blocks(&LineInput {
            lines: &lines,
            first_line: 0,
            first_byte: 0,
            eol_widths: &eols,
        });
        assert_eq!(b[0].bytes, 0..3);
        // 第二块起点应跳过 "\r\n"
        assert_eq!(b[1].bytes.start, 5);
    }

    #[test]
    fn crlf_multiline_block_byte_range_counts_two_byte_eols() {
        // 段落两行，CRLF。source 用 '\n' 连接（5 字节），
        // 但文档里占 "ab\r\ncd" = 6 字节，bytes.end 必须是 6。
        let lines: Vec<String> = vec!["ab".into(), "cd".into()];
        let eols = vec![2u8, 2u8];
        let b = scan_blocks(&LineInput {
            lines: &lines,
            first_line: 0,
            first_byte: 0,
            eol_widths: &eols,
        });
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].source, "ab\ncd");
        assert_eq!(b[0].bytes, 0..6);
    }

    #[test]
    fn strip_records_heading_prefix() {
        let b = scan("## 标题\n");
        assert_eq!(b[0].strip, vec![3]); // "## "
        assert_eq!(b[0].inline.as_ref().unwrap().text, "标题");
    }

    #[test]
    fn strip_records_quote_prefix_per_line() {
        let b = scan("> 一\n> 二\n");
        assert_eq!(b[0].strip, vec![2, 2]);
    }

    #[test]
    fn strip_records_task_marker() {
        let b = scan("- [x] 完成\n");
        // "- " (2) + "[x] " (4) = 6
        assert_eq!(b[0].strip, vec![6]);
        assert_eq!(b[0].inline.as_ref().unwrap().text, "完成");
    }

    #[test]
    fn inline_offset_maps_back_to_source_column() {
        let b = scan("## 标题内容\n");
        let blk = &b[0];
        let inline = blk.inline.as_ref().unwrap();
        // 渲染文本开头对应源码第 3 字节（"## " 之后）
        assert_eq!(blk.inline_to_line_col(0), (0, 3));
        // 渲染文本末尾对应源码行末
        let (line, col) = blk.inline_to_line_col(inline.text.len());
        assert_eq!(line, 0);
        assert_eq!(col, blk.source.len());
    }

    #[test]
    fn inline_offset_maps_second_quote_line() {
        let b = scan("> 甲\n> 乙\n");
        let blk = &b[0];
        let inline = blk.inline.as_ref().unwrap();
        assert_eq!(inline.text, "甲\n乙");
        // "乙" 在渲染文本中的偏移
        let r = inline.text.find('乙').unwrap();
        let (line, col) = blk.inline_to_line_col(r);
        assert_eq!(line, 1);
        assert_eq!(col, 2); // "> " 之后
    }

    #[test]
    fn inline_line_col_round_trip() {
        let b = scan("> **粗** 体\n");
        let blk = &b[0];
        let inline = blk.inline.as_ref().unwrap();
        for (r, _) in inline.text.char_indices() {
            let (line, col) = blk.inline_to_line_col(r);
            assert_eq!(blk.line_col_to_inline(line, col), r, "rendered {r}");
        }
    }

    #[test]
    fn strip_len_matches_line_count() {
        for src in [
            "# H\n",
            "> a\n> b\n> c\n",
            "- [ ] x\n  续行\n",
            "普通\n第二行\n",
        ] {
            let b = scan(src);
            for blk in &b {
                if blk.inline.is_some() {
                    assert_eq!(
                        blk.strip.len(),
                        blk.source.split('\n').count(),
                        "块 {:?} 的 strip 长度应等于行数",
                        blk.kind
                    );
                }
            }
        }
    }

    #[test]
    fn image_block_is_recognized_and_breaks_paragraph() {
        // 单独成行的图片语法应为 Image 块，且 inline 为 None。
        // 注意 scan 按 '\n' 切行，末尾换行会多出一个空行块（与 rule 测试一致）。
        let b = scan("![封面](cover.png)\n");
        assert_eq!(b.len(), 2);
        match &b[0].kind {
            BlockKind::Image { src } => assert_eq!(src, "cover.png"),
            k => panic!("expected Image block, got {k:?}"),
        }
        assert!(b[0].inline.is_none(), "图片块不做行内解析");
        assert_eq!(b[1].kind, BlockKind::Blank);

        // 图片行应打断前面的段落
        let b = scan("前文\n\n![图](a.png)\n\n后文\n");
        let kinds: Vec<_> = b.iter().map(|x| x.kind.clone()).collect();
        assert!(kinds.iter().any(|k| matches!(k, BlockKind::Image { .. })));
        // 不应出现把图片吞进段落的情况
        assert!(
            !b.iter()
                .any(|x| matches!(x.kind, BlockKind::Paragraph) && x.source.contains("!["))
        );
    }

    #[test]
    fn image_src_parses_path_only() {
        assert_eq!(
            image_src("![alt text](path/to/img.jpg)"),
            Some("path/to/img.jpg".to_string())
        );
        assert_eq!(image_src("正文里有 ![x](y.png) 混排"), None);
        assert_eq!(image_src("![空]()"), None);
        assert_eq!(image_src("[普通链接](z.png)"), None);
    }
}
