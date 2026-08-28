/// Greets a person by name.
///
/// # Arguments
///
/// * `name` - The name of the person to greet
///
/// # Returns
///
/// A greeting string in the format "Hello, {name}!"
fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

fn main() {
    println!("{}", greet("world"));
}
