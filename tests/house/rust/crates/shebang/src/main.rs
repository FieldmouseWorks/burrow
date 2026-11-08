#!/usr/bin/env -S cargo run --quiet #[cfg(test)]
mod live;

fn main() {
    println!("{}", live::word());
}
