mod error;
mod model;
mod sys;
mod util;

fn main() {
    println!("kernelctl {}", env!("CARGO_PKG_VERSION"));
}
