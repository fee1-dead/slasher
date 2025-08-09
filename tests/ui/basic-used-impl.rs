//@ check-pass
#![deny(redetect::unused)]

trait MyTrait {
    fn method();
}

impl MyTrait for u32 {
    fn method() {}
}

fn main() {
    u32::method();
}
