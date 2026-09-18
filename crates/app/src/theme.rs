//! 配色与排版度量。
//!
//! 设计约束：**不依赖色相区分信息**。
//! 所有语义差异同时用「对比度 + 字重 + 背景块 + 边框」表达，
//! 保证色觉差异用户也能分辨。正文对比度 ≥ 12:1，次要文本 ≥ 7:1。

use gpui::{Hsla, Pixels, px, rgb, rgba};

#[derive(Clone, Copy)]
pub struct Theme {
    /// 编辑区背景
    pub bg: Hsla,
    /// 侧栏 / 状态栏背景
    pub surface: Hsla,
    /// 悬停态背景
    pub hover: Hsla,
    /// 选中项背景（侧栏）
    pub active: Hsla,
    /// 分隔线
    pub border: Hsla,

    /// 正文
    pub text: Hsla,
    /// 标题（比正文更黑，配合字重）
    pub heading: Hsla,
    /// 次要文本：状态栏、行号、路径
    pub muted: Hsla,
    /// 强调色：光标、链接、焦点边框
    pub accent: Hsla,

    /// 选区填充
    pub selection: Hsla,
    /// 行内代码背景
    pub code_bg: Hsla,
    /// 行内代码前景
    pub code_fg: Hsla,
    /// 代码块背景
    pub code_block_bg: Hsla,
    /// 引用块左侧竖条
    pub quote_bar: Hsla,
    /// 引用块文本
    pub quote_fg: Hsla,
    /// 列表标记符
    pub marker: Hsla,
    /// Markdown 标记符本体（活动块里显示的 `**`、`#`）——压低但可见
    pub syntax: Hsla,
    /// AI 续写幽灵文本
    pub ghost: Hsla,
    /// 分割线
    pub rule: Hsla,
    /// 警示（未保存标记等）
    pub warn: Hsla,
    /// 双向链接前景（带下划线强化，不单靠色相）
    pub wikilink: Hsla,
    /// 标签前景
    pub tag: Hsla,
    /// 标签底色（chip 背景，与前景形成对比）
    pub tag_bg: Hsla,
    /// 浮层遮罩（压暗背景，聚焦面板）
    pub scrim: Hsla,
    /// 浮层投影色（只做层次暗示，不承载信息）
    pub shadow: Hsla,
}

impl Theme {
    pub fn light() -> Self {
        Self {
            bg: rgb(0xffffff).into(),
            surface: rgb(0xf3f4f6).into(),
            hover: rgb(0xe6e8ec).into(),
            active: rgb(0xd8e3fb).into(),
            border: rgb(0xd0d4da).into(),

            text: rgb(0x16181d).into(),
            heading: rgb(0x0a0c10).into(),
            muted: rgb(0x555b63).into(),
            accent: rgb(0x0b4fd8).into(),

            selection: rgba(0x0b4fd83a).into(),
            code_bg: rgb(0xeceef2).into(),
            code_fg: rgb(0x8a3a00).into(),
            code_block_bg: rgb(0xf5f6f8).into(),
            quote_bar: rgb(0x0b4fd8).into(),
            quote_fg: rgb(0x2f3540).into(),
            marker: rgb(0x0b4fd8).into(),
            syntax: rgb(0x8b929c).into(),
            ghost: rgb(0x8b929c).into(),
            rule: rgb(0xc7ccd3).into(),
            warn: rgb(0x8a3a00).into(),
            wikilink: rgb(0x6b2bd8).into(),
            tag: rgb(0x0b6b4f).into(),
            tag_bg: rgb(0xe3f3ec).into(),
            scrim: rgba(0x0a0c104d).into(),
            shadow: rgba(0x0a0c1029).into(),
        }
    }

    pub fn dark() -> Self {
        Self {
            bg: rgb(0x15171c).into(),
            surface: rgb(0x1b1e24).into(),
            hover: rgb(0x272b33).into(),
            active: rgb(0x2c3a55).into(),
            border: rgb(0x333841).into(),

            text: rgb(0xeef0f4).into(),
            heading: rgb(0xffffff).into(),
            muted: rgb(0xa2a9b4).into(),
            accent: rgb(0x7aa7ff).into(),

            selection: rgba(0x7aa7ff40).into(),
            code_bg: rgb(0x252932).into(),
            code_fg: rgb(0xf0b37a).into(),
            code_block_bg: rgb(0x1d2027).into(),
            quote_bar: rgb(0x7aa7ff).into(),
            quote_fg: rgb(0xd3d8e0).into(),
            marker: rgb(0x7aa7ff).into(),
            syntax: rgb(0x767d89).into(),
            ghost: rgb(0x767d89).into(),
            rule: rgb(0x3d434d).into(),
            warn: rgb(0xf0b37a).into(),
            wikilink: rgb(0xb98bff).into(),
            tag: rgb(0x5fd6a8).into(),
            tag_bg: rgb(0x1f3a31).into(),
            scrim: rgba(0x0000008c).into(),
            shadow: rgba(0x00000080).into(),
        }
    }
}

/// 排版度量。集中放在这里，避免魔法数字散落在渲染代码里。
pub struct Metrics;

impl Metrics {
    /// 正文字号
    pub const BODY: Pixels = px(15.);
    /// 正文行高倍率
    pub const LINE_HEIGHT: f32 = 1.75;
    /// 编辑区左右内边距
    pub const PAD_X: Pixels = px(56.);
    /// 编辑区上下内边距
    pub const PAD_Y: Pixels = px(28.);
    /// 正文最大宽度（超宽窗口时居中，避免长行难读）
    pub const MAX_WIDTH: Pixels = px(760.);
    /// 侧栏宽度
    pub const SIDEBAR: Pixels = px(240.);
    /// 右侧知识面板宽度（大纲 / 反链 / 标签）
    pub const RIGHT_PANEL: Pixels = px(280.);
    /// 块间距
    pub const BLOCK_GAP: Pixels = px(8.);

    // ---- 组件度量：圆角与行高集中在这里，保证各界面观感一致 ----
    /// 浮层面板圆角
    pub const RADIUS_PANEL: Pixels = px(12.);
    /// 按钮 / 列表项圆角
    pub const RADIUS_BTN: Pixels = px(7.);
    /// 顶栏高度（浮动条本体）
    pub const CHROME_H: Pixels = px(40.);
    /// 列表行高
    pub const ROW_H: Pixels = px(28.);
    /// 浮层顶部留白：面板统一从窗口这个高度往下排，切换浮层时不跳
    pub const OVERLAY_TOP: Pixels = px(96.);

    /// 各级标题字号倍率
    pub fn heading_scale(level: u8) -> f32 {
        match level {
            1 => 1.85,
            2 => 1.5,
            3 => 1.28,
            4 => 1.12,
            5 => 1.02,
            _ => 0.96,
        }
    }
}
