//! 性能验证：不是微基准，而是回答三个具体问题——
//! 1. 打开一个大文件要多久（决定"秒开"能不能兑现）
//! 2. 视口解析要多久（决定滚动是否掉帧）
//! 3. 在大文件末尾打一个字要多久（决定输入是否有延迟）
//!
//! 用 `cargo test --release -p fastnote-core --test perf -- --nocapture` 查看实测数字。

use std::time::Instant;

use fastnote_core::{Document, Editor};

/// 一帧的预算。release 下按 60FPS 的 16.6ms 留余量取 16ms；
/// debug 下 ropey / pulldown-cmark 都没做内联优化，实测慢 5~10 倍，
/// 因此放宽到 10 倍，仅作为「复杂度没写错」的冒烟检查——
/// 真实性能数字必须看 `cargo test --release`。
const fn frame_budget_ms() -> u128 {
    if cfg!(debug_assertions) { 160 } else { 16 }
}

/// 最坏单帧允许比平均值宽松：偶发的 OS 调度尖峰、页错误不代表算法退化。
const fn worst_budget_ms() -> u128 {
    frame_budget_ms() * 4
}

fn mode() -> &'static str {
    if cfg!(debug_assertions) {
        "debug（数字仅供参考，请用 --release 看真实性能）"
    } else {
        "release"
    }
}

/// 构造一份接近真实笔记形态的大文档（标题 / 段落 / 列表 / 代码块混排）。
fn synth(target_bytes: usize) -> String {
    let mut s = String::with_capacity(target_bytes + 1024);
    let mut i = 0usize;
    while s.len() < target_bytes {
        s.push_str(&format!("## 第 {i} 节标题\n\n"));
        s.push_str(
            "这是一段**正文**内容，包含 `行内代码`、[链接](https://example.com) 以及一些中文字符，\
             用来模拟真实笔记的行内标记密度。\n\n",
        );
        s.push_str("- 列表项一\n- 列表项二\n- [ ] 一个待办事项\n\n");
        if i % 5 == 0 {
            s.push_str("```rust\nfn demo() -> usize {\n    42\n}\n```\n\n");
        }
        i += 1;
    }
    s
}

#[test]
fn large_document_open_and_viewport_parse() {
    // 30MB：足够暴露"全量解析"式实现的问题，同时让测试保持在可接受时长内
    let target = 30 * 1024 * 1024;
    let src = synth(target);
    let mb = src.len() as f64 / 1024.0 / 1024.0;

    let t0 = Instant::now();
    let mut doc = Document::from_str(&src);
    let build = t0.elapsed();

    let lines = doc.len_lines();
    println!("\n[{}]", mode());
    println!("文档规模: {mb:.1} MB / {lines} 行");
    println!("构建 Rope（含行索引）: {build:?}");

    // 首屏：解析前 60 行
    let t1 = Instant::now();
    let n_first = doc.blocks_for_lines(0..60).len();
    let first_paint = t1.elapsed();
    println!("首屏视口解析（60 行 → {n_first} 块）: {first_paint:?}");

    // 跳到文档正中间，模拟拖动滚动条
    let mid = lines / 2;
    let t2 = Instant::now();
    let n_mid = doc.blocks_for_lines(mid..mid + 60).len();
    let mid_paint = t2.elapsed();
    println!("跳转到中部再解析（{n_mid} 块）: {mid_paint:?}");

    // 连续滚动 200 屏，统计单帧最坏耗时
    let mut worst = std::time::Duration::ZERO;
    let mut total = std::time::Duration::ZERO;
    for k in 0..200 {
        let start = (mid + k * 40).min(lines.saturating_sub(1));
        let t = Instant::now();
        let _ = doc.blocks_for_lines(start..start + 60);
        let e = t.elapsed();
        total += e;
        worst = worst.max(e);
    }
    let avg = total / 200;
    println!("连续滚动 200 帧: 平均 {avg:?} / 最坏 {worst:?}");

    assert!(
        avg.as_millis() < frame_budget_ms(),
        "视口解析平均耗时超预算({}ms): {avg:?}",
        frame_budget_ms()
    );
    assert!(
        worst.as_millis() < worst_budget_ms(),
        "视口解析最坏单帧超预算({}ms): {worst:?}",
        worst_budget_ms()
    );
    assert!(
        first_paint.as_millis() < frame_budget_ms(),
        "首屏解析过慢: {first_paint:?}"
    );
}

#[test]
fn typing_at_end_of_large_document_stays_fast() {
    let src = synth(20 * 1024 * 1024);
    let mut ed = Editor::new(Document::from_str(&src));
    ed.move_doc_end(false);

    // 预热一次视口解析
    let last = ed.doc.len_lines().saturating_sub(1);
    let _ = ed.doc.blocks_for_lines(last.saturating_sub(60)..last);

    let mut worst = std::time::Duration::ZERO;
    let mut total = std::time::Duration::ZERO;
    let n = 200;
    for _ in 0..n {
        let t = Instant::now();
        ed.insert("字");
        // 输入后必须重新解析视口才算完整一帧
        let last = ed.doc.len_lines().saturating_sub(1);
        let _ = ed.doc.blocks_for_lines(last.saturating_sub(60)..last);
        let e = t.elapsed();
        total += e;
        worst = worst.max(e);
    }
    let avg = total / n;
    println!("\n[{}]", mode());
    println!("20MB 文档末尾连续输入 {n} 次: 平均 {avg:?} / 最坏 {worst:?}");
    assert!(
        avg.as_millis() < frame_budget_ms(),
        "输入平均延迟超预算({}ms): {avg:?}",
        frame_budget_ms()
    );
    assert!(
        worst.as_millis() < worst_budget_ms(),
        "输入最坏延迟超预算({}ms): {worst:?}",
        worst_budget_ms()
    );
}

#[test]
fn open_from_disk_measures_real_io() {
    let dir = std::env::temp_dir().join("fastnote-perf");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("big.md");

    let src = synth(30 * 1024 * 1024);
    std::fs::write(&path, &src).unwrap();
    let mb = src.len() as f64 / 1024.0 / 1024.0;

    let t = Instant::now();
    let mut doc = Document::open(&path).unwrap();
    let open = t.elapsed();

    let t2 = Instant::now();
    let _ = doc.blocks_for_lines(0..60);
    let paint = t2.elapsed();

    println!("\n从磁盘打开 {mb:.1} MB: {open:?}（含读盘）");
    println!("打开后首屏可见: {paint:?}");
    println!("总计到可交互: {:?}", open + paint);

    std::fs::remove_file(&path).ok();
}
