//! 白板：自由摆放的节点与连线，序列化成 Markdown 里的 ` ```board ` 代码块。
//!
//! 为什么不做成独立文件（比如 `.canvas`）：白板本质是"另一种笔记"，
//! 放进代码块后它照样是普通 Markdown —— 能被索引、能被搜索、能被 `[[双链]]`，
//! 用户还可以直接改 JSON。独立文件格式则会立刻长出第二套文件管理逻辑。
//!
//! 坐标一律用**归一化** `[0, 1]`，与画布像素尺寸解耦：换窗口大小、
//! 甚至换成导出 SVG，节点相对位置都不变。

use std::ops::Range;

/// 代码块的语言标记。
pub const FENCE: &str = "board";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Node {
    pub id: u32,
    pub x: f32,
    pub y: f32,
    #[serde(default)]
    pub text: String,
    /// 可选：指向笔记库里的某篇笔记（`[[名字]]` 里的名字）。
    /// 有值的节点在画布上可双击跳转。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Edge {
    pub from: u32,
    pub to: u32,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Board {
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

impl Board {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, id: u32) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// 下一个可用 id。删过节点后 id 不复用，避免连线指向"换了个身份"的旧 id。
    pub fn next_id(&self) -> u32 {
        self.nodes.iter().map(|n| n.id).max().map_or(1, |m| m + 1)
    }

    /// 新增节点，坐标会被夹到 `[0, 1]`。
    pub fn add_node(&mut self, x: f32, y: f32, text: impl Into<String>) -> u32 {
        let id = self.next_id();
        self.nodes.push(Node {
            id,
            x: x.clamp(0., 1.),
            y: y.clamp(0., 1.),
            text: text.into(),
            note: None,
        });
        id
    }

    /// 删除节点，连带删掉挂在它身上的所有连线。
    ///
    /// 留悬空边会让渲染层每帧都要做存在性检查，不如在数据层保证自洽。
    pub fn remove_node(&mut self, id: u32) {
        self.nodes.retain(|n| n.id != id);
        self.edges.retain(|e| e.from != id && e.to != id);
    }

    pub fn move_node(&mut self, id: u32, x: f32, y: f32) {
        if let Some(n) = self.nodes.iter_mut().find(|n| n.id == id) {
            n.x = x.clamp(0., 1.);
            n.y = y.clamp(0., 1.);
        }
    }

    pub fn set_text(&mut self, id: u32, text: impl Into<String>) {
        if let Some(n) = self.nodes.iter_mut().find(|n| n.id == id) {
            n.text = text.into();
        }
    }

    /// 连线。自环与重复边直接忽略，端点必须都存在。
    pub fn connect(&mut self, from: u32, to: u32) -> bool {
        if from == to || self.node(from).is_none() || self.node(to).is_none() {
            return false;
        }
        let dup = self
            .edges
            .iter()
            .any(|e| (e.from == from && e.to == to) || (e.from == to && e.to == from));
        if dup {
            return false;
        }
        self.edges.push(Edge { from, to });
        true
    }

    /// 覆盖落点：找出 `(x, y)`（归一化）命中的节点 id。
    ///
    /// `radius` 是容差（归一化单位），由调用方按画布尺寸换算后传入，
    /// 这样数据层不需要知道像素。
    pub fn hit(&self, x: f32, y: f32, radius: f32) -> Option<u32> {
        let r2 = radius * radius;
        self.nodes
            .iter()
            .rev() // 后画的在上层，命中优先
            .find(|n| {
                let dx = n.x - x;
                let dy = n.y - y;
                dx * dx + dy * dy <= r2
            })
            .map(|n| n.id)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}

/// 在 Markdown 正文里定位 ` ```board ` 代码块，返回 `(内容区间, 整块区间)`。
///
/// 整块区间含围栏行本身，供替换用；内容区间只有 JSON 部分，供解析用。
fn locate(md: &str) -> Option<(Range<usize>, Range<usize>)> {
    let mut offset = 0usize;
    let mut open: Option<(usize, usize)> = None; // (围栏行起, 内容起)

    for line in md.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let body = line.trim_end_matches(['\n', '\r']);
        let t = body.trim();

        if open.is_none() {
            // 允许 ```board 后面跟空白或多余的反引号
            if let Some(rest) = t.strip_prefix("```") {
                if rest.trim() == FENCE {
                    open = Some((start, offset));
                }
            }
        } else if t == "```" || t.trim_end_matches('`').is_empty() && t.len() >= 3 {
            let (fence_start, content_start) = open.take().unwrap();
            return Some((content_start..start, fence_start..offset));
        }
    }
    // 有开头没结尾：整块当成到文末
    open.map(|(fence_start, content_start)| (content_start..md.len(), fence_start..md.len()))
}

/// 取出笔记里的白板。没有代码块或 JSON 坏掉时返回 `None`。
pub fn extract(md: &str) -> Option<Board> {
    let (content, _) = locate(md)?;
    Board::from_json(&md[content])
}

/// 把白板写回 Markdown：替换已有的代码块，没有则追加到文末。
pub fn splice(md: &str, board: &Board) -> String {
    let block = format!("```{FENCE}\n{}\n```\n", board.to_json());

    match locate(md) {
        Some((_, whole)) => {
            let mut out = String::with_capacity(md.len() + block.len());
            out.push_str(&md[..whole.start]);
            out.push_str(&block);
            out.push_str(&md[whole.end..]);
            out
        }
        None => {
            let mut out = md.to_string();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&block);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_move_remove_keep_ids_stable() {
        let mut b = Board::new();
        let a = b.add_node(0.1, 0.2, "A");
        let c = b.add_node(0.3, 0.4, "B");
        assert_ne!(a, c);

        b.move_node(a, 0.9, 0.8);
        assert_eq!(b.node(a).unwrap().x, 0.9);

        b.remove_node(a);
        assert!(b.node(a).is_none());
        // id 不复用：删掉 a 后新节点不能拿到 a 的 id
        assert_ne!(b.add_node(0., 0., "C"), a);
    }

    #[test]
    fn coordinates_are_clamped() {
        let mut b = Board::new();
        let id = b.add_node(-3., 7.5, "越界");
        let n = b.node(id).unwrap();
        assert_eq!((n.x, n.y), (0., 1.));
    }

    #[test]
    fn remove_node_drops_dangling_edges() {
        let mut b = Board::new();
        let a = b.add_node(0., 0., "A");
        let c = b.add_node(1., 1., "B");
        assert!(b.connect(a, c));
        b.remove_node(a);
        assert!(b.edges.is_empty(), "悬空边必须一起清掉");
    }

    #[test]
    fn connect_rejects_self_loop_duplicates_and_missing_nodes() {
        let mut b = Board::new();
        let a = b.add_node(0., 0., "A");
        let c = b.add_node(0.5, 0.5, "B");
        assert!(!b.connect(a, a), "自环不合法");
        assert!(b.connect(a, c));
        assert!(!b.connect(a, c), "重复边不合法");
        assert!(!b.connect(c, a), "反向重复边同样不合法");
        assert!(!b.connect(a, 999), "端点不存在不合法");
    }

    #[test]
    fn hit_picks_topmost_within_radius() {
        let mut b = Board::new();
        let a = b.add_node(0.2, 0.2, "左");
        b.add_node(0.5, 0.5, "底");
        let d = b.add_node(0.5, 0.5, "顶");

        assert_eq!(b.hit(0.2, 0.2, 0.02), Some(a));
        assert_eq!(b.hit(0.5, 0.5, 0.02), Some(d), "重叠时应命中后画的那个");
        assert_eq!(b.hit(0.35, 0.35, 0.02), None, "半径外不算命中");
        // 容差放大后能命中：半径由调用方按画布尺寸换算
        assert_eq!(b.hit(0.35, 0.35, 0.25), Some(d));
    }

    #[test]
    fn json_roundtrip() {
        let mut b = Board::new();
        let a = b.add_node(0.25, 0.75, "想法");
        let c = b.add_node(0.5, 0.1, "目标");
        b.connect(a, c);
        b.nodes[1].note = Some("项目计划".into());

        let back = Board::from_json(&b.to_json()).unwrap();
        assert_eq!(back, b);
    }

    #[test]
    fn extract_finds_board_block() {
        let md = "# 标题\n\n```board\n{\"nodes\":[{\"id\":1,\"x\":0.1,\"y\":0.2,\"text\":\"A\"}]}\n```\n\n尾巴\n";
        let b = extract(md).unwrap();
        assert_eq!(b.nodes.len(), 1);
        assert_eq!(b.nodes[0].text, "A");
        assert_eq!(b.nodes[0].note, None, "缺省字段应能反序列化");
    }

    #[test]
    fn extract_ignores_other_fences() {
        let md = "```rust\nfn main() {}\n```\n";
        assert!(extract(md).is_none());
    }

    #[test]
    fn extract_returns_none_on_broken_json() {
        let md = "```board\n{不是 JSON}\n```\n";
        assert!(extract(md).is_none());
    }

    #[test]
    fn splice_replaces_existing_block_in_place() {
        let md = "# 标题\n\n```board\n{\"nodes\":[]}\n```\n\n尾巴\n";
        let mut b = Board::new();
        b.add_node(0.5, 0.5, "新");

        let out = splice(md, &b);
        assert!(out.starts_with("# 标题\n\n"), "块前内容要保持");
        assert!(out.ends_with("\n\n尾巴\n"), "块后内容要保持");
        assert_eq!(extract(&out).unwrap(), b);
        assert_eq!(out.matches("```board").count(), 1, "不能出现两个块");
    }

    #[test]
    fn splice_appends_when_missing() {
        let b = {
            let mut b = Board::new();
            b.add_node(0.2, 0.2, "唯一");
            b
        };
        let out = splice("# 只有标题", &b);
        assert!(out.starts_with("# 只有标题\n\n```board\n"));
        assert_eq!(extract(&out).unwrap(), b);
    }

    #[test]
    fn splice_into_empty_document_has_no_leading_blank() {
        let out = splice("", &Board::new());
        assert!(out.starts_with("```board\n"), "空文档不该先塞空行：{out:?}");
        assert!(extract(&out).is_some());
    }

    #[test]
    fn splice_is_idempotent() {
        let mut b = Board::new();
        b.add_node(0.3, 0.3, "稳定");
        let once = splice("# T\n", &b);
        let twice = splice(&once, &b);
        assert_eq!(once, twice);
    }

    #[test]
    fn unterminated_block_is_still_replaced() {
        let md = "# T\n\n```board\n{\"nodes\":[]}\n";
        let mut b = Board::new();
        b.add_node(0.1, 0.1, "x");
        let out = splice(md, &b);
        assert_eq!(out.matches("```board").count(), 1);
        assert_eq!(out.matches("```").count(), 2, "应补上结束围栏");
        assert_eq!(extract(&out).unwrap(), b);
    }
}
