fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rerun-if-changed=src/recording/macos_system.m");
        println!("cargo:rerun-if-changed=MeetliteCapture-Info.plist");
        println!("cargo:rerun-if-env-changed=MEETLITE_EMBEDDED_INFO_PLIST");
        cc::Build::new()
            .file("src/recording/macos_system.m")
            .flag("-fobjc-arc")
            // Xcode 26 selector stubs are incompatible with Rust's linker path.
            .flag("-fno-objc-msgsend-selector-stubs")
            .compile("meetlite_macos_system");

        println!("cargo:rustc-link-lib=framework=AudioToolbox");
        println!("cargo:rustc-link-lib=framework=CoreAudio");
        println!("cargo:rustc-link-lib=framework=Foundation");
        if let Ok(info_plist) = std::env::var("MEETLITE_EMBEDDED_INFO_PLIST") {
            println!(
                "cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}",
                info_plist
            );
        }
    }
}
