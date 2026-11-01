mod seven;

fn main() {
    println!("{}", itoa::Buffer::new().format(seven::SEVEN));
}
