include!(concat!(env!("OUT_DIR"), "/built.rs"));

fn main() {
    println!("{BUILT}");
}
