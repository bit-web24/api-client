//! Compiles the Slint UI into Rust code included by `src/main.rs`.

/// Compiles `ui/app.slint`; widgets not drawn by hand (scroll bars) use the dark Fluent style.
fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/app.slint", config)
        .expect("invariant: ui/app.slint must compile");
}
