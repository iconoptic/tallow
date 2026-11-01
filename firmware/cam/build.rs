//! Points the build at ESP-IDF. This crate does not bake in the mesh key.

fn main() {
    embuild::espidf::sysenv::output();
}
