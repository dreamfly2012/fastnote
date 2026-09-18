//! 版本快照：把笔记的历史内容留档在 `<库根>/.fastnote/history/` 下。
//!
//! 放在 `.fastnote` 里有两个实际好处：侧栏与索引扫描都会跳过 `.` 开头的目录
//! （不会污染笔记树），而且它是"跟着库走"的元数据 —— 把库目录拷到另一台
//! 机器，历史也就一起过去了，不需要任何账号或服务端。
//!
//! 触发点是**写盘之前**：`snapshot` 保存的是"这一版之前的样子"，
//! 所以它是"能退回上一步"，而不是"每次保存留一份相同的副本"。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::date;

/// 快照仓库相对库根的路径。
pub const STORE_DIR: &str = ".fastnote/history";
/// 单个文件保留的快照份数上限，超出后从最旧的开始删。
pub const MAX_PER_FILE: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub path: PathBuf,
    /// 展示用时间戳 `2026-09-17 14:05:33`
    pub stamp: String,
    pub bytes: u64,
}

/// 某篇笔记的快照目录：镜像相对路径 + `.snap` 后缀。
///
/// 加后缀而不是直接把 `a.md` 变成目录 `a`，是为了避免 `a.md` 与 `a.txt`
/// 这类同名的不同扩展名笔记撞进同一个目录。
fn snap_dir(root: &Path, file: &Path) -> Option<PathBuf> {
    let rel = file.strip_prefix(root).ok()?;
    let mut name = rel.to_string_lossy().replace('\\', "/");
    name.push_str(".snap");
    Some(root.join(STORE_DIR).join(name))
}

/// 把某个时刻的 Unix 秒格式化成 `(文件名片段, 展示串)`。
fn stamps(secs: i64) -> (String, String) {
    let show = date::datetime_from_unix(secs);
    let compact = show
        .replace(['-', ':'], "")
        .replacen(' ', "-", 1)
        .replace(' ', "");
    (compact, show)
}

/// 为 `file` 的**当前内容**留一份快照。
///
/// 返回 `Ok(None)` 表示无需快照：文件不存在、内容为空，或与最新一份完全相同。
pub fn snapshot(root: &Path, file: &Path) -> Result<Option<PathBuf>> {
    if !file.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(file)
        .with_context(|| format!("读取 {} 失败", file.display()))?;
    if content.trim().is_empty() {
        return Ok(None);
    }
    let Some(dir) = snap_dir(root, file) else {
        return Ok(None);
    };
    std::fs::create_dir_all(&dir)?;

    let (compact, _) = stamps(date::now_unix());
    let mut path = dir.join(format!("{compact}.md"));
    let mut n = 1;
    while path.exists() {
        // 同一秒内连续写：内容一致就没必要再留一份
        if std::fs::read_to_string(&path).map(|s| s == content).unwrap_or(false) {
            return Ok(None);
        }
        n += 1;
        path = dir.join(format!("{compact}-{n}.md"));
    }

    std::fs::write(&path, &content)?;
    prune(&dir)?;
    Ok(Some(path))
}

/// 超出上限时删最旧的几份。
fn prune(dir: &Path) -> Result<()> {
    let mut files = list_files(dir);
    if files.len() <= MAX_PER_FILE {
        return Ok(());
    }
    // 文件名按时间戳升序，删前面的
    files.truncate(files.len() - MAX_PER_FILE);
    for f in files {
        let _ = std::fs::remove_file(f);
    }
    Ok(())
}

fn list_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "md").unwrap_or(false))
        .collect();
    out.sort();
    out
}

/// 列出某篇笔记的全部快照，最新的在前。
pub fn list(root: &Path, file: &Path) -> Vec<Snapshot> {
    let Some(dir) = snap_dir(root, file) else {
        return Vec::new();
    };
    let mut out: Vec<Snapshot> = list_files(&dir)
        .into_iter()
        .map(|path| {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            Snapshot {
                stamp: format_stem(&stem),
                bytes: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                path,
            }
        })
        .collect();
    out.reverse();
    out
}

/// `20260917-140533` -> `2026-09-17 14:05:33`（识别不了的按原样展示）。
fn format_stem(stem: &str) -> String {
    let stem = stem.split('-').take(2).collect::<Vec<_>>().join("-");
    let Some((d, t)) = stem.split_once('-') else {
        return stem;
    };
    if d.len() != 8 || t.len() < 6 {
        return stem;
    }
    format!(
        "{}-{}-{} {}:{}:{}",
        &d[0..4],
        &d[4..6],
        &d[6..8],
        &t[0..2],
        &t[2..4],
        &t[4..6]
    )
}

pub fn read(snap: &Path) -> Result<String> {
    Ok(std::fs::read_to_string(snap)?)
}

/// 恢复某个快照到 `file`。
///
/// 覆盖前先给**当前内容**留一份快照 —— 否则"恢复"本身就是一次不可逆操作，
/// 手滑点错就彻底丢了。
pub fn restore(root: &Path, file: &Path, snap: &Path) -> Result<()> {
    let body = read(snap)?;
    snapshot(root, file)?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(file, body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("fastnote-hist-{name}"));
        std::fs::remove_dir_all(&p).ok();
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write(file: &Path, body: &str) {
        if let Some(d) = file.parent() {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(file, body).unwrap();
    }

    #[test]
    fn snapshot_skips_empty_and_missing_files() {
        let root = tmp("skip");
        assert!(snapshot(&root, &root.join("没有这个文件.md")).unwrap().is_none());
        let f = root.join("空.md");
        write(&f, "   \n");
        assert!(snapshot(&root, &f).unwrap().is_none());
        assert!(list(&root, &f).is_empty());
    }

    #[test]
    fn snapshot_then_restore_returns_previous_content() {
        let root = tmp("restore");
        let f = root.join("笔记.md");
        write(&f, "第一版");

        // 模拟"保存前留档"：先把第一版存下来，再写第二版
        snapshot(&root, &f).unwrap();
        write(&f, "第二版");

        let snaps = list(&root, &f);
        assert_eq!(snaps.len(), 1, "只应有一份快照");
        assert_eq!(read(&snaps[0].path).unwrap(), "第一版");

        restore(&root, &f, &snaps[0].path).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "第一版");
    }

    #[test]
    fn restore_is_itself_reversible() {
        let root = tmp("revert");
        let f = root.join("笔记.md");
        write(&f, "A");
        snapshot(&root, &f).unwrap();
        write(&f, "B");

        let snaps = list(&root, &f);
        restore(&root, &f, &snaps[0].path).unwrap(); // B -> A
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "A");

        // 恢复动作把 B 也留了档，所以还能再走回去
        let after = list(&root, &f);
        assert!(after.len() >= 2, "恢复前应给当前版本留档，实际 {} 份", after.len());
        assert!(after.iter().any(|s| read(&s.path).unwrap() == "B"));
    }

    #[test]
    fn identical_content_is_not_snapshotted_twice() {
        let root = tmp("dup");
        let f = root.join("稳定.md");
        write(&f, "# 不变");
        assert!(snapshot(&root, &f).unwrap().is_some());
        assert!(snapshot(&root, &f).unwrap().is_none(), "内容没变不该再留一份");
        assert_eq!(list(&root, &f).len(), 1);
    }

    #[test]
    fn snapshots_are_kept_in_subdirectories_mirroring_the_rel_path() {
        let root = tmp("sub");
        let f = root.join("daily/2026-09-17.md");
        write(&f, "# 今天");
        snapshot(&root, &f).unwrap();

        let dir = root.join(STORE_DIR).join("daily/2026-09-17.md.snap");
        assert!(dir.is_dir(), "快照目录应镜像相对路径：{}", dir.display());
        // `.` 开头的目录不该被当成笔记扫出来
        let v = crate::Vault::open(&root).unwrap();
        assert!(
            v.walk_notes().iter().all(|p| !p.starts_with(root.join(".fastnote"))),
            "快照不能混进笔记列表"
        );
    }

    #[test]
    fn prune_keeps_only_the_newest_n() {
        let root = tmp("prune");
        let dir = root.join(STORE_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..MAX_PER_FILE + 5 {
            std::fs::write(dir.join(format!("2026010{i:02}-120000.md")), "x").unwrap();
        }
        prune(&dir).unwrap();
        let left = list_files(&dir);
        assert_eq!(left.len(), MAX_PER_FILE);
        // 留下的必须是文件名最大的那批（最旧的 5 份被删）
        assert!(left[0].file_name().unwrap().to_string_lossy().contains("202601005"));
        assert!(left
            .last()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("202601044"));
    }

    #[test]
    fn format_stem_renders_readable_timestamps() {
        assert_eq!(format_stem("20260917-140533"), "2026-09-17 14:05:33");
        assert_eq!(format_stem("20260917-140533-2"), "2026-09-17 14:05:33");
        // 认不出来就原样返回，不能 panic
        assert_eq!(format_stem("乱七八糟"), "乱七八糟");
    }
}
