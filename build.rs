// Embeds res/app.rc (icon, manifest, version info) with the Windows SDK's rc.exe when it is installed.
// Without the SDK the app still builds and runs; it just has no icon in Explorer.
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_rc() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("NFSW_RC") {
        return Some(PathBuf::from(p));
    }
    let bin = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(bin)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join(r"x64\rc.exe").is_file())
        .collect();
    versions.sort();
    versions.pop().map(|p| p.join(r"x64\rc.exe"))
}

fn main() {
    println!("cargo:rerun-if-changed=res");
    println!("cargo:rerun-if-env-changed=NFSW_RC");
    let Some(rc) = find_rc() else {
        println!("cargo:warning=rc.exe not found: building without icon and manifest");
        return;
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("app.res");
    let status = Command::new(&rc)
        .args(["/nologo", "/fo"])
        .arg(&out)
        .arg("res/app.rc")
        .status()
        .expect("run rc.exe");
    assert!(status.success(), "rc.exe failed");
    // The linker takes a compiled .res like an object file.
    println!("cargo:rustc-link-arg-bins={}", out.display());
}
