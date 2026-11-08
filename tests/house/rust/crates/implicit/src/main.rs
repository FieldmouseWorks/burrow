const TESTS: &str = include_str!("word_tests.rs");

fn main() {
    println!(
        "{}",
        if TESTS.contains("fn words") {
            "implicit"
        } else {
            "lost"
        }
    );
}
