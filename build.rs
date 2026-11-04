// Compiles the Objective-C permission guide (native/permission_guide.m) with ARC.
fn main() {
    println!("cargo:rerun-if-changed=native/permission_guide.m");
    cc::Build::new()
        .file("native/permission_guide.m")
        .flag("-fobjc-arc")
        .flag("-fmodules")
        .compile("clip_native");
    for framework in ["AppKit", "AVFoundation", "ApplicationServices", "CoreGraphics", "CoreImage", "QuartzCore"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
