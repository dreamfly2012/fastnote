//! 笔记库（Vault）：一个本地目录下的 Markdown 文件树。
//!
//! 扫描是浅层 + 按需展开的：只读当前目录一层，展开子目录时才继续读。
//! 这样即使笔记库有几万个文件，启动时也只付一层目录的 IO 代价。

use std::path::{Path, PathBuf};

use anyhow::Result;

/// 被识别为笔记的扩展名。
const NOTE_EXTS: &[&str] = &["md", "markdown", "mdx", "txt"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    /// 目录在树中的展开状态
    pub expanded: bool,
    /// 缩进层级，0 为笔记库根下的直接子项
    pub depth: usize,
}

impl Entry {
    pub fn is_note(&self) -> bool {
        !self.is_dir && is_note_path(&self.path)
    }
}

pub fn is_note_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| NOTE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

pub struct Vault {
    root: PathBuf,
    /// 扁平化的可见条目列表，顺序即渲染顺序
    entries: Vec<Entry>,
}

impl Vault {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let mut v = Self {
            root,
            entries: Vec::new(),
        };
        v.reload()?;
        Ok(v)
    }

    #[inline]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[inline]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// 重新扫描：保留已展开目录的展开状态。
    pub fn reload(&mut self) -> Result<()> {
        let expanded: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|e| e.is_dir && e.expanded)
            .map(|e| e.path.clone())
            .collect();

        self.entries = read_level(&self.root, 0)?;

        for path in expanded {
            if let Some(i) = self.entries.iter().position(|e| e.path == path) {
                self.expand_at(i)?;
            }
        }
        Ok(())
    }

    /// 切换目录展开状态。
    pub fn toggle(&mut self, index: usize) -> Result<()> {
        let Some(e) = self.entries.get(index) else {
            return Ok(());
        };
        if !e.is_dir {
            return Ok(());
        }
        if e.expanded {
            self.collapse_at(index);
            Ok(())
        } else {
            self.expand_at(index)
        }
    }

    fn expand_at(&mut self, index: usize) -> Result<()> {
        let (path, depth) = {
            let Some(e) = self.entries.get(index) else {
                return Ok(());
            };
            if !e.is_dir || e.expanded {
                return Ok(());
            }
            (e.path.clone(), e.depth)
        };
        let children = read_level(&path, depth + 1)?;
        self.entries[index].expanded = true;
        self.entries.splice(index + 1..index + 1, children);
        Ok(())
    }

    fn collapse_at(&mut self, index: usize) {
        let depth = match self.entries.get(index) {
            Some(e) => e.depth,
            None => return,
        };
        let mut end = index + 1;
        while end < self.entries.len() && self.entries[end].depth > depth {
            end += 1;
        }
        self.entries.drain(index + 1..end);
        self.entries[index].expanded = false;
    }

    /// 在笔记库中新建笔记，返回其路径。
    pub fn create_note(&mut self, name: &str) -> Result<PathBuf> {
        self.create_note_with(name, "")
    }

    /// 带初始内容新建笔记（模板展开后的正文走这里）。
    pub fn create_note_with(&mut self, name: &str, body: &str) -> Result<PathBuf> {
        let path = self.unique_path(name);
        std::fs::write(&path, body)?;
        self.reload()?;
        Ok(path)
    }

    /// 不重名的落点：重名则追加序号，绝不覆盖用户已有文件。
    fn unique_path(&self, name: &str) -> PathBuf {
        let mut file = name.trim().to_string();
        if file.is_empty() {
            file = "未命名".into();
        }
        if !is_note_path(Path::new(&file)) {
            file.push_str(".md");
        }
        let mut path = self.root.join(&file);
        let stem = Path::new(&file)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名".into());
        let mut n = 2;
        while path.exists() {
            path = self.root.join(format!("{stem} {n}.md"));
            n += 1;
        }
        path
    }

    /// 库里的模板（`templates/*.md`）。
    pub fn templates(&self) -> Vec<crate::template::Template> {
        crate::template::load_all(&self.root)
    }

    /// 遍历笔记库内所有笔记文件（递归，供 RAG 索引使用）。
    pub fn walk_notes(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        walk(&self.root, &mut out, 0);
        out
    }

    /// 打开（不存在则创建）笔记库内的相对路径笔记，父目录会自动建。
    ///
    /// 与 [`Vault::create_note`] 的区别：这里是**确定性路径**而不是自动改名，
    /// 每日笔记每天必须落到同一个文件，重复调用不能产生 `2026-09-17 2.md`。
    pub fn ensure_note_at(&mut self, rel: &str) -> Result<PathBuf> {
        self.ensure_note_at_with(rel, "")
    }

    /// 同 [`Vault::ensure_note_at`]，但新建时把 `body` 作为初始内容写进去；
    /// 文件已存在则**不动它**（每日笔记反复打开不能覆盖当天已有的记录）。
    pub fn ensure_note_at_with(&mut self, rel: &str, body: &str) -> Result<PathBuf> {
        let path = self.root.join(rel);
        if !path.exists() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, body)?;
            self.reload()?;
        }
        Ok(path)
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 16 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }
        if p.is_dir() {
            walk(&p, out, depth + 1);
        } else if is_note_path(&p) {
            out.push(p);
        }
    }
}

fn read_level(dir: &Path, depth: usize) -> Result<Vec<Entry>> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        // 无权限或已被删除时返回空列表，不让侧栏整体炸掉
        Err(_) => return Ok(Vec::new()),
    };

    for e in rd.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            dirs.push(Entry {
                path,
                name,
                is_dir: true,
                expanded: false,
                depth,
            });
        } else if is_note_path(&path) {
            files.push(Entry {
                path,
                name,
                is_dir: false,
                expanded: false,
                depth,
            });
        }
    }

    // 目录在前，各自按名称排序（不区分大小写）
    dirs.sort_by_key(|e| e.name.to_lowercase());
    files.sort_by_key(|e| e.name.to_lowercase());
    dirs.extend(files);
    Ok(dirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("fastnote-vault-{name}"));
        std::fs::remove_dir_all(&p).ok();
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn lists_dirs_first_then_notes_and_skips_non_notes() {
        let root = tmp("list");
        std::fs::write(root.join("b.md"), "").unwrap();
        std::fs::write(root.join("a.md"), "").unwrap();
        std::fs::write(root.join("image.png"), "").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();

        let v = Vault::open(&root).unwrap();
        let names: Vec<_> = v.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["sub", "a.md", "b.md"]);
    }

    #[test]
    fn expand_and_collapse_subdirectory() {
        let root = tmp("expand");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("inner.md"), "").unwrap();

        let mut v = Vault::open(&root).unwrap();
        assert_eq!(v.entries().len(), 1);
        v.toggle(0).unwrap();
        assert_eq!(v.entries().len(), 2);
        assert_eq!(v.entries()[1].name, "inner.md");
        assert_eq!(v.entries()[1].depth, 1);
        v.toggle(0).unwrap();
        assert_eq!(v.entries().len(), 1);
    }

    #[test]
    fn create_note_avoids_overwriting_existing() {
        let root = tmp("create");
        let mut v = Vault::open(&root).unwrap();
        let p1 = v.create_note("笔记").unwrap();
        let p2 = v.create_note("笔记").unwrap();
        assert_ne!(p1, p2);
        assert!(p1.exists() && p2.exists());
    }

    #[test]
    fn walk_notes_is_recursive() {
        let root = tmp("walk");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("x.md"), "").unwrap();
        std::fs::write(root.join("a/y.md"), "").unwrap();
        std::fs::write(root.join("a/b/z.md"), "").unwrap();
        let v = Vault::open(&root).unwrap();
        assert_eq!(v.walk_notes().len(), 3);
    }

    #[test]
    fn ensure_note_at_creates_once_and_reuses() {
        let root = tmp("ensure");
        let mut v = Vault::open(&root).unwrap();

        let p1 = v.ensure_note_at("daily/2026-09-17.md").unwrap();
        assert!(p1.exists());
        // 写入内容后再次调用，不能覆盖、也不能另建一个带序号的文件
        std::fs::write(&p1, "# 今天的记录").unwrap();
        let p2 = v.ensure_note_at("daily/2026-09-17.md").unwrap();
        assert_eq!(p1, p2);
        assert_eq!(std::fs::read_to_string(&p1).unwrap(), "# 今天的记录");
        assert!(!root.join("daily/2026-09-17 2.md").exists());
    }

    #[test]
    fn create_note_with_writes_initial_body() {
        let root = tmp("create-with");
        let mut v = Vault::open(&root).unwrap();
        let p = v.create_note_with("周报", "# 周报\n\n正文").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "# 周报\n\n正文");
        // 重名保护依旧生效
        let p2 = v.create_note_with("周报", "另一份").unwrap();
        assert_ne!(p, p2);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "# 周报\n\n正文");
    }

    #[test]
    fn ensure_note_at_with_does_not_touch_existing_file() {
        let root = tmp("ensure-with");
        let mut v = Vault::open(&root).unwrap();
        let p = v.ensure_note_at_with("daily/2026-09-17.md", "# 模板内容").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "# 模板内容");

        // 已有记录时再调用，绝不能把当天的内容冲掉
        std::fs::write(&p, "# 手写的记录").unwrap();
        let p2 = v.ensure_note_at_with("daily/2026-09-17.md", "# 模板内容").unwrap();
        assert_eq!(p, p2);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "# 手写的记录");
    }

    #[test]
    fn templates_lists_template_dir() {
        let root = tmp("templates");
        std::fs::create_dir_all(root.join("templates")).unwrap();
        std::fs::write(root.join("templates/会议.md"), "# {{date}}").unwrap();
        let v = Vault::open(&root).unwrap();
        let t = v.templates();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].name, "会议");
        // 模板目录本身不能被当成笔记列出来（它是目录，只有里面的文件是笔记）
        assert!(v.entries().iter().any(|e| e.name == "templates" && e.is_dir));
    }
}
