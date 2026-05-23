trait Greeter {
    fn greet(&self) -> String;
}

struct Person {
    name: String,
}

impl Greeter for Person {
    fn greet(&self) -> String {
        format!("Hello, {}", self.name)
    }
}

fn main() {
    let p = Person {
        name: "World".to_string(),
    };
    let _ = p.greet();
}
