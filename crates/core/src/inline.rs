//! 行内（inline）Markdown 解析。
//!
//! 输出「渲染文本 + 不重叠的样式区间」这种扁平结构，而不是嵌套 AST。
//! 原因：GPUI 的 `StyledText` 接受 `(文本, Vec<(Range, HighlightStyle)>)`，
//! 扁平区间可以零转换直接喂给渲染层，省掉一次树遍历。
//!
//! 在通用 Markdown 之上，额外支持两种本地知识库语义：
//! - `[[笔记名]]` / `[[笔记名|显示文本]]`：双向链接（wikilink）
//! - `#标签`：行内标签（frontmatter 之外的轻量标注）

use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// 行内样式标记位。用位运算而非 enum，因为样式可叠加（粗斜体、代码+链接）。
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct SpanStyle(u8);

impl SpanStyle {
    pub const NONE: Self = Self(0);
    pub const BOLD: Self = Self(1 << 0);
    pub const ITALIC: Self = Self(1 << 1);
    pub const CODE: Self = Self(1 << 2);
    pub const STRIKE: Self = Self(1 << 3);
    pub const LINK: Self = Self(1 << 4);
    /// 双向链接 `[[...]]`
    pub const WIKILINK: Self = Self(1 << 5);
    /// 标签 `#tag`
    pub const TAG: Self = Self(1 << 6);

    #[inline]
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[inline]
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    #[inline]
    pub fn is_plain(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for SpanStyle {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// 一段连续同样式的文本在渲染文本中的位置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyledSpan {
    /// 在 `InlineText::text`（渲染文本）中的字节区间。
    pub range: Range<usize>,
    /// 对应的源码字节区间。渲染文本剥掉了 `**`、`` ` `` 等标记，
    /// 所以「点击渲染文本某处 → 光标落在源码哪个字节」必须靠这个映射，
    /// 否则光标一进入块（块回退为源码）位置就会跳。
    pub src: Range<usize>,
    pub style: SpanStyle,
}

/// 一个块的行内解析结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InlineText {
    /// 去掉 Markdown 标记符后的可见文本，供渲染层直接显示。
    pub text: String,
    /// 有序、互不重叠的样式区间（字节偏移，相对 `text`）。
    pub spans: Vec<StyledSpan>,
    /// 链接区间与目标地址。
    pub links: Vec<(Range<usize>, String)>,
    /// 双向链接区间与目标笔记名（去掉 `[[`/`]]` 与 `|` 后的部分）。
    pub wikilinks: Vec<(Range<usize>, String)>,
    /// 标签区间与标签名（不含 `#`）。
    pub tags: Vec<(Range<usize>, String)>,
}

impl InlineText {
    /// 纯文本快路径：整块没有任何 Markdown 标记时跳过解析器，直接构造。
    pub fn plain(text: impl Into<String>) -> Self {
        let text = text.into();
        let spans = if text.is_empty() {
            Vec::new()
        } else {
            vec![StyledSpan {
                range: 0..text.len(),
                src: 0..text.len(),
                style: SpanStyle::NONE,
            }]
        };
        Self {
            text,
            spans,
            links: Vec::new(),
            wikilinks: Vec::new(),
            tags: Vec::new(),
        }
    }

    /// 渲染文本偏移 → 源码偏移。
    ///
    /// 用于鼠标点击：点在渲染后的文本上，要换算回源码位置去放光标。
    /// 落在 span 内部时按区间内相对位置线性映射；纯文本 span 两侧长度相等，
    /// 因此常见情况是精确的。落在 span 之间（标记符被剥掉的位置）时取最近端点。
    pub fn source_offset(&self, rendered: usize) -> usize {
        if self.spans.is_empty() {
            return 0;
        }
        for sp in &self.spans {
            if rendered < sp.range.start {
                // 落在上一个 span 之后、这个 span 之前，说明点在被剥掉的标记符上
                return sp.src.start;
            }
            if rendered <= sp.range.end {
                let off = rendered - sp.range.start;
                let src_len = sp.src.end.saturating_sub(sp.src.start);
                return sp.src.start + off.min(src_len);
            }
        }
        self.spans.last().map(|s| s.src.end).unwrap_or(0)
    }

    /// 源码偏移 → 渲染文本偏移。用于光标离开块时把位置带回渲染视图。
    pub fn rendered_offset(&self, src: usize) -> usize {
        if self.spans.is_empty() {
            return 0;
        }
        for sp in &self.spans {
            if src < sp.src.start {
                return sp.range.start;
            }
            if src <= sp.src.end {
                let off = src - sp.src.start;
                let len = sp.range.end.saturating_sub(sp.range.start);
                return sp.range.start + off.min(len);
            }
        }
        self.text.len()
    }
}

/// 判断源文本是否含有任何可能的行内标记字符。
/// 大部分笔记正文是纯文本，这个检查让常见情况绕过 pulldown-cmark。
#[inline]
fn has_inline_marker(src: &str) -> bool {
    src.bytes().any(|b| {
        matches!(
            b,
            b'*' | b'_' | b'`' | b'[' | b'!' | b'~' | b'<' | b'&' | b'\\' | b'#'
        )
    })
}

/// 行内扫描出的片段：纯文本区间 / 双向链接 / 标签。
#[derive(Clone)]
enum Seg {
    Plain(Range<usize>),
    Wiki {
        range: Range<usize>,
        target: String,
        display: String,
    },
    Tag {
        range: Range<usize>,
        name: String,
    },
}

impl Seg {
    fn start(&self) -> usize {
        match self {
            Seg::Plain(r) => r.start,
            Seg::Wiki { range, .. } => range.start,
            Seg::Tag { range, .. } => range.start,
        }
    }
    fn end(&self) -> usize {
        match self {
            Seg::Plain(r) => r.end,
            Seg::Wiki { range, .. } => range.end,
            Seg::Tag { range, .. } => range.end,
        }
    }
}

/// 把源文本切分为「纯文本片段 + 双向链接 + 标签」三段序列。
///
/// 双向链接 `[[A]]` / `[[A|B]]` 与标签 `#word` 是原子单元，不能被 pulldown-cmark
/// 拆开；它们各自作为整体参与渲染，并且保留完整的源码区间，以便点击时精确映射。
fn tokenize(src: &str) -> Vec<Seg> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let n = chars.len();
    let mut specials: Vec<Seg> = Vec::new();

    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || ('\u{4e00}'..='\u{9fff}').contains(&c);

    let mut i = 0;
    while i < n {
        let (b, c) = chars[i];

        // 双向链接：[[ ... ]]
        if c == '[' && i + 1 < n && chars[i + 1].1 == '[' {
            let mut j = i + 2;
            let mut close = None;
            while j + 1 < n {
                if chars[j].1 == ']' && chars[j + 1].1 == ']' {
                    close = Some(j + 1);
                    break;
                }
                j += 1;
            }
            if let Some(e) = close {
                let inner_start = chars[i + 2].0;
                let inner_end = chars[j].0;
                let inner = &src[inner_start..inner_end];
                let (target, display) = match inner.split_once('|') {
                    Some((t, a)) => {
                        let t = t.trim().to_string();
                        let a = a.trim();
                        let a = if a.is_empty() { t.clone() } else { a.to_string() };
                        (t, a)
                    }
                    None => {
                        let t = inner.trim().to_string();
                        (t.clone(), t)
                    }
                };
                if !target.is_empty() {
                    // e 是第二个 `]` 的字符下标，结束字节需越过它本身
                    let end_byte = chars[e].0 + chars[e].1.len_utf8();
                    specials.push(Seg::Wiki {
                        range: b..end_byte,
                        target,
                        display,
                    });
                    i = e + 1;
                    continue;
                }
            }
        }

        // 标签：# 后接一个及以上词字符（中文也行），且 # 前不能是词字符
        if c == '#' {
            let prev_word = i > 0 && is_word(chars[i - 1].1);
            if !prev_word {
                let mut k = i + 1;
                while k < n && is_word(chars[k].1) {
                    k += 1;
                }
                if k > i + 1 {
                    let name_end = if k < n { chars[k].0 } else { src.len() };
                    let name = &src[chars[i + 1].0..name_end];
                    specials.push(Seg::Tag {
                        range: chars[i].0..name_end,
                        name: name.to_string(),
                    });
                    i = k;
                    continue;
                }
            }
        }

        i += 1;
    }

    // 按起始位置排序后，在特殊片段之间填入纯文本片段
    specials.sort_by_key(|s| s.start());
    let mut segs = Vec::with_capacity(specials.len() + 1);
    let mut cursor = 0;
    for s in &specials {
        if s.start() > cursor {
            segs.push(Seg::Plain(cursor..s.start()));
        }
        segs.push(s.clone());
        cursor = s.end();
    }
    if cursor < src.len() {
        segs.push(Seg::Plain(cursor..src.len()));
    }
    segs
}

/// 解析一段纯文本子串（不含 wiki/tag），把结果追加进 `out`。
/// `base` 是子串在原始 `src` 中的字节起点，用于把 pulldown 给出的子串内偏移
/// 还原成原始源码偏移。
fn parse_sub(out: &mut InlineText, sub: &str, base: usize) {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    let mut style = SpanStyle::NONE;
    let mut stack: Vec<SpanStyle> = Vec::new();
    let mut link_open: Option<(usize, String)> = None;

    // 追加文本并记录样式区间。只有「样式相同 + 渲染文本相邻 + 源码也相邻」才合并：
    // 源码不相邻说明中间有被剥掉的标记符，合并会让 source_offset 映射失真。
    let push = |out: &mut InlineText, s: &str, style: SpanStyle, src: Range<usize>| {
        if s.is_empty() {
            return;
        }
        let start = out.text.len();
        out.text.push_str(s);
        let end = out.text.len();
        match out.spans.last_mut() {
            Some(last)
                if last.style == style && last.range.end == start && last.src.end == src.start =>
            {
                last.range.end = end;
                last.src.end = src.end;
            }
            _ => out.spans.push(StyledSpan {
                range: start..end,
                src,
                style,
            }),
        }
    };

    for (ev, span) in Parser::new_ext(sub, opts).into_offset_iter() {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Strong => {
                    stack.push(style);
                    style.insert(SpanStyle::BOLD);
                }
                Tag::Emphasis => {
                    stack.push(style);
                    style.insert(SpanStyle::ITALIC);
                }
                Tag::Strikethrough => {
                    stack.push(style);
                    style.insert(SpanStyle::STRIKE);
                }
                Tag::Link { dest_url, .. } => {
                    stack.push(style);
                    style.insert(SpanStyle::LINK);
                    link_open = Some((out.text.len(), dest_url.to_string()));
                }
                Tag::Image { dest_url, .. } => {
                    stack.push(style);
                    link_open = Some((out.text.len(), dest_url.to_string()));
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough => {
                    style = stack.pop().unwrap_or(SpanStyle::NONE);
                }
                TagEnd::Link | TagEnd::Image => {
                    if let Some((start, url)) = link_open.take() {
                        let end = out.text.len();
                        if start < end {
                            out.links.push((start..end, url));
                        }
                    }
                    style = stack.pop().unwrap_or(SpanStyle::NONE);
                }
                _ => {}
            },
            Event::Text(t) => push(out, &t, style, base + span.start..base + span.end),
            Event::Code(t) => {
                // Code 事件的 span 含首尾反引号，需要定位内层文本的真实源码位置
                let inner = sub
                    .get(span.clone())
                    .and_then(|s| s.find(t.as_ref()))
                    .map(|i| span.start + i)
                    .unwrap_or(span.start);
                push(
                    out,
                    &t,
                    style | SpanStyle::CODE,
                    base + inner..base + inner + t.len(),
                );
            }
            Event::SoftBreak | Event::HardBreak => push(out, "\n", style, base + span.start..base + span.end),
            Event::InlineHtml(t) | Event::Html(t) => {
                push(out, &t, style, base + span.start..base + span.end)
            }
            _ => {}
        }
    }
}

/// 解析一段行内 Markdown（含双向链接与标签）。
pub fn parse_inline(src: &str) -> InlineText {
    if !has_inline_marker(src) {
        return InlineText::plain(src);
    }

    let mut out = InlineText::default();
    for seg in tokenize(src) {
        match seg {
            Seg::Plain(r) => {
                let sub = &src[r.clone()];
                if has_inline_marker(sub) {
                    // 含 Markdown 标记，仍需走 pulldown；边界空格可能被整段修剪，
                    // 但带标记的片段一般不紧贴 wiki/tag，影响可忽略。
                    parse_sub(&mut out, sub, r.start);
                } else {
                    // 纯文本片段直接原样拷贝，保留段间空格（pulldown 会裁掉整段首尾空白）
                    if !sub.is_empty() {
                        let start = out.text.len();
                        out.text.push_str(sub);
                        let end = out.text.len();
                        out.spans.push(StyledSpan {
                            range: start..end,
                            src: r,
                            style: SpanStyle::NONE,
                        });
                    }
                }
            }
            Seg::Wiki {
                range,
                target,
                display,
            } => {
                let start = out.text.len();
                out.text.push_str(&display);
                let end = out.text.len();
                out.spans.push(StyledSpan {
                    range: start..end,
                    src: range,
                    style: SpanStyle::WIKILINK,
                });
                out.wikilinks.push((start..end, target));
            }
            Seg::Tag { range, name } => {
                let start = out.text.len();
                out.text.push('#');
                out.text.push_str(&name);
                let end = out.text.len();
                out.spans.push(StyledSpan {
                    range: start..end,
                    src: range,
                    style: SpanStyle::TAG,
                });
                out.tags.push((start..end, name));
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styles(t: &InlineText) -> Vec<(&str, u8)> {
        t.spans
            .iter()
            .map(|s| (&t.text[s.range.clone()], s.style.bits()))
            .collect()
    }

    #[test]
    fn plain_text_fast_path() {
        let t = parse_inline("普通一段文字，没有标记");
        assert_eq!(t.text, "普通一段文字，没有标记");
        assert_eq!(t.spans.len(), 1);
        assert!(t.spans[0].style.is_plain());
    }

    #[test]
    fn bold_italic_code() {
        let t = parse_inline("前 **粗** 中 *斜* 后 `code`");
        assert_eq!(t.text, "前 粗 中 斜 后 code");
        let s = styles(&t);
        assert!(s.contains(&("粗", SpanStyle::BOLD.bits())));
        assert!(s.contains(&("斜", SpanStyle::ITALIC.bits())));
        assert!(s.contains(&("code", SpanStyle::CODE.bits())));
    }

    #[test]
    fn nested_bold_italic_merges_flags() {
        let t = parse_inline("***both***");
        assert_eq!(t.text, "both");
        let want = (SpanStyle::BOLD | SpanStyle::ITALIC).bits();
        assert_eq!(t.spans[0].style.bits(), want);
    }

    #[test]
    fn strikethrough() {
        let t = parse_inline("~~删掉~~");
        assert_eq!(t.text, "删掉");
        assert!(t.spans[0].style.contains(SpanStyle::STRIKE));
    }

    #[test]
    fn link_records_url_and_range() {
        let t = parse_inline("看 [文档](https://example.com/a) 吧");
        assert_eq!(t.text, "看 文档 吧");
        assert_eq!(t.links.len(), 1);
        let (range, url) = &t.links[0];
        assert_eq!(&t.text[range.clone()], "文档");
        assert_eq!(url, "https://example.com/a");
    }

    #[test]
    fn adjacent_same_style_spans_are_merged() {
        // "a" 与 "b" 同为无样式，应合并成一个区间而不是两个
        let t = parse_inline("ab **c**");
        assert_eq!(t.spans[0].range, 0..3);
    }

    #[test]
    fn plain_text_source_mapping_is_identity() {
        let src = "普通文字";
        let t = parse_inline(src);
        for i in 0..=src.len() {
            assert_eq!(t.source_offset(i), i, "rendered {i}");
        }
    }

    #[test]
    fn source_offset_maps_inside_bold_precisely() {
        let src = "前 **粗体** 后";
        let t = parse_inline(src);
        assert_eq!(t.text, "前 粗体 后");
        // 粗体「内部」的位置必须精确落回 ** 之内，这是点击定位的核心保证
        let r = t.text.find('体').unwrap();
        assert_eq!(t.source_offset(r), src.find('体').unwrap());
        // 末尾要映射到源码末尾附近，而不是越界
        assert!(t.source_offset(t.text.len()) <= src.len());
    }

    #[test]
    fn source_offset_at_marker_seam_stays_outside_marker() {
        // 渲染文本剥掉了 **，所以「星号前」与「星号后」在渲染上是同一个零宽位置，
        // 无法区分。约定归到前一段的源码末尾 —— 光标停在标记符外侧，
        // 于是在粗体开头输入的文字不会被吸进粗体里（与 Typora 行为一致）。
        let src = "前 **粗体** 后";
        let t = parse_inline(src);
        let seam = t.text.find('粗').unwrap();
        let s = t.source_offset(seam);
        assert_eq!(s, src.find("**").unwrap());
        // 无论落哪一侧，都不能越过标记符跑进粗体正文
        assert!(s <= src.find('粗').unwrap());
    }

    #[test]
    fn code_span_maps_inside_backticks() {
        let src = "跑 `cargo test` 看看";
        let t = parse_inline(src);
        // 同理，测代码区间「内部」的位置
        let r = t.text.find("test").unwrap();
        assert_eq!(t.source_offset(r), src.find("test").unwrap());
    }

    #[test]
    fn source_and_rendered_offsets_round_trip() {
        let src = "a **b** c `d` e";
        let t = parse_inline(src);
        for (i, _) in t.text.char_indices() {
            let s = t.source_offset(i);
            // 源码偏移换回渲染偏移应落在同一位置（标记符位置允许就近取整）
            assert_eq!(t.rendered_offset(s), i, "rendered {i} -> src {s}");
        }
    }

    #[test]
    fn source_offset_never_exceeds_source_len() {
        let src = "**多** *种* `标` ~~记~~ [x](y)";
        let t = parse_inline(src);
        for i in 0..=t.text.len() {
            assert!(t.source_offset(i) <= src.len());
        }
    }

    #[test]
    fn wikilink_parsed_with_target_and_display() {
        let src = "参见 [[项目计划]] 与 [[会议纪要|那次会议]]";
        let t = parse_inline(src);
        assert_eq!(t.text, "参见 项目计划 与 那次会议");
        assert_eq!(t.wikilinks.len(), 2);
        assert_eq!(t.wikilinks[0].1, "项目计划");
        assert_eq!(t.wikilinks[1].1, "会议纪要");
        // 点击 Wikilink span 应映射回 `[[` 起点
        let (range, _) = &t.wikilinks[0];
        let r = range.start;
        let s = t.source_offset(r);
        assert_eq!(&src[s..s + 2], "[[");
    }

    #[test]
    fn tag_parsed_inline() {
        let src = "这是 #重要 的笔记 #todo/2026";
        let t = parse_inline(src);
        // 第二个含 '/'，但当前只取一个词段；这里验证基础标签
        assert!(t.text.contains("#重要"));
        assert!(!t.tags.is_empty());
        assert_eq!(t.tags[0].1, "重要");
    }

    #[test]
    fn wikilink_and_markdown_do_not_conflict() {
        // 普通链接 [x](y) 不应被当成 wikilink
        let t = parse_inline("见 [文档](https://x.com) 与 [[另一篇]]");
        assert_eq!(t.links.len(), 1);
        assert_eq!(t.wikilinks.len(), 1);
        assert_eq!(t.wikilinks[0].1, "另一篇");
    }
}
