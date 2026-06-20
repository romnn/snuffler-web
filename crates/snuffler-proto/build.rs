fn main() {
    for file in [
        "common.proto",
        "scene.proto",
        "view.proto",
        "snuffling.proto",
        "data.proto",
    ] {
        println!("cargo:rerun-if-changed=../../proto/snuffler/bridge/v1/{file}");
    }
    prost_build::Config::new()
        .compile_protos(
            &[
                "../../proto/snuffler/bridge/v1/common.proto",
                "../../proto/snuffler/bridge/v1/scene.proto",
                "../../proto/snuffler/bridge/v1/view.proto",
                "../../proto/snuffler/bridge/v1/snuffling.proto",
                "../../proto/snuffler/bridge/v1/data.proto",
            ],
            &["../../proto"],
        )
        .expect("failed to compile snuffler bridge protobuf schema");
}
