// value.rs is two modules on purpose.
#![allow(clippy::duplicate_mod)]

#[path = "shared/entry.rs"]
mod entry;

#[cfg(test)]
#[allow(dead_code)]
#[path = "shared/value.rs"]
mod value_alias;

fn main() {
    println!("{}", entry::word());
}
