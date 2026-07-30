fn main() {
    link_clang_runtime();
    tauri_build::build()
}

/// Link clang's compiler-rt builtins on macOS.
///
/// ggml-metal (inside whisper.cpp) uses Objective-C `@available()`, which the
/// compiler lowers to a call to `__isPlatformVersionAtLeast` — a symbol that
/// lives in `libclang_rt.osx.a`. Rust links with `-nodefaultlibs`, so nothing
/// pulls that archive in and the release build dies at link time on an
/// undefined symbol.
///
/// Debug builds link fine, which is what makes this easy to miss: Tauri only
/// pins `-mmacosx-version-min` for release, and without a deployment target
/// older than the checked version the `@available` test constant-folds away and
/// the symbol is never referenced. The failure shows up the first time you try
/// to ship, not while developing.
fn link_clang_runtime() {
    if !cfg!(target_os = "macos") {
        return;
    }

    let Ok(out) = std::process::Command::new("xcrun")
        .args(["--find", "clang"])
        .output()
    else {
        return;
    };
    let clang = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    let Some(bin) = clang.parent() else { return };

    // .../usr/lib/clang/<version>/lib/darwin/libclang_rt.osx.a — the version
    // directory moves with the toolchain, so search for it rather than pinning.
    let root = bin.join("..").join("lib").join("clang");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let mut versions: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("lib/darwin/libclang_rt.osx.a").exists())
        .collect();
    versions.sort();

    if let Some(newest) = versions.last() {
        println!(
            "cargo:rustc-link-search=native={}",
            newest.join("lib/darwin").display()
        );
        println!("cargo:rustc-link-lib=static=clang_rt.osx");
    }
}
