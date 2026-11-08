// word.rs is two modules on purpose.
#![allow(clippy::duplicate_mod)]

macro_rules! module {
    ($name:ident) => {
        #[path = "word.rs"]
        pub mod $name;
    };
}

module!(made);

#[cfg(test)]
#[allow(dead_code)]
#[path = "word.rs"]
mod word_alias;
