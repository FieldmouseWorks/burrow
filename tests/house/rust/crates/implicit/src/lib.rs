pub fn word() -> &'static str {
    "implicit"
}

// The program, as a test module too: it stays, being Cargo's own target.
#[cfg(test)]
#[allow(dead_code)]
#[path = "main.rs"]
mod cli_tests;

#[cfg(test)]
mod word_tests;
