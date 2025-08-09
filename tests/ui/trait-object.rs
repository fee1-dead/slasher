#![deny(redetect::unused)]

// make sure that trait object usage is marked as used

trait MyTrait {}

impl MyTrait for u32 {}
impl MyTrait for u8 {}
//~^ ERROR: implementation of MyTrait for u8 is unused

fn u(_x: &dyn MyTrait) {}

fn main() {
    u(&1u32);
}

