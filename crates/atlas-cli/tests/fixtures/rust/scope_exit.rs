fn main() {
    let x = Box::new(42);
    // no explicit drop — scope exit should free
}
