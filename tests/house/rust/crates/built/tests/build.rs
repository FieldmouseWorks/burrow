fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    std::fs::write(
        format!("{out}/built.rs"),
        "const BUILT: &str = \"built\";\n",
    )
    .unwrap();
}
