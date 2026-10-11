use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for lib in &["fcft", "pixman-1"] {
        // Explicit `-L` flags. These may be empty: pkgconf strips libdirs it
        // considers "system" paths from `--libs-only-L`.
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

        // The library's own libdir. pkgconf omits this from `--libs-only-L` when
        // it is a system path (e.g. `/usr/local/lib` on OpenBSD) — but that dir
        // is NOT on the linker's default search path there, so `-lfcft` fails to
        // resolve. Add it explicitly; a duplicate `-L` on Linux is harmless.
        let dir = Command::new("pkg-config")
            .args(["--variable=libdir", lib])
            .output()?;
        if dir.status.success() {
            let path = String::from_utf8_lossy(&dir.stdout);
            let path = path.trim();
            if !path.is_empty() {
                println!("cargo:rustc-link-search=native={path}");
            }
        }
    }
    println!("cargo:rustc-link-lib=dylib=fcft");
    println!("cargo:rustc-link-lib=dylib=pixman-1");
    println!("cargo:rerun-if-changed=build.rs");
    Ok(())
}
