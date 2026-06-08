//! Build script for cel-spaces.
//!
//! The private SkyLight framework lives in `/System/Library/PrivateFrameworks`,
//! which is NOT on the linker's default framework search path. Add it — but
//! only on macOS when the `spaces` feature is enabled, so default / non-macOS
//! builds (which use the stubs) never reference the private framework.

fn main() {
    let is_macos = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    let spaces_feature = std::env::var("CARGO_FEATURE_SPACES").is_ok();
    if is_macos && spaces_feature {
        println!("cargo:rustc-link-search=framework=/System/Library/PrivateFrameworks");
    }
}
