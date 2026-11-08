fn main() {
    let read = house_fixture_inner::DATA.contains("fn data");
    println!("{}", if read { "outer" } else { "lost" });
}
