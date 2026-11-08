//! A program whose unit tests' module is the file of the program named
//! tests, which Cargo finds in src/bin by itself: lib/project.nix's
//! cargoTests keeps it, whatever declares it.

fn main() {
    println!("main");
}

#[allow(dead_code)]
#[cfg(test)]
mod tests;
