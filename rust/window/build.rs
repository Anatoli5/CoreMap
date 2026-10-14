// The window's markup (ui/app.slint) is compiled into Rust at build time: a mistake in it fails the build.
//
// The build time goes into the window title, like the old window's «сборка 14.10.26 14:02». tools\rust.ps1 passes
// it as COREMAP_STAMP: a new one when the sources changed, and a new one again when Smart App Control refused the
// built exe - a rebuild of unchanged Rust code is the same file and would get the same verdict.
fn main() {
    println!("cargo:rerun-if-env-changed=COREMAP_STAMP");
    let stamp = std::env::var("COREMAP_STAMP").unwrap_or_default();
    println!("cargo:rustc-env=COREMAP_BUILT={stamp}");
    slint_build::compile("ui/app.slint").expect("ui/app.slint");
}
