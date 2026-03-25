// Native UI builds don't use a Rust binary entry point.
// The library (dimmy_lib) is consumed as a DLL (Windows) or static lib (macOS).
fn main() {
    eprintln!("This binary is not used. Use the native UI app instead.");
    std::process::exit(1);
}
