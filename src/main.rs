//! `whatsup` binary entry point. CLI parsing and dispatch arrive in M1
//! (docs/DESIGN.md section 12); M0 only provides the skeleton.

fn main() {
    println!("whatsup {} (work in progress)", env!("CARGO_PKG_VERSION"));
}
