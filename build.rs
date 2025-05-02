use std::path::PathBuf;

fn main() {
    gen_wayland();
    gen_ei();
}

fn gen_ei() {
    let project_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let code = ei_scanner::Config::default()
        .context_type(ei_scanner::ContextType::Sender)
        .protocol(project_dir.join("protocol/ei.xml"))
        .interface("ei_button", 1)
        .interface("ei_handshake", 1)
        .interface("ei_device", 2)
        .interface("ei_callback", 1)
        .interface("ei_connection", 1)
        .interface("ei_pingpong", 1)
        .interface("ei_seat", 1)
        .interface("ei_pointer_absolute", 1)
        .interface("ei_scroll", 1)
        .generate();
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out_dir.join("ei.rs"), code).unwrap();
}

fn gen_wayland() {
    let project_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let code = wayland_scanner::Config::default()
        .protocol(project_dir.join("protocol/single-pixel-buffer-v1.xml"))
        .protocol(project_dir.join("protocol/wayland.xml"))
        .protocol(project_dir.join("protocol/wlr-layer-shell-unstable-v1.xml"))
        .protocol(project_dir.join("protocol/wlr-virtual-pointer-unstable-v1.xml"))
        .protocol(project_dir.join("protocol/xdg-output-unstable-v1.xml"))
        .protocol(project_dir.join("protocol/xdg-shell.xml"))
        .global("wl_display", 1)
        .global("wl_compositor", 4)
        .global("wl_output", 2)
        .global("wl_seat", 4)
        .global("wl_shm", 1)
        .global("zxdg_output_manager_v1", 3)
        .global("zwlr_layer_shell_v1", 1)
        .global("zwlr_virtual_pointer_manager_v1", 1)
        .global("wp_single_pixel_buffer_manager_v1", 1)
        .generate();
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(out_dir.join("wayland.rs"), code).unwrap();
}
