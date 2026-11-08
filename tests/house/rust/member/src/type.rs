//! A module named by a raw identifier, which a #[cfg(test)] module reads
//! too: it stays.

pub fn answer() -> u32 {
    crate::vm_tests::answer()
}
