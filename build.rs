use std::path::PathBuf;

fn main() {
    let project_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let code = wayland_scanner::Config::default()
        .protocol(project_dir.join("protocol/wayland.xml"))
        .protocol(project_dir.join("protocol/wlr-layer-shell-unstable-v1.xml"))
        .protocol(project_dir.join("protocol/wlr-virtual-pointer-unstable-v1.xml"))
        .protocol(project_dir.join("protocol/xdg-output-unstable-v1.xml"))
        .protocol(project_dir.join("protocol/xdg-shell.xml"))
        .global("wl_display", 1)
        .global("wl_compositor", 4)
        .global("wl_output", 2)
        .global("wl_seat", 1)
        .global("wl_shm", 1)
        .global("zxdg_output_manager_v1", 3)
        .global("zwlr_layer_shell_v1", 1)
        .global("zwlr_virtual_pointer_manager_v1", 1)
        .generate();
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out_dir.join("wayland.rs"), code).unwrap();
}
