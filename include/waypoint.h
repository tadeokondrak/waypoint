#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include <wayland-client-core.h>

struct waypoint_state {
    bool running;
    bool have_all_globals;

    struct xkb_context *xkb_context;

    struct wl_display *wl_display;
    struct wl_registry *wl_registry;

    struct wl_shm *wl_shm;
    struct wl_compositor *wl_compositor;
    struct zwlr_layer_shell_v1 *wlr_layer_shell;
    struct zwlr_virtual_pointer_manager_v1 *wlr_virtual_pointer_manager;
    struct zxdg_output_manager_v1 *xdg_output_manager;

    struct wl_surface *wl_surface;
    struct zwlr_layer_surface_v1 *wlr_layer_surface;

    struct zwlr_virtual_pointer_v1 *wlr_virtual_pointer;

    struct wl_list buffers;
    struct wl_list seats;
    struct wl_list outputs;

    struct waypoint_output *output;

    int surface_width, surface_height;

    int grid_size;
    int32_t color0;
    int32_t color1;

    double x, y;
    double width, height;
};

struct waypoint_global {
    const struct wl_interface *interface;
    const char *name;
    int version;
    bool is_singleton;
    union {
        size_t offset;
        void (*callback)(struct waypoint_state *state, void *data);
    };
};

struct waypoint_seat {
    struct wl_list link;
    struct waypoint_state *state;
    struct wl_seat *wl_seat;
    struct wl_pointer *wl_pointer;
    struct wl_keyboard *wl_keyboard;
    struct xkb_state *xkb_state;
    struct xkb_keymap *xkb_keymap;
    char *name;
    uint32_t mods;
};

struct waypoint_output {
    struct wl_list link;
    struct waypoint_state *state;
    struct wl_output *wl_output;
    struct zxdg_output_v1 *xdg_output;
    char *name;
    int scale_factor;
    int width, height;
};

struct waypoint_buffer {
    struct wl_list link;
    struct waypoint_state *state;
    struct wl_buffer *wl_buffer;
    int width, height;
    unsigned char *data;
    size_t size;
    bool in_use;
};

