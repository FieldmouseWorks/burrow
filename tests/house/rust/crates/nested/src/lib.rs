// shared.rs is two modules on purpose.
#![allow(clippy::duplicate_mod)]

pub mod nested {
    pub mod shared;
}

#[cfg(test)]
#[allow(dead_code)]
#[path = "nested/shared.rs"]
mod test_shared;
