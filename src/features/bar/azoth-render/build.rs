use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for lib in &["fcft", "pixman-1"] {
        let out = Command::new("pkg-config")
            .args(["--libs-only-L", lib])
            .output()?;
        if out.status.success() {
            for flag in String::from_utf8_lossy(&out.stdout).split_whitespace() {
                if let Some(path) = flag.strip_prefix("-L") {
                    println!("cargo:rustc-link-search=native={path}");
                }
            }
        }
    }
    println!("cargo:rustc-link-lib=dylib=fcft");
    println!("cargo:rustc-link-lib=dylib=pixman-1");
    println!("cargo:rerun-if-changed=build.rs");
    Ok(())
}
