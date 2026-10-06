//! `lyra-node`: see the library.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    lyra_node::main(&args);
}
