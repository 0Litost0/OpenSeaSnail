fn main() {
    println!("cargo:rerun-if-env-changed=SEASNAIL_CAPSULE_SMOKE");
    tauri_build::build()
}
