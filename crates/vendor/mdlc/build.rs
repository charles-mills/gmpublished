use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=assets/mdlc.rc");
    println!("cargo:rerun-if-changed=assets/mdlc.ico");

    let target = std::env::var("TARGET").expect("cargo 必定设置 TARGET");
    if !target.contains("windows") {
        return;
    }

    if !rc_available(&target) {
        println!(
            "cargo:warning=跳过图标资源：找不到 rc.exe（缺 Windows SDK，或在非 Windows 宿主上交叉编译）—— 产物将没有图标"
        );
        return;
    }

    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("cargo 必定设置 CARGO_MANIFEST_DIR");
    let rc = Path::new(&manifest_dir).join("assets").join("mdlc.rc");

    match embed_resource::compile_for(&rc, ["mdlc"], embed_resource::NONE) {
        embed_resource::CompilationResult::Ok | embed_resource::CompilationResult::NotWindows => {}
        embed_resource::CompilationResult::NotAttempted(missing) => {
            println!("cargo:warning=跳过图标资源：{missing} —— 产物将没有图标");
        }
        embed_resource::CompilationResult::Failed(message) => panic!(
            "图标资源编译失败：{message}\n\
             资源编译器是能找到的，所以这多半是 assets/mdlc.rc 或 assets/mdlc.ico 本身写坏了。"
        ),
    }
}

fn rc_available(target: &str) -> bool {
    if !target.ends_with("-msvc") {
        return true;
    }

    let from_env = std::env::var_os(format!("RC_{target}"))
        .or_else(|| std::env::var_os(format!("RC_{}", target.replace('-', "_"))))
        .or_else(|| std::env::var_os("RC"));

    let resolved = from_env
        .map(std::path::PathBuf::from)
        .or_else(|| embed_resource::find_windows_sdk_tool("rc.exe"));

    if let Some(path) = resolved
        && is_runnable(&path)
    {
        return true;
    }

    is_runnable(std::path::Path::new("rc.exe"))
}

fn is_runnable(program: &std::path::Path) -> bool {
    std::process::Command::new(program)
        .arg("/?")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}
