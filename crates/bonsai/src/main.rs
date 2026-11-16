fn main() {
    if let Some(code) = bonsai::askpass::run_from_env() {
        std::process::exit(code);
    }
    bonsai::run();
}
