//! One file two modules read, only one of them #[cfg(test)]: it stays.

pub fn answer() -> u32 {
    crate::vm_tests::answer()
}
