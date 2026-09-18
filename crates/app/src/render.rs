//! 块级视觉样式映射：`BlockKind` → 字号 / 缩进 / 底色 / 装饰，
//! 以及行内 `SpanStyle` → GPUI `TextRun`。
//!
//! 这一层只做「样式决策」，不碰布局与绘制，方便单独调整外观。

use fastnote_core::{
    block::{BlockKind, ListMarker},
    inline::{InlineText, SpanStyle},
};
use gpui::{
    Font, FontStyle, FontWeight, Hsla, Pixels, StrikethroughStyle, TextRun, UnderlineStyle, font,
    px,
};

use crate::theme::{Metrics, Theme};

/// 正文字体。Windows 上 Microsoft YaHei UI 同时覆盖拉丁与中日韩字形，
/// 避免中英混排时因缺字回退导致基线跳动。
pub const BODY_FAMILY: &str = "Microsoft YaHei UI";
/// 等宽字体。代码块与行内代码用。
pub const MONO_FAMILY: &str = "Cascadia Mono";

pub fn body_font() -> Font {
    font(BODY_FAMILY)
}

pub fn mono_font() -> Font {
    font(MONO_FAMILY)
}

/// 一个块的视觉参数。
#[derive(Clone)]
pub struct BlockStyle {
    pub font_size: Pixels,
    pub line_height: Pixels,
    pub font: Font,
    pub color: Hsla,
    /// 文本左缩进
    pub indent: Pixels,
    /// 整块底色（代码块）
    pub bg: Option<Hsla>,
    /// 左侧竖条颜色（引用块）
    pub bar: Option<Hsla>,
    /// 块上方额外间距
    pub gap_above: Pixels,
    /// 块下方额外间距
    pub gap_below: Pixels,
    /// 行首装饰（列表符号 / 任务框）。渲染模式下由渲染层单独绘制。
    pub prefix: Option<String>,
    /// 是否画一条水平分割线（Rule）
    pub is_rule: bool,
}

impl BlockStyle {
    fn base(theme: &Theme) -> Self {
        Self {
            font_size: Metrics::BODY,
            line_height: px((f32::from(Metrics::BODY) * Metrics::LINE_HEIGHT).round()),
            font: body_font(),
            color: theme.text,
            indent: px(0.),
            bg: None,
            bar: None,
            gap_above: px(0.),
            gap_below: Metrics::BLOCK_GAP,
            prefix: None,
            is_rule: false,
        }
    }
}

/// 计算块的视觉参数。
///
/// `active` 表示光标是否在该块内。活动块回退为源码显示，
/// 但仍保留字号与缩进，这样光标进出块时布局不跳动 —— 这是即时渲染
/// 体验的关键：**只有标记符的可见性变化，几何位置不变**。
pub fn block_style(kind: &BlockKind, theme: &Theme, active: bool) -> BlockStyle {
    let mut s = BlockStyle::base(theme);
    match kind {
        BlockKind::Blank => {
            s.gap_below = px(0.);
        }
        BlockKind::Heading { level } => {
            let scale = Metrics::heading_scale(*level);
            s.font_size = px((f32::from(Metrics::BODY) * scale).round());
            // 标题行高倍率略收紧，避免大字号时行距过散
            s.line_height = px((f32::from(s.font_size) * 1.45).round());
            s.font = body_font().bold();
            s.color = theme.heading;
            s.gap_above = if *level <= 2 { px(18.) } else { px(12.) };
            s.gap_below = px(6.);
        }
        BlockKind::Paragraph => {}
        BlockKind::Quote { depth } => {
            s.indent = px(16. * (*depth as f32).max(1.));
            s.bar = Some(theme.quote_bar);
            s.color = theme.quote_fg;
        }
        BlockKind::List {
            marker,
            indent,
            task,
        } => {
            s.indent = px(22. + 20. * (*indent as f32));
            s.gap_below = px(2.);
            // 活动块显示源码（含 `- ` / `1. ` / `[x]`），不再叠加渲染用的前缀
            if !active {
                s.prefix = Some(match (task, marker) {
                    (Some(true), _) => "☑".into(),
                    (Some(false), _) => "☐".into(),
                    (None, ListMarker::Bullet) => "•".into(),
                    (None, ListMarker::Ordered(n)) => format!("{n}."),
                });
            }
        }
        BlockKind::CodeFence { .. } => {
            s.font = mono_font();
            s.font_size = px((f32::from(Metrics::BODY) - 1.).round());
            s.line_height = px((f32::from(s.font_size) * 1.6).round());
            s.bg = Some(theme.code_block_bg);
            s.indent = px(12.);
            s.gap_above = px(10.);
            s.gap_below = px(10.);
        }
        BlockKind::Rule => {
            s.is_rule = true;
            s.gap_above = px(12.);
            s.gap_below = px(12.);
        }
        BlockKind::Table => {
            s.font = mono_font();
            s.font_size = px((f32::from(Metrics::BODY) - 1.).round());
            s.line_height = px((f32::from(s.font_size) * 1.7).round());
        }
        BlockKind::Image { .. } => {
            // 图片块自身由位图精灵绘制，文本样式只用于占位/退化时的源码显示
            s.gap_above = px(8.);
            s.gap_below = px(12.);
        }
    }
    s
}

/// 把行内样式区间转成 GPUI 的 `TextRun` 序列。
///
/// `text` 是渲染文本，`inline.spans` 覆盖它的全部字节；
/// 未被 span 覆盖的空隙补一个默认 run，保证 `sum(run.len) == text.len()`
/// —— GPUI 的整形要求 run 长度和与文本长度严格相等，否则 panic。
pub fn inline_runs(
    inline: &InlineText,
    range: std::ops::Range<usize>,
    base: &BlockStyle,
    theme: &Theme,
) -> Vec<TextRun> {
    let mut runs: Vec<TextRun> = Vec::with_capacity(inline.spans.len() + 2);
    let mut at = range.start;

    let push = |runs: &mut Vec<TextRun>, len: usize, style: SpanStyle| {
        if len == 0 {
            return;
        }
        runs.push(styled_run(len, style, base, theme));
    };

    for sp in &inline.spans {
        if sp.range.end <= range.start {
            continue;
        }
        if sp.range.start >= range.end {
            break;
        }
        let s = sp.range.start.max(range.start);
        let e = sp.range.end.min(range.end);
        if s > at {
            push(&mut runs, s - at, SpanStyle::NONE);
        }
        push(&mut runs, e - s, sp.style);
        at = e;
    }
    if at < range.end {
        push(&mut runs, range.end - at, SpanStyle::NONE);
    }
    if runs.is_empty() && !range.is_empty() {
        push(&mut runs, range.len(), SpanStyle::NONE);
    }
    runs
}

fn styled_run(len: usize, style: SpanStyle, base: &BlockStyle, theme: &Theme) -> TextRun {
    let mut f = base.font.clone();
    let mut color = base.color;
    let mut bg = None;

    if style.contains(SpanStyle::CODE) {
        f = mono_font();
        color = theme.code_fg;
        bg = Some(theme.code_bg);
    }
    if style.contains(SpanStyle::BOLD) {
        f.weight = FontWeight::BOLD;
    }
    if style.contains(SpanStyle::ITALIC) {
        f.style = FontStyle::Italic;
    }
    if style.contains(SpanStyle::LINK) {
        color = theme.accent;
    }
    if style.contains(SpanStyle::WIKILINK) {
        color = theme.wikilink;
    }
    if style.contains(SpanStyle::TAG) {
        color = theme.tag;
        bg = Some(theme.tag_bg);
    }

    TextRun {
        len,
        font: f,
        color,
        background_color: bg,
        underline: style
            .contains(SpanStyle::LINK)
            .then(|| UnderlineStyle {
                thickness: px(1.),
                color: Some(theme.accent),
                wavy: false,
            })
            .or_else(|| {
                style.contains(SpanStyle::WIKILINK).then(|| UnderlineStyle {
                    thickness: px(1.),
                    color: Some(theme.wikilink),
                    wavy: false,
                })
            }),
        strikethrough: style.contains(SpanStyle::STRIKE).then(|| StrikethroughStyle {
            thickness: px(1.5),
            color: Some(theme.muted),
        }),
    }
}

/// 活动块的源码 run：把行首的结构标记（`#`、`>`、`-`、`1.`）压成弱色，
/// 正文保持正常色。这样编辑时既能看到原始语法，视觉重心又还在内容上。
pub fn source_runs(line: &str, base: &BlockStyle, theme: &Theme) -> Vec<TextRun> {
    let marker_len = leading_marker_len(line);
    let mut runs = Vec::with_capacity(2);
    if marker_len > 0 {
        runs.push(TextRun {
            len: marker_len,
            font: base.font.clone(),
            color: theme.syntax,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    if marker_len < line.len() {
        runs.push(TextRun {
            len: line.len() - marker_len,
            font: base.font.clone(),
            color: base.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    runs
}

/// 行首结构标记的字节长度（含其后的空格）。
fn leading_marker_len(line: &str) -> usize {
    let b = line.as_bytes();
    let mut i = 0;
    // 前导空格 / 制表符属于缩进，不算标记
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    let start = i;

    // ATX 标题
    if b.get(i) == Some(&b'#') {
        let mut j = i;
        while j < b.len() && b[j] == b'#' && j - i < 6 {
            j += 1;
        }
        if b.get(j) == Some(&b' ') {
            return j + 1;
        }
    }
    // 引用（可嵌套）
    if b.get(i) == Some(&b'>') {
        let mut j = i;
        while j < b.len() && (b[j] == b'>' || b[j] == b' ') {
            j += 1;
        }
        return j;
    }
    // 无序列表
    if matches!(b.get(i), Some(&b'-') | Some(&b'*') | Some(&b'+'))
        && b.get(i + 1) == Some(&b' ')
    {
        let mut j = i + 2;
        // 任务框
        if b.get(j) == Some(&b'[')
            && matches!(b.get(j + 1), Some(&b' ') | Some(&b'x') | Some(&b'X'))
            && b.get(j + 2) == Some(&b']')
        {
            j += 3;
            if b.get(j) == Some(&b' ') {
                j += 1;
            }
        }
        return j;
    }
    // 有序列表
    let mut j = i;
    while j < b.len() && b[j].is_ascii_digit() {
        j += 1;
    }
    if j > i && matches!(b.get(j), Some(&b'.') | Some(&b')')) && b.get(j + 1) == Some(&b' ') {
        return j + 2;
    }
    // 代码围栏
    if line[start..].starts_with("```") || line[start..].starts_with("~~~") {
        return line.len();
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_len_heading() {
        assert_eq!(leading_marker_len("## 标题"), 3);
        assert_eq!(leading_marker_len("#不是标题"), 0);
    }

    #[test]
    fn marker_len_list_and_task() {
        assert_eq!(leading_marker_len("- 项"), 2);
        assert_eq!(leading_marker_len("- [ ] 待办"), 6);
        assert_eq!(leading_marker_len("- [x] 完成"), 6);
        assert_eq!(leading_marker_len("12. 项"), 4);
    }

    #[test]
    fn marker_len_quote_and_fence() {
        assert_eq!(leading_marker_len("> 引用"), 2);
        assert_eq!(leading_marker_len(">> 深"), 3);
        assert_eq!(leading_marker_len("```rust"), 7);
    }

    #[test]
    fn marker_len_plain() {
        assert_eq!(leading_marker_len("普通段落"), 0);
        assert_eq!(leading_marker_len(""), 0);
    }

    #[test]
    fn inline_runs_cover_whole_range() {
        let inline = InlineText::plain("hello world");
        let theme = Theme::light();
        let base = BlockStyle::base(&theme);
        let runs = inline_runs(&inline, 0..inline.text.len(), &base, &theme);
        let total: usize = runs.iter().map(|r| r.len).sum();
        assert_eq!(total, inline.text.len());
    }

    #[test]
    fn inline_runs_sub_range_covers_exactly() {
        let inline = fastnote_core::inline::parse_inline("a **bold** c");
        let theme = Theme::light();
        let base = BlockStyle::base(&theme);
        let r = 2..inline.text.len() - 1;
        let runs = inline_runs(&inline, r.clone(), &base, &theme);
        let total: usize = runs.iter().map(|x| x.len).sum();
        assert_eq!(total, r.len());
    }

    #[test]
    fn source_runs_cover_whole_line() {
        let theme = Theme::light();
        let base = BlockStyle::base(&theme);
        for line in ["## 标题", "- [ ] 待办", "普通", ""] {
            let runs = source_runs(line, &base, &theme);
            let total: usize = runs.iter().map(|r| r.len).sum();
            assert_eq!(total, line.len(), "line = {line:?}");
        }
    }
}
