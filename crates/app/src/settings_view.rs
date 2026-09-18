//! AI 设置面板。
//!
//! gpui 0.2.2 没有文本输入组件，因此不在这里内嵌编辑 base_url / api_key，
//! 而是提供「一键切换端点预设」的按钮；api_key 通过环境变量
//! `FASTNOTE_API_KEY`（或手动编辑配置文件）提供。配置落盘到
//! `%APPDATA%/fastnote/config.json`，本地 Ollama / LM Studio 无需 key 即可用。

use fastnote_ai::AiConfig;
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton,
    MouseDownEvent, ParentElement, Render, SharedString, Styled, Window, div, px,
    InteractiveElement,
};

use crate::command_palette::Close;
use crate::theme::{Metrics, Theme};
use crate::ui;
use crate::{ApplyPreset, OpenConfigDir};

/// 端点预设：`(显示名, 配置)`
pub fn presets() -> Vec<(SharedString, AiConfig)> {
    let mut v: Vec<(SharedString, AiConfig)> = Vec::new();

    let mut ollama = AiConfig::default();
    ollama.base_url = "http://localhost:11434/v1".into();
    ollama.model = "qwen2.5".into();
    v.push(("Ollama 本地 (http://localhost:11434，免 key)".into(), ollama));

    let mut lmstudio = AiConfig::default();
    lmstudio.base_url = "http://localhost:1234/v1".into();
    lmstudio.model = "local-model".into();
    v.push(("LM Studio 本地 (http://localhost:1234，免 key)".into(), lmstudio));

    let mut deepseek = AiConfig::default();
    deepseek.base_url = "https://api.deepseek.com/v1".into();
    deepseek.model = "deepseek-chat".into();
    v.push(("DeepSeek (需 key)".into(), deepseek));

    let mut openai = AiConfig::default();
    openai.base_url = "https://api.openai.com/v1".into();
    openai.model = "gpt-4o-mini".into();
    v.push(("OpenAI (需 key)".into(), openai));

    let mut moonshot = AiConfig::default();
    moonshot.base_url = "https://api.moonshot.cn/v1".into();
    moonshot.model = "moonshot-v1-8k".into();
    v.push(("Moonshot (需 key)".into(), moonshot));

    v
}

pub struct SettingsView {
    focus: FocusHandle,
    theme: Theme,
    current: AiConfig,
}

impl SettingsView {
    pub fn new(theme: Theme, current: AiConfig, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            theme,
            current,
        }
    }
}

impl EventEmitter<ApplyPreset> for SettingsView {}
impl EventEmitter<OpenConfigDir> for SettingsView {}
impl EventEmitter<Close> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme;
        let list = presets();

        let current = &self.current;
        let cur_base = current.base_url.clone();
        let cur_model = current.model.clone();
        let cur_key = if current.api_key.trim().is_empty() {
            "（未设置）".to_string()
        } else {
            "（已设置）".to_string()
        };
        let local = current.is_local();

        ui::scrim(theme)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_v: &mut SettingsView, _ev: &MouseDownEvent, window, cx| {
                    window.dispatch_action(Box::new(Close), cx)
                }),
            )
            .key_context("Settings")
            .track_focus(&self.focus)
            .child(
                ui::panel(theme, px(536.))
                    // 面板内部不冒泡到遮罩：gpui 的鼠标事件会一路传到祖先，
                    // 不显式停止的话点面板任意位置都会把面板关掉。
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_v: &mut SettingsView, _ev: &MouseDownEvent, _w, cx| {
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
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(ui::title(theme, "AI 设置"))
                                            .child(ui::chip(
                                                theme,
                                                if local { "本地" } else { "远端" },
                                            )),
                                    )
                                    .child(ui::sub(
                                        theme,
                                        format!("端点 {cur_base} · 模型 {cur_model} · key {cur_key}"),
                                    )),
                            )
                            .child(ui::icon_btn(
                                theme,
                                "settings-close",
                                "×",
                                cx.listener(|_v: &mut SettingsView, _ev: &MouseDownEvent, window, cx| {
                                    window.dispatch_action(Box::new(Close), cx)
                                }),
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .p(px(16.))
                            .text_color(theme.text)
                            .child(ui::sub(
                                theme,
                                "选择预设写入配置。api_key 通过环境变量 FASTNOTE_API_KEY 提供，或编辑配置文件。",
                            ))
                            .child(
                                div()
                                    .flex_col()
                                    .gap(px(6.))
                                    .children(list.iter().enumerate().map(|(i, (label, _))| {
                                        div()
                                            .id(i)
                                            .px(px(12.))
                                            .py(px(8.))
                                            .rounded(Metrics::RADIUS_BTN)
                                            .bg(theme.surface)
                                            .text_size(px(13.))
                                            .text_color(theme.text)
                                            .cursor(gpui::CursorStyle::PointingHand)
                                            .hover(|s| s.bg(theme.hover))
                                            .child(label.clone())
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                cx.listener(move |_v: &mut SettingsView, _ev: &MouseDownEvent, _window, cx| {
                                                    cx.emit(ApplyPreset(i))
                                                }),
                                            )
                                    })),
                            )
                            .child(
                                div()
                                    .id("settings-open-dir")
                                    .px(px(12.))
                                    .py(px(8.))
                                    .rounded(Metrics::RADIUS_BTN)
                                    .bg(theme.surface)
                                    .text_size(px(13.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(theme.accent)
                                    .cursor(gpui::CursorStyle::PointingHand)
                                    .hover(|s| s.bg(theme.hover))
                                    .child("打开配置文件所在目录")
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|_v: &mut SettingsView, _ev: &MouseDownEvent, _window, cx| cx.emit(OpenConfigDir)),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme.muted)
                                    .child("Esc 或点击空白处关闭"),
                            ),
                    ),
            )
    }
}
