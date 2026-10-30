//! The package's binary, which says which zlib its build found.

fn main() {
    println!("{}", house_native::zlib_version());
}
