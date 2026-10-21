//! Prints [`web_test::report`] natively, for CI to compare with the wasm run.

fn main() {
    print!("{}", web_test::report());
}
