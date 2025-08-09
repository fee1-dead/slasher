#![deny(redetect::unused)]

// assoc const usage is also used

trait MyTrait {
    const A: u32;
}

impl MyTrait for u32 {
    const A: u32 = 32;
}
impl MyTrait for u8 {
    //~^ ERROR: implementation of MyTrait for u8 is unused
    const A: u32 = 8;
}

fn main() {
    println!("{}", u32::A);
}
