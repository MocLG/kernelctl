mod error;
mod loaders;
mod model;
mod sys;
mod ui;
mod util;

fn main() {
    println!("kernelctl {}", env!("CARGO_PKG_VERSION"));
}
