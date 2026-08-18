fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rerun-if-changed=src/recording/macos_system.m");
        cc::Build::new()
            .file("src/recording/macos_system.m")
            .flag("-fobjc-arc")
            // Xcode 26 selector stubs are incompatible with Rust's linker path.
            .flag("-fno-objc-msgsend-selector-stubs")
            .compile("meetlite_macos_system");

        println!("cargo:rustc-link-lib=framework=AudioToolbox");
        println!("cargo:rustc-link-lib=framework=CoreAudio");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }
}
