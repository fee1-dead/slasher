//@ check-pass
#![deny(redetect::unused)]
#![crate_type = "lib"]

trait MyTrait {}

impl MyTrait for u32 {}

#[inline]
fn uwu<T: MyTrait>() {}

#[inline]
fn what() {
    uwu::<u32>()
}

pub fn main() {
    what();
}
