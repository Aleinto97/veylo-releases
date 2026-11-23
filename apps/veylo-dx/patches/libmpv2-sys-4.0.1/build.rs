use std::env;
use std::path::PathBuf;

#[cfg(not(feature = "use-bindgen"))]
fn main() {
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    let crate_path = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    // Veylo patch: the stock bindings are 64-bit (their layout checks fail on 32-bit). 32-bit ARM Android
    // (TVs) gets its own, made from include/ with bindgen 0.72.1 and this crate's options:
    //   bindgen <header including the four include/*.h> --formatter prettyplease --impl-debug
    //     --opaque-type mpv_handle --opaque-type mpv_render_context -- --target=armv7a-linux-androideabi26 --sysroot=<NDK sysroot>
    let bindings = match (env::var("CARGO_CFG_TARGET_OS").as_deref(), env::var("CARGO_CFG_TARGET_ARCH").as_deref()) {
        (Ok("android"), Ok("arm")) => "pregenerated_bindings_android_armv7.rs",
        _ => "pregenerated_bindings.rs",
    };
    std::fs::copy(
        crate_path.join(bindings),
        out_path.join("bindings.rs"),
    )
    .expect("Couldn't find pregenerated bindings!");

    println!("cargo:rustc-link-lib=mpv");
}

#[cfg(feature = "use-bindgen")]
fn main() {
    let bindings = bindgen::Builder::default()
        .formatter(bindgen::Formatter::Prettyplease)
        .header("include/client.h")
        .header("include/render.h")
        .header("include/render_gl.h")
        .header("include/stream_cb.h")
        .impl_debug(true)
        .opaque_type("mpv_handle")
        .opaque_type("mpv_render_context")
        .generate()
        .expect("Unable to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings!");

    println!("cargo:rustc-link-lib=mpv");
}
