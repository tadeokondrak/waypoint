#include <stdbool.h>
#include <stdint.h>

#include <wayland-util.h>

struct state {
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

    struct output *output;

    int surface_width, surface_height;

    int grid_size;
    int32_t color0;
    int32_t color1;

    double x, y;
    double width, height;
};

struct global {
    const struct wl_interface *interface;
    const char *name;
    int version;
    bool is_singleton;
    union {
        size_t offset;
        void (*callback)(struct state *state, void *data);
    };
};

struct seat {
    struct wl_list link;
    struct state *state;
    struct wl_seat *wl_seat;
    struct wl_keyboard *wl_keyboard;
    struct xkb_state *xkb_state;
    struct xkb_keymap *xkb_keymap;
    char *name;
};

struct output {
    struct wl_list link;
    struct state *state;
    struct wl_output *wl_output;
    struct zxdg_output_v1 *xdg_output;
    char *name;
    int scale_factor;
    int width, height;
};

struct buffer {
    struct wl_list link;
    struct state *state;
    struct wl_buffer *wl_buffer;
    int width, height;
    unsigned char *data;
    size_t size;
    bool in_use;
};

void draw(struct state *state);
void update_pointer(struct state *state);
void split_left(struct state *state);
void split_right(struct state *state);
void split_up(struct state *state);
void split_down(struct state *state);
void click(struct state *state);
void quit(struct state *state);

struct buffer *get_buffer(struct state *state, int width, int height);

extern const struct wl_output_listener wl_output_listener;
extern const struct zxdg_output_v1_listener xdg_output_listener;
extern const struct wl_keyboard_listener wl_keyboard_listener;
extern const struct wl_seat_listener wl_seat_listener;
extern const struct wl_buffer_listener wl_buffer_listener;
extern const struct wl_registry_listener wl_registry_listener;
extern const struct zwlr_layer_surface_v1_listener wlr_layer_surface_listener;

extern const struct global globals[];
extern const size_t globals_len;
