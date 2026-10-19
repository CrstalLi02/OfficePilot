fn main() {
    // 窗口加载的是本机服务（http://127.0.0.1:端口），远程来源只能调用这里声明的命令
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["reveal_path", "open_path", "pick_folder"])),
    )
    .expect("tauri-build 失败");
}
