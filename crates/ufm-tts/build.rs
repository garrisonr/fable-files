// The fable-tts helper links sherpa-onnx's shared libraries, which
// ride beside the binary (dev builds: target dir; shipped builds:
// the app's tts-runtime resources). An $ORIGIN rpath lets the loader
// find them there without LD_LIBRARY_PATH or a system install.
fn main() {
    if std::env::var_os("CARGO_FEATURE_ENGINE").is_some() {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
    }
}
