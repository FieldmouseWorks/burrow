// The cc crate compiles add.c, as a bundled C library's build script does.
fn main() {
    cc::Build::new().file("add.c").compile("add");
}
