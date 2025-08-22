//@ aux-build: derives.rs
//@ rustc-env: SLASHER_WORKSPACE_ROOT=basic
//@ rustc-env: SLASHER_WORKSPACE_RE=derives
//@ rustc-env: SLASHER_TRAIT_RE=Debug|Hash

fn main() {
    let d = derives::Derives;
    println!("{d:?}");
}
