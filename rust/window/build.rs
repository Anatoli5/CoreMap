// The window's markup (ui/app.slint) is compiled into Rust at build time: a mistake in it fails the build.
fn main() {
    slint_build::compile("ui/app.slint").expect("ui/app.slint");
}
