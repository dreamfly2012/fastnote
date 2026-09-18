//! 构建脚本（仅 Windows 生效）。
//!
//! 把 `res/fastnote.ico` 编译进 exe 的资源表（RT_GROUP_ICON，资源 ID 1）。
//! GPUI 的 Windows 后端在 `platform/windows/platform.rs::load_icon` 里用
//! `LoadImageW(module, PCWSTR(1), IMAGE_ICON, ...)` 读取这个 ID=1 的图标，
//! 于是任务栏 / Alt-Tab / 窗口标题栏都会显示它。
//!
//! `embed-resource` 已存在于本仓库的 `Cargo.lock`（gpui 自身构建就用它），
//! 它通过 `vswhom` 定位 Visual Studio 的 `rc.exe`，因此不依赖 `rc.exe` 在 PATH 中。

fn main() {
    #[cfg(target_os = "windows")]
    {
        let rc = "res/fastnote.rc";
        println!("cargo:rerun-if-changed={rc}");
        println!("cargo:rerun-if-changed=res/fastnote.ico");
        let _ = embed_resource::compile(rc, embed_resource::NONE);
    }
}
