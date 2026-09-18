//! 笔记模板：`<库根>/templates/*.md` 下的普通 Markdown，加一层变量替换。
//!
//! 模板刻意不做成"另一套格式"——它就是一个普通笔记文件，可以在编辑器里
//! 直接改，也能被索引、被双链。变量只是 `{{名字}}` 形式的占位符，
//! 替换发生在"新建笔记写入文件"这一步，模板文件本身永远不被改写。
//!
//! 支持的变量：
//!
//! | 变量 | 含义 | 示例 |
//! |---|---|---|
//! | `{{date}}` | 日期 | `2026-09-17` |
//! | `{{time}}` | 时间 | `14:05:33` |
//! | `{{datetime}}` | 日期 + 时间 | `2026-09-17 14:05:33` |
//! | `{{title}}` | 笔记标题（文件名去扩展名） | `项目计划` |
//! | `{{year}}` `{{month}}` `{{day}}` | 日期分段 | `2026` / `09` / `17` |
//! | `{{weekday}}` | 星期 | `周四` |
//! | `{{cursor}}` | 光标落点（由 app 层消费，展开后为空） | |

use std::path::{Path, PathBuf};

use crate::date;

/// 模板目录名（库根下）。
pub const TEMPLATE_DIR: &str = "templates";

/// 光标占位符。展开成空串，app 层用它的**位置**决定新建后光标落在哪。
pub const CURSOR: &str = "{{cursor}}";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    /// 模板名（文件名去扩展名），也是列表里的显示名
    pub name: String,
    pub path: PathBuf,
    pub body: String,
}

/// 变量替换的上下文。时间取一次就不再变，保证一次展开里所有变量一致。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ctx {
    pub date: String,
    pub time: String,
    pub title: String,
}

impl Ctx {
    /// 用系统当前时间构造（默认 UTC+8）。
    pub fn now(title: impl Into<String>) -> Self {
        Self {
            date: date::today(),
            time: date::time_with_offset(date::DEFAULT_OFFSET_MINUTES),
            title: title.into(),
        }
    }

    /// 指定日期构造，供单测与"补记昨天"这类场景使用。
    pub fn on(date: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            date: date.into(),
            time: date::time_with_offset(date::DEFAULT_OFFSET_MINUTES),
            title: title.into(),
        }
    }
}

/// 展开模板正文里的变量。
///
/// 未知变量原样保留 —— 手滑写错时看得见，比悄悄吃掉好排查。
pub fn expand(body: &str, ctx: &Ctx) -> String {
    let (y, m, d) = date::parse_ymd(&ctx.date)
        .map(|(y, m, d)| (format!("{y:04}"), format!("{m:02}"), format!("{d:02}")))
        .unwrap_or_default();

    let mut out = String::with_capacity(body.len() + 64);
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            // 没有闭合，剩下的当普通文本
            out.push_str(&rest[start..]);
            return out;
        };
        let key = after[..end].trim();
        match key {
            "date" => out.push_str(&ctx.date),
            "time" => out.push_str(&ctx.time),
            "datetime" => {
                out.push_str(&ctx.date);
                out.push(' ');
                out.push_str(&ctx.time);
            }
            "title" => out.push_str(&ctx.title),
            "year" => out.push_str(&y),
            "month" => out.push_str(&m),
            "day" => out.push_str(&d),
            "weekday" => out.push_str(&date::weekday_cn(&ctx.date)),
            "cursor" => {}
            _ => {
                out.push_str("{{");
                out.push_str(&after[..end]);
                out.push_str("}}");
            }
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// 展开结果里光标应落的字节偏移。没有 `{{cursor}}` 时返回 `None`。
pub fn cursor_offset(body: &str, ctx: &Ctx) -> Option<usize> {
    let before = body.find(CURSOR)?;
    Some(expand(&body[..before], ctx).len())
}

/// 扫描库里的模板目录。目录不存在时返回空列表（不是错误）。
pub fn load_all(root: &Path) -> Vec<Template> {
    let dir = root.join(TEMPLATE_DIR);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<Template> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter(|e| crate::vault::is_note_path(&e.path()))
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_stem()?.to_string_lossy().to_string();
            let body = std::fs::read_to_string(&path).ok()?;
            Some(Template { name, path, body })
        })
        .collect();
    out.sort_by_key(|t| t.name.to_lowercase());
    out
}

/// 首次使用时写入一份起步模板，让"模板"这个概念有可见的入口。
///
/// 只在 templates 目录**完全不存在**时写入，已有目录不动 ——
/// 用户可能特意删光了模板，不该被我们又塞回去。
pub fn seed_if_missing(root: &Path) -> std::io::Result<Option<PathBuf>> {
    let dir = root.join(TEMPLATE_DIR);
    if dir.exists() {
        return Ok(None);
    }
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("会议记录.md");
    std::fs::write(
        &path,
        "# {{date}} {{weekday}} 会议记录\n\n\
         - 时间：{{time}}\n\
         - 参会：\n\
         - 议题：\n\n\
         ## 结论\n\n{{cursor}}\n\n\
         ## 后续行动\n\n- [ ] \n",
    )?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        Ctx {
            date: "2026-09-17".into(),
            time: "14:05:33".into(),
            title: "项目计划".into(),
        }
    }

    #[test]
    fn expands_all_known_variables() {
        let out = expand(
            "# {{title}} {{date}} {{weekday}} {{year}}/{{month}}/{{day}} {{time}}",
            &ctx(),
        );
        assert_eq!(out, "# 项目计划 2026-09-17 周四 2026/09/17 14:05:33");
    }

    #[test]
    fn datetime_joins_date_and_time() {
        assert_eq!(expand("{{datetime}}", &ctx()), "2026-09-17 14:05:33");
    }

    #[test]
    fn unknown_variables_are_left_alone() {
        // 写错了要看得见，而不是被悄悄替换成空
        assert_eq!(expand("{{nope}} 与 {{date}}", &ctx()), "{{nope}} 与 2026-09-17");
    }

    #[test]
    fn unclosed_braces_do_not_eat_the_rest() {
        assert_eq!(expand("前缀 {{date 后缀", &ctx()), "前缀 {{date 后缀");
    }

    #[test]
    fn cursor_expands_to_empty_and_reports_offset() {
        let body = "abc{{cursor}}def";
        assert_eq!(expand(body, &ctx()), "abcdef");
        assert_eq!(cursor_offset(body, &ctx()), Some(3));
        assert_eq!(cursor_offset("无占位符", &ctx()), None);
    }

    #[test]
    fn cursor_offset_accounts_for_expansion_before_it() {
        // 占位符前面的变量展开后长度会变，偏移必须用展开后的长度算
        let body = "{{date}}{{cursor}}";
        assert_eq!(cursor_offset(body, &ctx()), Some(10));
    }

    #[test]
    fn loads_templates_sorted_and_skips_non_notes() {
        let root = std::env::temp_dir().join("fastnote-tpl-load");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(root.join(TEMPLATE_DIR)).unwrap();
        std::fs::write(root.join(TEMPLATE_DIR).join("会议记录.md"), "A").unwrap();
        std::fs::write(root.join(TEMPLATE_DIR).join("周报.md"), "B").unwrap();
        std::fs::write(root.join(TEMPLATE_DIR).join("logo.png"), "x").unwrap();

        let t = load_all(&root);
        let names: Vec<_> = t.iter().map(|t| t.name.as_str()).collect();
        // 中文按 Unicode 码点排序（会 < 周），这一点与侧栏的排序规则一致
        assert_eq!(names, vec!["会议记录", "周报"]);
        assert_eq!(t[0].body, "A", "中文模板也要能正确读出正文");
    }

    #[test]
    fn missing_template_dir_is_not_an_error() {
        let root = std::env::temp_dir().join("fastnote-tpl-none");
        std::fs::remove_dir_all(&root).ok();
        assert!(load_all(&root).is_empty());
    }

    #[test]
    fn seed_creates_once_and_never_overwrites() {
        let root = std::env::temp_dir().join("fastnote-tpl-seed");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();

        assert!(seed_if_missing(&root).unwrap().is_some());
        assert!(seed_if_missing(&root).unwrap().is_none(), "已存在就不该再写");
        // 用户清空模板后不该被塞回来
        std::fs::remove_file(root.join(TEMPLATE_DIR).join("会议记录.md")).unwrap();
        assert!(seed_if_missing(&root).unwrap().is_none());
    }
}
