//! Build script: derives compound `cfg` aliases from Cargo features so the long
//! `any(feature = "tile", …)` layout lists live in exactly one place.

fn main() {
    // Every built-in tiling layout feature. This is the ONLY place the full set
    // is enumerated — add a new layout here and every `#[cfg(any_layout)]` /
    // `#[cfg(not(any_layout))]` site updates automatically.
    const LAYOUTS: &[&str] = &[
        "tile", "monocle", "col", "scroll", "dwindle", "bstack", "centeredmaster",
    ];

    // `any_layout` is set when at least one built-in layout is compiled in; its
    // negation marks the built-in `Fallback` code paths.
    println!("cargo::rustc-check-cfg=cfg(any_layout)");
    let enabled = LAYOUTS
        .iter()
        .any(|f| std::env::var_os(format!("CARGO_FEATURE_{}", f.to_uppercase())).is_some());
    if enabled {
        println!("cargo::rustc-cfg=any_layout");
    }
}
