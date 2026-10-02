fn main() {
    // The runtime seam looks up the dev sidecar as
    // `binaries/mira-runtime-<target triple>`, so bake in the compiled triple.
    let target = std::env::var("TARGET").expect("cargo provides TARGET to build scripts");
    println!("cargo:rustc-env=MIRA_RUNTIME_TARGET_TRIPLE={target}");
    tauri_build::build()
}
