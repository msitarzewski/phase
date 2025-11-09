use std::io::{self, Read, Write};

fn main() {
    // Read input from stdin
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .expect("Failed to read from stdin");

    // Reverse the input string
    let reversed: String = input.chars().rev().collect();

    // Write to stdout
    print!("{}", reversed);
    io::stdout().flush().expect("Failed to flush stdout");

    // Exit successfully (implicit return)
}
