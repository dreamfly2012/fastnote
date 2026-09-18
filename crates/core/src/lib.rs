//! fastnote 核心引擎：与 GUI 完全解耦的文本层。
//!
//! 几个模块各管一件事：
//! - [`document`]：Rope 存储 + 惰性视口块解析 + 编辑与落盘
//! - [`block`] / [`inline`]：Markdown 的块级扫描与行内解析
//! - [`vault`]：本地笔记库文件树
//! - [`index`]：双向链接、标签、搜索与关系图谱
//! - [`template`]：新建笔记的模板与变量替换
//! - [`board`]：白板（节点 / 连线）及其在 Markdown 里的存取
//! - [`history`]：写盘前的版本留档与恢复
//!
//! 这一层不引入任何窗口/渲染依赖，因此可以脱离 GUI 单测，
//! 也意味着将来换渲染后端不需要动核心逻辑。

pub mod block;
pub mod board;
pub mod date;
pub mod document;
pub mod editor;
pub mod history;
pub mod index;
pub mod inline;
pub mod template;
pub mod vault;

pub use block::{Block, BlockKind, ListMarker};
pub use board::{Board, Edge as BoardEdge, Node as BoardNode};
pub use document::{Document, Newline};
pub use editor::Editor;
pub use history::Snapshot;
pub use index::{
    Graph, GraphEdge, GraphNode, LinkRef, NoteMeta, QuerySpec, SearchHit, VaultIndex, parse_query,
};
pub use inline::{InlineText, SpanStyle, StyledSpan};
pub use template::Template;
pub use vault::{Entry, Vault};
