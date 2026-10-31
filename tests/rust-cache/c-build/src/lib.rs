extern "C" {
    fn add(a: i32, b: i32) -> i32;
}

pub fn sum(a: i32, b: i32) -> i32 {
    unsafe { add(a, b) }
}
