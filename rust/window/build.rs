// The window's markup (ui/app.slint) is compiled into Rust at build time: a mistake in it fails the build.
//
// The version number (file VERSION, passed by tools\rust.ps1 as COREMAP_VERSION) goes into the window title - the
// owner's demand 15.10: a number he can name, not a build time. COREMAP_STAMP stays internal: a new one when the
// sources changed, and a new one again when Smart App Control refused the built exe - a rebuild of unchanged Rust
// code is the same file and would get the same verdict.
fn main() {
    println!("cargo:rerun-if-env-changed=COREMAP_STAMP");
    println!("cargo:rerun-if-env-changed=COREMAP_VERSION");
    // The standard stamp also identifies this local build helper: SAC may refuse its previous unsigned image.
    let stamp =
        std::env::var("COREMAP_STAMP").unwrap_or_else(|_| option_env!("COREMAP_STAMP").unwrap_or_default().to_owned());
    println!("cargo:rustc-env=COREMAP_BUILT={stamp}");
    let version = std::env::var("COREMAP_VERSION").unwrap_or_else(|_| "без номера".into());
    println!("cargo:rustc-env=COREMAP_VERSION={version}");
    slint_build::compile("ui/app.slint").expect("ui/app.slint");
}
