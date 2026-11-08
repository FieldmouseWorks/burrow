// A file in the directory of a #[cfg(test)] module's own modules, which a
// module that isn't one reads by its path: the directory stays.
#[path = "sorting/sorting_tests/limit.rs"]
mod limit;

pub fn smallest(a: u32, b: u32) -> u32 {
    a.min(b).min(limit::LIMIT)
}

// Its tests as a module of its own, in sorting/.
#[cfg(test)]
mod sorting_tests;
