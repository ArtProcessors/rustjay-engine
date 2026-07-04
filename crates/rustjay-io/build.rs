fn main() {
    #[cfg(target_os = "macos")]
    {
        // Syphon linking (framework search path, link-lib, dev-time rpath) is
        // handled by the published syphon-core crate, which bundles
        // Syphon.framework under its own frameworks/ dir. Release bundles must
        // still copy the framework into <app>.app/Contents/Frameworks — the
        // rpaths below make the binary look there.

        // ===== NDI Library =====
        // NDI rpath: always add if installed (propagates to downstream test binaries)
        let ndi_lib_paths = ["/usr/local/lib", "/Library/NDI SDK for Apple/lib/macOS"];

        for path in &ndi_lib_paths {
            if std::path::Path::new(path).exists() {
                println!("cargo:rustc-link-arg=-Wl,-rpath,{}", path);
            }
        }

        // ===== AVFoundation (camera authorization) =====
        println!("cargo:rustc-link-lib=framework=AVFoundation");

        // Bundle-friendly rpaths
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Frameworks");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path/../Frameworks");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path");

        println!("cargo:rerun-if-changed=build.rs");
    }
}
