//! 编 `proto/seasnail/v1/{transcript,cleanup}.proto` → prost 生成 `seasnail.v1` 模块。
//!
//! 用 `protox`（纯 Rust `protoc` 实现，无外部 protoc 依赖——对齐设计文档横切关注点
//! 「Rust prost，纯 Rust 无外部 protoc」，AI 友好、构建机无需 brew install protobuf）
//! 解析 proto 为 `FileDescriptorSet`，再交 `prost-build` 生成 Rust 代码。
//! 生成代码落到 OUT_DIR，由 `src/lib.rs` include。

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    // proto 在仓库根 proto/seasnail/v1/transcript.proto。
    let proto_root = manifest_dir.join("..").join("..").join("proto");
    let proto_dir = proto_root.join("seasnail").join("v1");
    let proto_files = [
        proto_dir.join("transcript.proto"),
        proto_dir.join("cleanup.proto"),
        proto_root
            .join("seasnail")
            .join("native")
            .join("v1")
            .join("post_paste_monitor.proto"),
    ];

    for proto_file in &proto_files {
        println!("cargo:rerun-if-changed={}", proto_file.display());
    }

    // 纯 Rust 解析（无需系统 protoc）。
    let file_descriptor_set =
        protox::compile(&proto_files, [&proto_root]).expect("protox 解析 seasnail.v1 proto 失败");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let mut config = prost_build::Config::new();
    config.out_dir(&out_dir);
    config
        .compile_fds(file_descriptor_set)
        .expect("prost 生成 seasnail.v1 Rust 代码失败");
}
