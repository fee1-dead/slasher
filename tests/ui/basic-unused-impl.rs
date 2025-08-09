#![deny(redetect::unused)]
trait MyTrait {}

impl MyTrait for u32 {}
impl MyTrait for u8 {}
//~^ ERROR: implementation of MyTrait for u8 is unused

fn u<T: MyTrait>() {}

fn main() {
    u::<u32>();
}
