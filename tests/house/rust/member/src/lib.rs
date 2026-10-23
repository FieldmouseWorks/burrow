//! A workspace member, which the Rust gate must check as well as the root.

pub fn double(a: u32) -> u32 {
    a * 2
}

#[cfg(test)]
mod tests {
    #[test]
    fn doubles() {
        assert_eq!(super::double(2), 4);
    }
}
