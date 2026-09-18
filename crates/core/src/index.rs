//! 笔记库索引：在本地笔记库之上提供知识图谱所需的查询能力。
//!
//! 与 `Vault`（只管文件树）不同，`VaultIndex` 扫描全部笔记正文，建立：
//! - 双向链接：`[[笔记名]]` 的出链与反链
//! - 标签：`#标签` 与 frontmatter `tags:` 汇总
//! - 全文搜索：库内模糊/子串匹配，返回行号与上下文片段
//!
//! 索引是按需重建的（成本与笔记数线性相关，中等规模库是毫秒级），
//! 不常驻内存、不引入额外依赖，契合「本地优先 + 极速」的定位。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::vault::Vault;

/// 单篇笔记的分析结果。
#[derive(Clone)]
pub struct NoteMeta {
    pub path: PathBuf,
    /// 显示名（文件名去扩展名）
    pub name: String,
    /// 提取到的全部标签（去重，不含 `#`）
    pub tags: Vec<String>,
    /// 出链目标原始文本（去重）
    pub outgoing: Vec<String>,
}

/// 一条反链 / 出链引用。
#[derive(Clone, Debug)]
pub struct LinkRef {
    pub source: PathBuf,
    pub source_name: String,
    /// 该链接在来源笔记中的行号（0 基）
    pub line: usize,
    /// 来源行文本（去掉行尾空白）
    pub snippet: String,
}

/// 一条搜索命中。
#[derive(Clone, Debug)]
pub struct SearchHit {
    pub path: PathBuf,
    pub name: String,
    pub line: usize,
    pub snippet: String,
}

/// 笔记库索引。
#[derive(Clone)]
pub struct VaultIndex {
    notes: Vec<NoteMeta>,
    by_stem: HashMap<String, PathBuf>,
    by_path: HashMap<PathBuf, usize>,
}

impl VaultIndex {
    /// 扫描整个笔记库构建索引。
    pub fn build(vault: &Vault) -> Self {
        let mut notes = Vec::new();
        let mut by_stem = HashMap::new();
        let mut by_path = HashMap::new();

        for path in vault.walk_notes() {
            let meta = analyze(&path);
            if let Some(name) = meta.as_ref().map(|m| m.name.clone()) {
                by_stem
                    .entry(name.to_lowercase())
                    .or_insert_with(|| path.clone());
            }
            if meta.is_some() {
                by_path.insert(path.clone(), notes.len());
            }
            if let Some(m) = meta {
                notes.push(m);
            }
        }

        Self {
            notes,
            by_stem,
            by_path,
        }
    }

    /// 重建索引（笔记增删或内容变化后调用）。
    pub fn rebuild(&mut self, vault: &Vault) {
        *self = Self::build(vault);
    }

    pub fn notes(&self) -> &[NoteMeta] {
        &self.notes
    }

    /// 把 `[[目标]]` 解析到实际文件路径。
    ///
    /// Obsidian 语义：目标通常是文件名（去扩展名），大小写不敏感；
    /// 若含 `/` 则按路径后缀匹配（忽略大小写）。
    pub fn resolve(&self, target: &str) -> Option<&Path> {
        let t = target.trim();
        if t.is_empty() {
            return None;
        }
        // 去掉可能的扩展名，统一以词干比较
        let t_stem = t
            .rsplit_once('.')
            .map(|(s, ext)| {
                if matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdx" | "txt") {
                    s
                } else {
                    t
                }
            })
            .unwrap_or(t);
        let t_lower = t_stem.to_lowercase();

        // 含路径分隔：尝试后缀匹配
        if t_lower.contains('/') {
            let best = self
                .notes
                .iter()
                .filter(|n| {
                    n.path
                        .to_string_lossy()
                        .to_lowercase()
                        .ends_with(&t_lower)
                })
                .min_by_key(|n| n.path.components().count());
            if let Some(n) = best {
                return Some(&n.path);
            }
        }

        self.by_stem.get(&t_lower).map(|p| p.as_path())
    }

    /// 指向 `path` 的全部反链。
    pub fn backlinks(&self, path: &Path) -> Vec<LinkRef> {
        let mut refs = Vec::new();
        for note in &self.notes {
            if note.path == path {
                continue;
            }
            for target in &note.outgoing {
                if let Some(dest) = self.resolve(target) {
                    if dest == path {
                        // 收集该笔记中指向 path 的行
                        if let Ok(content) = std::fs::read_to_string(&note.path) {
                            for (i, line) in content.lines().enumerate() {
                                if line_contains_wikilink(line, target) {
                                    refs.push(LinkRef {
                                        source: note.path.clone(),
                                        source_name: note.name.clone(),
                                        line: i,
                                        snippet: line.trim().to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        refs
    }

    /// `path` 指向的笔记的标题（去扩展名），便于面板显示。
    pub fn name_of(&self, path: &Path) -> String {
        self.by_path
            .get(path)
            .and_then(|&i| self.notes.get(i))
            .map(|n| n.name.clone())
            .unwrap_or_else(|| path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())
    }

    /// 全部标签及其出现次数，按次数降序、同名按字典序。
    pub fn all_tags(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for n in &self.notes {
            for tag in &n.tags {
                *counts.entry(tag.clone()).or_insert(0) += 1;
            }
        }
        let mut v: Vec<_> = counts.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }

    /// 库内全文搜索（大小写不敏感子串匹配）。
    /// 每个笔记最多返回 `max_per_note` 条命中，总命中数不超过 `max_total`。
    pub fn search(&self, query: &str, max_per_note: usize, max_total: usize) -> Vec<SearchHit> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for note in &self.notes {
            // 标题命中优先展示
            if note.name.to_lowercase().contains(&q) {
                hits.push(SearchHit {
                    path: note.path.clone(),
                    name: note.name.clone(),
                    line: 0,
                    snippet: note.name.clone(),
                });
                if hits.len() >= max_total {
                    return hits;
                }
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&note.path) {
                let mut per = 0;
                for (i, line) in content.lines().enumerate() {
                    if line.to_lowercase().contains(&q) {
                        hits.push(SearchHit {
                            path: note.path.clone(),
                            name: note.name.clone(),
                            line: i,
                            snippet: line.trim().to_string(),
                        });
                        per += 1;
                        if hits.len() >= max_total || per >= max_per_note {
                            break;
                        }
                    }
                }
            }
            if hits.len() >= max_total {
                break;
            }
        }
        hits
    }

    /// 执行查询块（` ```query `）：按标签 / 关键词 / 链接约束筛选笔记。
    ///
    /// 三类条件之间是「与」关系；关键词做大小写不敏感子串匹配，
    /// 命中行取自第一个关键词在正文中的位置。
    pub fn query(&self, spec: &QuerySpec) -> Vec<SearchHit> {
        if spec.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for note in &self.notes {
            // 标签：全部满足
            let tags_ok = spec
                .tags
                .iter()
                .all(|t| note.tags.iter().any(|x| x.eq_ignore_ascii_case(t)));
            if !tags_ok {
                continue;
            }
            // 链接：必须链接到指定笔记
            if !spec.links.iter().all(|l| self.links_to(note, l)) {
                continue;
            }

            let content = if spec.terms.is_empty() {
                None
            } else {
                std::fs::read_to_string(&note.path).ok()
            };
            let lower = content.as_ref().map(|c| c.to_lowercase());
            let name_lower = note.name.to_lowercase();

            let mut hit_line = 0usize;
            let mut ok = true;
            for (i, t) in spec.terms.iter().enumerate() {
                let t = t.to_lowercase();
                match lower.as_ref().and_then(|c| c.find(&t)) {
                    Some(p) => {
                        if i == 0 {
                            // 字节位置 → 行号
                            let c = content.as_ref().unwrap();
                            hit_line = c[..p].lines().count().saturating_sub(1);
                        }
                    }
                    None => {
                        // 正文没命中就退回文件名匹配
                        if !name_lower.contains(&t) {
                            ok = false;
                            break;
                        }
                    }
                }
            }
            if !ok {
                continue;
            }

            let snippet = content
                .as_ref()
                .and_then(|c| c.lines().nth(hit_line))
                .map(|l| l.trim().to_string())
                .unwrap_or_default();
            out.push(SearchHit {
                path: note.path.clone(),
                name: note.name.clone(),
                line: hit_line,
                snippet: if snippet.is_empty() {
                    note.name.clone()
                } else {
                    snippet
                },
            });
            if out.len() >= spec.limit {
                break;
            }
        }
        out
    }

    /// `note` 是否链接到 `target`（走与 `resolve` 相同的解析规则）。
    fn links_to(&self, note: &NoteMeta, target: &str) -> bool {
        let dest = self.resolve(target);
        note.outgoing.iter().any(|o| match dest {
            Some(d) => self.resolve(o) == Some(d),
            None => o.eq_ignore_ascii_case(target),
        })
    }

    /// 构建关系图谱：节点是笔记，边是能解析到实际笔记的 `[[双向链接]]`。
    ///
    /// 布局用的是 Fruchterman-Reingold 力导向的简化版：固定迭代次数，
    /// 无随机数，因此对同一份索引每次都会得到完全相同的坐标——
    /// 面板重绘时不会抖动，也能写断言测试。
    ///
    /// 坐标归一化到 `[0, 1]`，具体像素尺寸由 UI 决定；
    /// 节点数超过 `max_nodes` 时按度数（连接数）从高到低截断。
    pub fn graph(&self, max_nodes: usize) -> Graph {
        let total = self.notes.len();
        if total == 0 {
            return Graph::default();
        }

        let slot_of: HashMap<&Path, usize> = self
            .notes
            .iter()
            .enumerate()
            .map(|(i, m)| (m.path.as_path(), i))
            .collect();

        // 1) 先在全量笔记上建边，才能用度数挑出"重要"的子图
        let mut raw: Vec<(usize, usize)> = Vec::new();
        let mut degree = vec![0usize; total];
        let mut seen = std::collections::HashSet::new();
        for (i, m) in self.notes.iter().enumerate() {
            for target in &m.outgoing {
                let Some(&j) = self.resolve(target).and_then(|p| slot_of.get(p)) else {
                    continue;
                };
                if i == j {
                    continue;
                }
                let key = if i < j { (i, j) } else { (j, i) };
                if !seen.insert(key) {
                    continue;
                }
                raw.push((i, j));
                degree[i] += 1;
                degree[j] += 1;
            }
        }

        // 2) 选出要画的节点：度数高的优先，同分按名称排序保证确定性
        let mut picked: Vec<usize> = (0..total).collect();
        picked.sort_by(|&a, &b| {
            degree[b]
                .cmp(&degree[a])
                .then_with(|| self.notes[a].name.cmp(&self.notes[b].name))
        });
        picked.truncate(max_nodes.max(1).min(total));
        // 再按名称排一次，让节点下标与索引内部顺序无关
        picked.sort_by(|&a, &b| self.notes[a].name.cmp(&self.notes[b].name));

        let new_slot: HashMap<usize, usize> = picked
            .iter()
            .enumerate()
            .map(|(n, &old)| (old, n))
            .collect();
        let edges: Vec<GraphEdge> = raw
            .into_iter()
            .filter_map(|(i, j)| match (new_slot.get(&i), new_slot.get(&j)) {
                (Some(&a), Some(&b)) => Some(GraphEdge { from: a, to: b }),
                _ => None,
            })
            .collect();

        // 3) 力导向布局
        let n = picked.len();
        let mut pos: Vec<(f32, f32)> = (0..n)
            .map(|i| {
                // 起始摆成一个圆，避免所有点从同一处出发导致斥力方向退化
                let a = i as f32 * std::f32::consts::TAU / n as f32;
                (0.5 + 0.34 * a.cos(), 0.5 + 0.34 * a.sin())
            })
            .collect();

        if n > 1 {
            let k = (1.0f32 / n as f32).sqrt();
            let iters = if n > 120 { 80 } else { 140 };
            let mut disp = vec![(0.0f32, 0.0f32); n];

            for it in 0..iters {
                for d in disp.iter_mut() {
                    *d = (0.0, 0.0);
                }

                // 斥力：所有点两两相斥
                for a in 0..n {
                    for b in (a + 1)..n {
                        let (mut dx, mut dy) = (pos[a].0 - pos[b].0, pos[a].1 - pos[b].1);
                        let mut dist = (dx * dx + dy * dy).sqrt();
                        if dist < 1e-4 {
                            // 完全重合时给一个确定性的小扰动，方向随下标变化
                            dx = 0.001 * (a as f32 + 1.0);
                            dy = 0.001;
                            dist = (dx * dx + dy * dy).sqrt();
                        }
                        let f = k * k / dist;
                        let (ux, uy) = (dx / dist, dy / dist);
                        disp[a].0 += ux * f;
                        disp[a].1 += uy * f;
                        disp[b].0 -= ux * f;
                        disp[b].1 -= uy * f;
                    }
                }

                // 引力：有链接的点互相靠近
                for e in &edges {
                    let (mut dx, mut dy) =
                        (pos[e.from].0 - pos[e.to].0, pos[e.from].1 - pos[e.to].1);
                    let mut dist = (dx * dx + dy * dy).sqrt();
                    if dist < 1e-4 {
                        dx = 0.001;
                        dy = 0.001;
                        dist = (dx * dx + dy * dy).sqrt();
                    }
                    let f = dist * dist / k * 0.5;
                    let (ux, uy) = (dx / dist, dy / dist);
                    disp[e.from].0 -= ux * f;
                    disp[e.from].1 -= uy * f;
                    disp[e.to].0 += ux * f;
                    disp[e.to].1 += uy * f;
                }

                // 位移：限幅降温 + 轻微向心，防止孤立点漂出画布
                let t = 0.12 * (1.0 - it as f32 / iters as f32) + 0.004;
                for i in 0..n {
                    let (dx, dy) = disp[i];
                    let len = (dx * dx + dy * dy).sqrt();
                    if len > 1e-6 {
                        let step = len.min(t);
                        pos[i].0 += dx / len * step;
                        pos[i].1 += dy / len * step;
                    }
                    pos[i].0 += (0.5 - pos[i].0) * 0.01;
                    pos[i].1 += (0.5 - pos[i].1) * 0.01;
                    pos[i].0 = pos[i].0.clamp(0.06, 0.94);
                    pos[i].1 = pos[i].1.clamp(0.06, 0.94);
                }
            }
        }

        Graph {
            nodes: picked
                .iter()
                .enumerate()
                .map(|(i, &old)| GraphNode {
                    path: self.notes[old].path.clone(),
                    name: self.notes[old].name.clone(),
                    x: pos[i].0,
                    y: pos[i].1,
                    degree: degree[old],
                })
                .collect(),
            edges,
        }
    }
}

/// 图谱节点：一篇笔记及其归一化坐标。
#[derive(Clone, Debug, PartialEq)]
pub struct GraphNode {
    pub path: PathBuf,
    pub name: String,
    /// 归一化坐标，范围 `[0, 1]`
    pub x: f32,
    pub y: f32,
    /// 连接数（出链 + 入链去重后），用于决定节点大小
    pub degree: usize,
}

/// 图谱中的一条边，`from` / `to` 是 [`Graph::nodes`] 的下标。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphEdge {
    pub from: usize,
    pub to: usize,
}

/// 笔记关系图谱。
#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// 查询块（` ```query `）的解析结果。
#[derive(Clone, Debug, Default)]
pub struct QuerySpec {
    /// 标签过滤，全部满足（AND）
    pub tags: Vec<String>,
    /// 全文关键词，全部满足（AND）
    pub terms: Vec<String>,
    /// 链接约束：结果必须链接到这些笔记
    pub links: Vec<String>,
    /// 最多返回条数
    pub limit: usize,
}

impl QuerySpec {
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.terms.is_empty() && self.links.is_empty()
    }
}

/// 解析查询块正文。
///
/// 语法（空白分隔，双引号内可含空格）：
/// - `#标签` —— 标签过滤
/// - `link:目标` —— 只保留链接到该笔记的笔记
/// - `limit:20` —— 结果条数上限（默认 20）
/// - 其余 —— 全文关键词
pub fn parse_query(body: &str) -> QuerySpec {
    let mut spec = QuerySpec {
        limit: 20,
        ..Default::default()
    };
    for tok in split_tokens(body) {
        if let Some(rest) = tok.strip_prefix('#') {
            if !rest.is_empty() {
                spec.tags.push(rest.to_string());
            }
        } else if let Some(rest) = tok.strip_prefix("link:") {
            let t = rest.trim_matches(|c| c == '[' || c == ']').trim();
            if !t.is_empty() {
                spec.links.push(t.to_string());
            }
        } else if let Some(rest) = tok.strip_prefix("limit:") {
            if let Ok(n) = rest.parse::<usize>() {
                spec.limit = n.clamp(1, 200);
            }
        } else if !tok.is_empty() {
            spec.terms.push(tok);
        }
    }
    spec
}

/// 按空白切分，支持中英文双引号包裹的短语。
fn split_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    for c in s.chars() {
        match c {
            '"' | '\u{201c}' | '\u{201d}' => in_q = !in_q,
            c if c.is_whitespace() && !in_q => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 分析单篇笔记：提取标签与出链。
fn analyze(path: &Path) -> Option<NoteMeta> {
    let content = std::fs::read_to_string(path).ok()?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut tags = Vec::new();
    let mut outgoing = Vec::new();

    // frontmatter 标签
    let fm_tags = parse_frontmatter_tags(&content);
    tags.extend(fm_tags);

    // 逐行扫描行内 #标签 与 [[链接]]
    for line in content.lines() {
        collect_tags(line, &mut tags);
        collect_wikilinks(line, &mut outgoing);
    }

    dedupe(&mut tags);
    dedupe(&mut outgoing);

    Some(NoteMeta {
        path: path.to_path_buf(),
        name,
        tags,
        outgoing,
    })
}

/// 解析开头 `---\n...\n---` 里的 `tags:` 区块（支持 `tags: [a, b]` 与 `- a` 列表两种写法）。
fn parse_frontmatter_tags(content: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let trimmed = content.strip_prefix("---");
    let Some(rest) = trimmed else { return tags };
    let end = rest.find("\n---");
    let block = match end {
        Some(e) => &rest[..e],
        None => return tags,
    };
    let mut in_tags = false;
    for line in block.lines() {
        let l = line.trim_start();
        if !in_tags {
            if let Some(rest) = l.strip_prefix("tags:") {
                let rest = rest.trim();
                if rest.starts_with('[') {
                    // 内联数组 [a, b, c]
                    let inner = rest
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .trim();
                    for part in inner.split(',') {
                        let t = part.trim().trim_matches('"').trim_matches('\'').to_string();
                        if !t.is_empty() {
                            tags.push(t);
                        }
                    }
                }
                in_tags = true;
            }
        } else {
            if l.starts_with("- ") {
                let t = l[2..].trim().trim_matches('"').trim_matches('\'').to_string();
                if !t.is_empty() {
                    tags.push(t);
                }
            } else if !l.starts_with('-') {
                // 退出 tags 区块
                break;
            }
        }
    }
    tags
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// 收集一行内的 `#标签`。
fn collect_tags(line: &str, out: &mut Vec<String>) {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i].1;
        if c == '#' {
            let prev_word = i > 0 && is_word_char(chars[i - 1].1);
            if !prev_word {
                let mut k = i + 1;
                while k < chars.len() && is_word_char(chars[k].1) {
                    k += 1;
                }
                if k > i + 1 {
                    // k 可能等于 chars.len()（标签在行尾），此时用整行长度作为结束字节
                    let end = if k < chars.len() {
                        chars[k].0
                    } else {
                        line.len()
                    };
                    let name = &line[chars[i + 1].0..end];
                    out.push(name.to_string());
                    i = k;
                    continue;
                }
            }
        }
        i += 1;
    }
}

/// 收集一行内的 `[[目标]]` / `[[目标|别名]]`。
fn collect_wikilinks(line: &str, out: &mut Vec<String>) {
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i].1 == '[' && chars[i + 1].1 == '[' {
            let mut j = i + 2;
            let mut close = None;
            while j + 1 < chars.len() {
                if chars[j].1 == ']' && chars[j + 1].1 == ']' {
                    close = Some(j);
                    break;
                }
                j += 1;
            }
            if let Some(j) = close {
                let inner = &line[chars[i + 2].0..chars[j].0];
                let target = inner.split_once('|').map(|(t, _)| t).unwrap_or(inner);
                let target = target.trim();
                if !target.is_empty() {
                    out.push(target.to_string());
                }
                i = j + 2;
                continue;
            }
        }
        i += 1;
    }
}

/// 判断某行是否包含指向 `target` 的 wikilink（用于反链精确行定位）。
fn line_contains_wikilink(line: &str, target: &str) -> bool {
    let t = target.trim().to_lowercase();
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i].1 == '[' && chars[i + 1].1 == '[' {
            let mut j = i + 2;
            let mut close = None;
            while j + 1 < chars.len() {
                if chars[j].1 == ']' && chars[j + 1].1 == ']' {
                    close = Some(j);
                    break;
                }
                j += 1;
            }
            if let Some(j) = close {
                let inner = &line[chars[i + 2].0..chars[j].0];
                let inner_target = inner.split_once('|').map(|(t, _)| t).unwrap_or(inner);
                if inner_target.trim().to_lowercase() == t {
                    return true;
                }
                i = j + 2;
                continue;
            }
        }
        i += 1;
    }
    false
}

fn dedupe(v: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|s| seen.insert(s.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_trailing_tag_without_panic() {
        let mut tags = Vec::new();
        collect_tags("#知识库", &mut tags);
        assert_eq!(tags, vec!["知识库".to_string()]);

        let mut tags2 = Vec::new();
        collect_tags("正文 #fastnote #知识库", &mut tags2);
        assert_eq!(tags2, vec!["fastnote".to_string(), "知识库".to_string()]);
    }

    #[test]
    fn hash_inside_word_is_not_tag() {
        let mut tags = Vec::new();
        collect_tags("abc#def", &mut tags);
        assert!(tags.is_empty());
    }

    #[test]
    fn resolve_matches_stem_case_insensitive() {
        let mut idx = VaultIndex {
            notes: Vec::new(),
            by_stem: std::collections::HashMap::new(),
            by_path: std::collections::HashMap::new(),
        };
        let p = PathBuf::from("笔记/项目计划.md");
        idx.notes.push(NoteMeta {
            path: p.clone(),
            name: "项目计划".into(),
            tags: Vec::new(),
            outgoing: Vec::new(),
        });
        idx.by_stem.insert("项目计划".to_lowercase(), p.clone());
        assert_eq!(idx.resolve("项目计划"), Some(p.as_path()));
        assert_eq!(idx.resolve("项目计划.md"), Some(p.as_path()));
    }

    #[test]
    fn query_spec_parses_tags_terms_and_limit() {
        let s = parse_query("#知识库 #待办 会议 \"项目 计划\" limit:5");
        assert_eq!(s.tags, vec!["知识库".to_string(), "待办".to_string()]);
        assert_eq!(s.terms, vec!["会议".to_string(), "项目 计划".to_string()]);
        assert_eq!(s.limit, 5);
        assert!(s.links.is_empty());
    }

    #[test]
    fn query_spec_parses_link_constraint() {
        let s = parse_query("link:[[项目计划]] 风险");
        assert_eq!(s.links, vec!["项目计划".to_string()]);
        assert_eq!(s.terms, vec!["风险".to_string()]);
        assert_eq!(s.limit, 20, "未指定 limit 时用默认值");
    }

    #[test]
    fn empty_query_is_empty_spec() {
        assert!(parse_query("   \n  ").is_empty());
        assert!(!parse_query("#a").is_empty());
    }

    /// 构造一个带链接关系的手工索引：A→B、A→C、B→C。
    fn linked_index() -> VaultIndex {
        let mut idx = VaultIndex {
            notes: Vec::new(),
            by_stem: HashMap::new(),
            by_path: HashMap::new(),
        };
        for (name, out) in [("A", vec!["B", "C"]), ("B", vec!["C"]), ("C", vec![])] {
            let p = PathBuf::from(format!("{name}.md"));
            idx.notes.push(NoteMeta {
                path: p.clone(),
                name: name.into(),
                tags: Vec::new(),
                outgoing: out.iter().map(|s| s.to_string()).collect(),
            });
            idx.by_stem.insert(name.to_lowercase(), p.clone());
            idx.by_path.insert(p, idx.notes.len() - 1);
        }
        idx
    }

    #[test]
    fn graph_builds_edges_and_normalized_coords() {
        let g = linked_index().graph(50);
        assert_eq!(g.nodes.len(), 3);
        assert_eq!(g.edges.len(), 3, "A→B、A→C、B→C 三条边");

        for node in &g.nodes {
            assert!(
                (0.0..=1.0).contains(&node.x) && (0.0..=1.0).contains(&node.y),
                "坐标必须归一化到 [0,1]，实际 {} {}",
                node.x,
                node.y
            );
        }

        // C 被两篇链接，度数最高
        let c = g.nodes.iter().find(|n| n.name == "C").unwrap();
        assert_eq!(c.degree, 2);
        assert_eq!(g.nodes.iter().find(|n| n.name == "A").unwrap().degree, 2);
        assert_eq!(g.nodes.iter().find(|n| n.name == "B").unwrap().degree, 2);
    }

    #[test]
    fn graph_layout_is_deterministic() {
        let idx = linked_index();
        let a = idx.graph(50);
        let b = idx.graph(50);
        for (x, y) in a.nodes.iter().zip(b.nodes.iter()) {
            assert_eq!(x.x, y.x, "同一索引两次布局必须一致");
            assert_eq!(x.y, y.y);
        }
    }

    #[test]
    fn graph_truncates_to_max_nodes_keeping_busiest() {
        let mut idx = linked_index();
        // 加一个完全孤立的低度数节点
        let p = PathBuf::from("孤立.md");
        idx.notes.push(NoteMeta {
            path: p.clone(),
            name: "孤立".into(),
            tags: Vec::new(),
            outgoing: Vec::new(),
        });
        idx.by_stem.insert("孤立".to_lowercase(), p);

        let g = idx.graph(2);
        assert_eq!(g.nodes.len(), 2, "超过上限时截断");
        assert!(
            g.nodes.iter().all(|n| n.degree > 0),
            "应保留连接数高的节点，丢掉孤立点"
        );
    }

    #[test]
    fn graph_of_empty_index_is_empty() {
        let idx = VaultIndex {
            notes: Vec::new(),
            by_stem: HashMap::new(),
            by_path: HashMap::new(),
        };
        let g = idx.graph(50);
        assert!(g.nodes.is_empty() && g.edges.is_empty());
    }
}
