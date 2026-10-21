use std::path::PathBuf;
use std::process::Command;

/// 把 officepilot 命令行编译好放到 binaries/，作为 sidecar 打进安装包（Tauri 要求文件名带目标三元组）
fn build_cli() {
    let target = std::env::var("TARGET").expect("TARGET");
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let cli = manifest_dir.join("../../cli");
    let exe = if target.contains("windows") { ".exe" } else { "" };
    let dest = manifest_dir.join("binaries").join(format!("officepilot-{target}{exe}"));
    println!("cargo:rerun-if-changed=../../cli/src");
    println!("cargo:rerun-if-changed=../../cli/guides");
    println!("cargo:rerun-if-changed=../../engine/src");
    println!("cargo:rerun-if-env-changed=OFFICEPILOT_SKIP_CLI");
    if std::env::var("OFFICEPILOT_SKIP_CLI").is_ok() && dest.exists() {
        return;
    }
    // 单独的 target 目录，避免和外层 cargo 争用构建锁
    let target_dir = manifest_dir.join("target").join("officepilot-cli");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "--release", "--target", &target, "--manifest-path"])
        .arg(cli.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .expect("编译 officepilot 命令行失败");
    assert!(status.success(), "编译 officepilot 命令行失败");
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::copy(target_dir.join(&target).join("release").join(format!("officepilot{exe}")), &dest).expect("复制 officepilot 失败");
}

fn main() {
    build_cli();
    // 窗口加载的是本机服务（http://127.0.0.1:端口），远程来源只能调用这里声明的命令
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["reveal_path", "open_path", "pick_folder"])),
    )
    .expect("tauri-build 失败");
}
