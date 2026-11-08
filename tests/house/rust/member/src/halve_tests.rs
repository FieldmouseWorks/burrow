// A test module may read files itself: Cargo builds the package without
// reading it, so its include doesn't keep the package's tests in.
const ITSELF: &str = include_str!("halve_tests.rs");

#[test]
fn halves() {
    assert_eq!(super::halve(4), 2);
    assert!(ITSELF.contains("fn halves"));
}
