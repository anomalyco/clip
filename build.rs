// Compiles the Objective-C permission guide (native/permission_guide.m) with ARC.
fn main() {
    println!("cargo:rerun-if-changed=native/permission_guide.m");
    // The release's version and channel (scripts/bundle.sh), for the updater.
    println!("cargo:rerun-if-env-changed=CLIP_VERSION");
    println!("cargo:rerun-if-env-changed=CLIP_BUILD_CHANNEL");
    cc::Build::new()
        .file("native/permission_guide.m")
        .flag("-fobjc-arc")
        .flag("-fmodules")
        .compile("clip_native");
    for framework in ["AppKit", "AVFoundation", "ApplicationServices", "CoreGraphics", "CoreImage", "QuartzCore"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
