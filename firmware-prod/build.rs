fn main() {
    // Set the root crate for esp-idf-sys in workspace context
    std::env::set_var("ESP_IDF_SYS_ROOT_CRATE", "pup-prod");

    embuild::espidf::sysenv::output();
}
