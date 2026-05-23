use std::collections::HashMap;
use std::io;

fn main() {
    let mut map: HashMap<String, i32> = HashMap::new();
    map.insert("key".to_string(), 42);
    println!("{:?}", map);
}
