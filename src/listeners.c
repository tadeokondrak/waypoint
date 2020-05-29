#include "waypoint.h"

#include <stdlib.h>
#include <string.h>

#include <unistd.h>
#include <sys/mman.h>

#include <wayland-client.h>
#include <xkbcommon/xkbcommon.h>
#include <xkbcommon/xkbcommon-keysyms.h>

#include "wlr-layer-shell-unstable-v1-client-protocol.h"
#include "wlr-virtual-pointer-unstable-v1-client-protocol.h"
#include "xdg-output-unstable-v1-client-protocol.h"
#include "xdg-shell-client-protocol.h"

static int global_compare(const void *a, const void *b) {
    return strcmp(
        ((const struct global *)a)->name,
        ((const struct global *)b)->name);
}

static void wl_output_geometry(void *data, struct wl_output *wl_output,
    int32_t x, int32_t y, int32_t physical_width, int32_t physical_height,
    int32_t subpixel, const char *make, const char *model, int32_t transform)
{
}

static void wl_output_mode(void *data, struct wl_output *wl_output,
    uint32_t flags, int32_t width, int32_t height, int32_t refresh)
{
}

static void wl_output_done(void *data, struct wl_output *wl_output) {
}

static void wl_output_scale(void *data, struct wl_output *wl_output,
    int32_t factor)
{
    struct output *output = data;
    output->scale_factor = factor;
}

const struct wl_output_listener wl_output_listener = {
    .geometry = wl_output_geometry,
    .mode = wl_output_mode,
    .done = wl_output_done,
    .scale = wl_output_scale,
};

static void xdg_output_logical_position(void *data,
    struct zxdg_output_v1 *zxdg_output_v1, int32_t x, int32_t y)
{
}

static void xdg_output_logical_size(void *data,
    struct zxdg_output_v1 *zxdg_output_v1, int32_t width, int32_t height)
{
    struct output *output = data;
    output->width = width;
    output->height = height;
}

static void xdg_output_done(void *data, struct zxdg_output_v1 *zxdg_output_v1) {
}

static void xdg_output_name(void *data, struct zxdg_output_v1 *zxdg_output_v1,
    const char *name)
{
    struct output *output = data;
    free(output->name);
    output->name = strdup(name);
}

static void xdg_output_description(void *data, struct zxdg_output_v1 *zxdg_output_v1,
    const char *description)
{
}

const struct zxdg_output_v1_listener xdg_output_listener = {
    .logical_position = xdg_output_logical_position,
    .logical_size = xdg_output_logical_size,
    .done = xdg_output_done,
    .name = xdg_output_name,
    .description = xdg_output_description,
};

static void wl_output_global(struct state *state, void *bound) {
    struct output *output = malloc(sizeof(struct output));
    *output = (struct output) {
        .state = state,
        .wl_output = bound,
    };
    if (state->have_all_globals) {
        output->xdg_output = zxdg_output_manager_v1_get_xdg_output(
            state->xdg_output_manager, output->wl_output);
        zxdg_output_v1_add_listener(
            output->xdg_output, &xdg_output_listener, output);
    }
    wl_output_add_listener(bound, &wl_output_listener, output);
    wl_list_insert(&state->outputs, &output->link);
}

static void wl_keyboard_keymap(void *data, struct wl_keyboard *wl_keyboard,
    uint32_t format, int32_t fd, uint32_t size)
{
    struct seat *seat = data;
    xkb_keymap_unref(seat->xkb_keymap);
    xkb_state_unref(seat->xkb_state);
    char *map_shm = mmap(NULL, size, PROT_READ, MAP_PRIVATE, fd, 0);
    seat->xkb_keymap = xkb_keymap_new_from_string(seat->state->xkb_context,
        map_shm, XKB_KEYMAP_FORMAT_TEXT_V1, XKB_KEYMAP_COMPILE_NO_FLAGS);
    munmap(map_shm, size);
    close(fd);
    seat->xkb_state = xkb_state_new(seat->xkb_keymap);
}

static void wl_keyboard_enter(void *data, struct wl_keyboard *wl_keyboard,
    uint32_t serial, struct wl_surface *surface, struct wl_array *keys)
{
}

static void wl_keyboard_leave(void *data, struct wl_keyboard *wl_keyboard,
    uint32_t serial, struct wl_surface *surface)
{
}

static void wl_keyboard_key(void *data, struct wl_keyboard *wl_keyboard,
    uint32_t serial, uint32_t time, uint32_t key, uint32_t state)
{
    if (state != WL_KEYBOARD_KEY_STATE_PRESSED)
        return;
    struct seat *seat = data;
    xkb_keysym_t keysym = xkb_state_key_get_one_sym(seat->xkb_state, key + 8);
    switch (keysym) {
    case XKB_KEY_Escape:
        seat->state->running = false;
        break;
    case XKB_KEY_n:
        split_left(seat->state);
        break;
    case XKB_KEY_e:
        split_down(seat->state);
        break;
    case XKB_KEY_i:
        split_up(seat->state);
        break;
    case XKB_KEY_o:
        split_right(seat->state);
        break;
    case XKB_KEY_Return:
        click(seat->state);
        quit(seat->state);
        break;
    }
}

static void wl_keyboard_modifiers(void *data, struct wl_keyboard *wl_keyboard,
    uint32_t serial, uint32_t mods_depressed, uint32_t mods_latched,
    uint32_t mods_locked, uint32_t group)
{
}

static void wl_keyboard_repeat_info(void *data, struct wl_keyboard *wl_keyboard,
    int32_t rate, int32_t delay)
{
}

const struct wl_keyboard_listener wl_keyboard_listener = {
    .keymap = wl_keyboard_keymap,
    .enter = wl_keyboard_enter,
    .leave = wl_keyboard_leave,
    .key = wl_keyboard_key,
    .modifiers = wl_keyboard_modifiers,
    .repeat_info = wl_keyboard_repeat_info,
};

static void wl_seat_capabilities(void *data, struct wl_seat *wl_seat,
    uint32_t capabilities)
{
    struct seat *seat = data;
    if ((capabilities & WL_SEAT_CAPABILITY_KEYBOARD)
        && seat->wl_keyboard == NULL)
    {
        seat->wl_keyboard = wl_seat_get_keyboard(wl_seat);
        wl_keyboard_add_listener(seat->wl_keyboard, &wl_keyboard_listener, seat);
    }
}

static void wl_seat_name(void *data, struct wl_seat *wl_seat, const char *name)
{
}

const struct wl_seat_listener wl_seat_listener = {
    .capabilities = wl_seat_capabilities,
    .name = wl_seat_name,
};

static void wl_seat_global(struct state *state, void *bound) {
    struct seat *seat = malloc(sizeof(struct seat));
    *seat = (struct seat) {
        .state = state,
        .wl_seat = bound,
    };
    wl_seat_add_listener(bound, &wl_seat_listener, seat);
    wl_list_insert(&state->seats, &seat->link);
}

static void wl_buffer_release(void *data, struct wl_buffer *wl_buffer) {
    struct buffer *buffer = data;
    buffer->in_use = false;
}

const struct wl_buffer_listener wl_buffer_listener = {
    .release = wl_buffer_release,
};

const struct global globals[] = {
    /* must stay sorted */
    {
        .interface = &wl_compositor_interface,
        .name = "wl_compositor",
        .version = 4,
        .is_singleton = true,
        .offset = offsetof(struct state, wl_compositor),
    },
    {
        .interface = &wl_output_interface,
        .name = "wl_output",
        .version = 3,
        .is_singleton = false,
        .callback = wl_output_global,
    },
    {
        .interface = &wl_seat_interface,
        .name = "wl_seat",
        .version = 7,
        .is_singleton = false,
        .callback = wl_seat_global,
    },
    {
        .interface = &wl_shm_interface,
        .name = "wl_shm",
        .version = 1,
        .is_singleton = true,
        .offset = offsetof(struct state, wl_shm),
    },
    {
        .interface = &zwlr_layer_shell_v1_interface,
        .name = "zwlr_layer_shell_v1",
        .version = 2,
        .is_singleton = true,
        .offset = offsetof(struct state, wlr_layer_shell),
    },
    {
        .interface = &zwlr_virtual_pointer_manager_v1_interface,
        .name = "zwlr_virtual_pointer_manager_v1",
        .version = 2,
        .is_singleton = true,
        .offset = offsetof(struct state, wlr_virtual_pointer_manager),
    },
    {
        .interface = &zxdg_output_manager_v1_interface,
        .name = "zxdg_output_manager_v1",
        .version = 3,
        .is_singleton = true,
        .offset = offsetof(struct state, xdg_output_manager),
    },
};

const size_t globals_len = sizeof(globals) / sizeof(globals[0]);

static void wl_registry_global(void *data, struct wl_registry *wl_registry,
    uint32_t name, const char *interface, uint32_t version)
{
    struct global global = { .name = interface };

    struct global *found = bsearch(&global, globals,
        sizeof(globals) / sizeof(struct global),
        sizeof(struct global), global_compare);

    if (!found)
        return;

    struct wl_proxy **location =
        (struct wl_proxy **)((uintptr_t)data + found->offset);

    if (!found->is_singleton) {
        if (found->callback) {
            void *bound = wl_registry_bind(wl_registry, name,
                found->interface, found->version);
            found->callback(data, bound);
        }
        return;
    }

    if (!*location) {
        *location = wl_registry_bind(wl_registry, name,
            found->interface, found->version);
    }
}

static void wl_registry_global_remove(void *data,
    struct wl_registry *wl_registry, uint32_t name)
{
}

const struct wl_registry_listener wl_registry_listener = {
    .global = wl_registry_global,
    .global_remove = wl_registry_global_remove,
};

static void wlr_layer_surface_configure(void *data,
    struct zwlr_layer_surface_v1 *zwlr_layer_surface_v1,
    uint32_t serial, uint32_t width, uint32_t height)
{
    struct state *state = data;
    state->surface_width = width;
    state->surface_height = height;
    zwlr_layer_surface_v1_ack_configure(zwlr_layer_surface_v1, serial);
    draw(state);
    update_pointer(state);
}

static void wlr_layer_surface_closed(void *data,
    struct zwlr_layer_surface_v1 *zwlr_layer_surface_v1)
{
    struct state *state = data;
    state->running = false;
}

const struct zwlr_layer_surface_v1_listener wlr_layer_surface_listener = {
    .configure = wlr_layer_surface_configure,
    .closed = wlr_layer_surface_closed,
};

