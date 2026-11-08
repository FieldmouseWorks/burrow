//! A member's binary, which the Rust package must install as well as the
//! root's.

fn main() {
    println!(
        "{}",
        house_fixture_member::double(
            house_fixture_member::shared::answer() + house_fixture_member::r#type::answer() - 21
        )
    );
}
