use std::env;

fn main() {
    let proto_dir = "protocol";
    let protos = [
        "protocol/handle.proto",
        "protocol/types.proto",
        "protocol/portal.proto",
        "protocol/ffi.proto",
    ];

    for p in &protos {
        println!("cargo:rerun-if-changed={p}");
    }
    println!("cargo:rerun-if-changed=build.rs");

    configure_linker();

    prost_build::Config::new()
        .compile_protos(&protos, &[proto_dir])
        .expect("failed to compile protos");
}

// WebRTC from the LiveKit rust-sdks links in Objective-C categories for the
// macOS/iOS video codec factories. Without `-ObjC`, Apple's linker strips
// category symbols as dead code and calls like
// `+[NSString stringForAbslStringView:]` throw
// `unrecognized selector` at runtime when the PeerConnection factory starts.
// On Linux, webrtc ships as a static archive and must be force-linked.
fn configure_linker() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match target_os.as_str() {
        "macos" | "ios" => {
            println!("cargo:rustc-link-arg=-ObjC");
        }
        "linux" => {
            println!("cargo:rustc-link-lib=static=webrtc");
        }
        _ => {}
    }
}
