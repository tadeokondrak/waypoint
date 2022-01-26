#include "buffer.h"
#include "waypoint.h"

#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <stdbool.h>

#include <time.h>
#include <unistd.h>
#include <sys/mman.h>

#include <xkbcommon/xkbcommon.h>
#include <wayland-client.h>
#include <linux/input-event-codes.h>

#include "wlr-layer-shell-unstable-v1-client-protocol.h"
#include "wlr-virtual-pointer-unstable-v1-client-protocol.h"
#include "xdg-output-unstable-v1-client-protocol.h"
#include "xdg-shell-client-protocol.h"

static void draw(struct waypoint_state *state);

static uint32_t time_ms(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_nsec / 1000;
}

static void update_pointer(struct waypoint_state *state) {
    zwlr_virtual_pointer_v1_motion_absolute(
        state->wlr_virtual_pointer, time_ms(),
        (state->output->width * state->x) + (state->output->width * state->width / 2),
        (state->output->height * state->y) + (state->output->height * state->height / 2),
        state->output->width, state->output->height);
    zwlr_virtual_pointer_v1_frame(state->wlr_virtual_pointer);
}

static void cut_left(struct waypoint_state *state, double value) {
    state->width *= value;
    update_pointer(state);
    draw(state);
}

static void cut_right(struct waypoint_state *state, double value) {
    state->x += state->width * (1.0 - value);
    state->width *= value;
    update_pointer(state);
    draw(state);
}

static void cut_up(struct waypoint_state *state, double value) {
    state->height *= value;
    update_pointer(state);
    draw(state);
}

static void cut_down(struct waypoint_state *state, double value) {
    state->y += state->height * (1.0 - value);
    state->height *= value;
    update_pointer(state);
    draw(state);
}

static void move_left(struct waypoint_state *state, double value) {
    state->x -= state->width * value;
    update_pointer(state);
    draw(state);
}

static void move_right(struct waypoint_state *state, double value) {
    state->x += state->width * value;
    update_pointer(state);
    draw(state);
}

static void move_up(struct waypoint_state *state, double value) {
    state->y -= state->height * value;
    update_pointer(state);
    draw(state);
}

static void move_down(struct waypoint_state *state, double value) {
    state->y += state->height * value;
    update_pointer(state);
    draw(state);
}

static void click(struct waypoint_state *state) {
    zwlr_virtual_pointer_v1_button(state->wlr_virtual_pointer, time_ms(), BTN_LEFT,
        WL_POINTER_BUTTON_STATE_PRESSED);
    zwlr_virtual_pointer_v1_frame(state->wlr_virtual_pointer);
    zwlr_virtual_pointer_v1_button(state->wlr_virtual_pointer, time_ms(), BTN_LEFT,
        WL_POINTER_BUTTON_STATE_RELEASED);
    zwlr_virtual_pointer_v1_frame(state->wlr_virtual_pointer);
}

static void quit(struct waypoint_state *state) {
    wl_display_flush(state->wl_display);
    state->running = false;
}

static int global_compare(const void *a, const void *b) {
    return strcmp(
        ((const struct waypoint_global *)a)->name,
        ((const struct waypoint_global *)b)->name);
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
    struct waypoint_output *output = data;
    output->scale_factor = factor;
}

static const struct wl_output_listener wl_output_listener = {
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
    struct waypoint_output *output = data;
    output->width = width;
    output->height = height;
}

static void xdg_output_done(void *data, struct zxdg_output_v1 *zxdg_output_v1) {
}

static void xdg_output_name(void *data, struct zxdg_output_v1 *zxdg_output_v1,
    const char *name)
{
    struct waypoint_output *output = data;
    free(output->name);
    output->name = strdup(name);
}

static void xdg_output_description(void *data, struct zxdg_output_v1 *zxdg_output_v1,
    const char *description)
{
}

static const struct zxdg_output_v1_listener xdg_output_listener = {
    .logical_position = xdg_output_logical_position,
    .logical_size = xdg_output_logical_size,
    .done = xdg_output_done,
    .name = xdg_output_name,
    .description = xdg_output_description,
};

static void wl_output_global(struct waypoint_state *state, void *bound) {
    struct waypoint_output *output = malloc(sizeof(struct waypoint_output));
    *output = (struct waypoint_output) {
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
    struct waypoint_seat *seat = data;
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
    struct waypoint_seat *seat = data;
    xkb_keysym_t keysym = xkb_state_key_get_one_sym(seat->xkb_state, key + 8);
    switch (keysym) {
    case XKB_KEY_Escape:
        quit(seat->state);
        break;
    case XKB_KEY_h:
        cut_left(seat->state, 0.5);
        break;
    case XKB_KEY_j:
        cut_down(seat->state, 0.5);
        break;
    case XKB_KEY_k:
        cut_up(seat->state, 0.5);
        break;
    case XKB_KEY_l:
        cut_right(seat->state, 0.5);
        break;
    case XKB_KEY_H:
        move_left(seat->state, 0.5);
        break;
    case XKB_KEY_J:
        move_down(seat->state, 0.5);
        break;
    case XKB_KEY_K:
        move_up(seat->state, 0.5);
        break;
    case XKB_KEY_L:
        move_right(seat->state, 0.5);
        break;
    case XKB_KEY_Return:
        update_pointer(seat->state);
        click(seat->state);
        quit(seat->state);
        break;
    }
}

static void wl_keyboard_modifiers(void *data, struct wl_keyboard *wl_keyboard,
    uint32_t serial, uint32_t mods_depressed, uint32_t mods_latched,
    uint32_t mods_locked, uint32_t group)
{
    struct waypoint_seat *seat = data;
    xkb_state_update_mask(seat->xkb_state,
        mods_depressed, mods_latched, mods_locked, 0, 0, group);
}

static void wl_keyboard_repeat_info(void *data, struct wl_keyboard *wl_keyboard,
    int32_t rate, int32_t delay)
{
}

static const struct wl_keyboard_listener wl_keyboard_listener = {
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
    struct waypoint_seat *seat = data;
    if ((capabilities & WL_SEAT_CAPABILITY_KEYBOARD)
        && seat->wl_keyboard == NULL)
    {
        seat->wl_keyboard = wl_seat_get_keyboard(wl_seat);
        wl_keyboard_add_listener(seat->wl_keyboard, &wl_keyboard_listener, seat);
    }
}

static void wl_seat_name(void *data, struct wl_seat *wl_seat, const char *name) {
}

static const struct wl_seat_listener wl_seat_listener = {
    .capabilities = wl_seat_capabilities,
    .name = wl_seat_name,
};

static void wl_seat_global(struct waypoint_state *state, void *bound) {
    struct waypoint_seat *seat = malloc(sizeof(struct waypoint_seat));
    *seat = (struct waypoint_seat) {
        .state = state,
        .wl_seat = bound,
    };
    wl_seat_add_listener(bound, &wl_seat_listener, seat);
    wl_list_insert(&state->seats, &seat->link);
}

static const struct waypoint_global globals[] = {
    /* must stay sorted */
    {
        .interface = &wl_compositor_interface,
        .name = "wl_compositor",
        .version = 4,
        .is_singleton = true,
        .offset = offsetof(struct waypoint_state, wl_compositor),
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
        .offset = offsetof(struct waypoint_state, wl_shm),
    },
    {
        .interface = &zwlr_layer_shell_v1_interface,
        .name = "zwlr_layer_shell_v1",
        .version = 2,
        .is_singleton = true,
        .offset = offsetof(struct waypoint_state, wlr_layer_shell),
    },
    {
        .interface = &zwlr_virtual_pointer_manager_v1_interface,
        .name = "zwlr_virtual_pointer_manager_v1",
        .version = 2,
        .is_singleton = true,
        .offset = offsetof(struct waypoint_state, wlr_virtual_pointer_manager),
    },
    {
        .interface = &zxdg_output_manager_v1_interface,
        .name = "zxdg_output_manager_v1",
        .version = 3,
        .is_singleton = true,
        .offset = offsetof(struct waypoint_state, xdg_output_manager),
    },
};

static void wl_registry_global(void *data, struct wl_registry *wl_registry,
    uint32_t name, const char *interface, uint32_t version)
{
    struct waypoint_global global = { .name = interface };

    struct waypoint_global *found = bsearch(&global, globals,
        sizeof(globals) / sizeof(struct waypoint_global),
        sizeof(struct waypoint_global), global_compare);

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

static const struct wl_registry_listener wl_registry_listener = {
    .global = wl_registry_global,
    .global_remove = wl_registry_global_remove,
};

static void wlr_layer_surface_configure(void *data,
    struct zwlr_layer_surface_v1 *zwlr_layer_surface_v1,
    uint32_t serial, uint32_t width, uint32_t height)
{
    struct waypoint_state *state = data;
    state->surface_width = width;
    state->surface_height = height;
    zwlr_layer_surface_v1_ack_configure(zwlr_layer_surface_v1, serial);
    update_pointer(state);
    draw(state);
}

static void wlr_layer_surface_closed(void *data,
    struct zwlr_layer_surface_v1 *zwlr_layer_surface_v1)
{
    struct waypoint_state *state = data;
    state->running = false;
}

static const struct zwlr_layer_surface_v1_listener wlr_layer_surface_listener = {
    .configure = wlr_layer_surface_configure,
    .closed = wlr_layer_surface_closed,
};

static void draw_outline(struct waypoint_buffer *buffer,
    int x, int y, int width, int height, int32_t color, int stroke)
{
    cairo_set_line_width(buffer->cairo, 1.0);
    cairo_set_source_rgba(buffer->cairo, 1.0, 1.0, 1.0, 1.0);
    cairo_rectangle(buffer->cairo, x, y, width, height);
    cairo_stroke(buffer->cairo);
}

static void draw(struct waypoint_state *state) {
    int factor = state->output->scale_factor;
    int width = state->surface_width * factor;
    int height = state->surface_height * factor;
    struct waypoint_buffer *buffer =
        waypoint_buffer_get(state->wl_shm, &state->buffers, width, height);
    memset(buffer->data, 0, buffer->size);
    for (int x = 0; x < state->grid_size; x++) {
        for (int y = 0; y < state->grid_size; y++) {
            int box_width = (width / state->grid_size) * state->width;
            int box_height = (height / state->grid_size) * state->height;
            int box_x = (width * state->x) + (x * box_width);
            int box_y = (height * state->y) + (y * box_height);
            draw_outline(buffer, box_x, box_y, box_width, box_height, state->color0, 1);
            draw_outline(buffer,
                box_x + factor, box_y + factor,
                box_width - 2 * factor, box_height - 2 * factor,
                state->color1, factor);
        }
    }
    wl_surface_set_buffer_scale(state->wl_surface, factor);
    wl_surface_attach(state->wl_surface, buffer->wl_buffer, 0, 0);
    wl_surface_damage_buffer(state->wl_surface, 0, 0, buffer->width, buffer->height);
    wl_surface_commit(state->wl_surface);
}

int main(void) {
    struct waypoint_state state = {
        .grid_size = 2,
        .color0 = 0xff000000,
        .color1 = 0xffffffff,
        .x = 0.0,
        .y = 0.0,
        .width = 1.0,
        .height = 1.0,
    };

    wl_list_init(&state.buffers);
    wl_list_init(&state.seats);
    wl_list_init(&state.outputs);

    state.xkb_context = xkb_context_new(XKB_CONTEXT_NO_FLAGS);

    state.wl_display = wl_display_connect(NULL);
    if (!state.wl_display) {
        perror("wl_display_connect");
        return EXIT_FAILURE;
    }

    state.wl_registry = wl_display_get_registry(state.wl_display);
    wl_registry_add_listener(state.wl_registry, &wl_registry_listener, &state);

    if (!wl_display_roundtrip(state.wl_display)) {
        perror("wl_display_roundtrip");
        return EXIT_FAILURE;
    }

    for (size_t i = 0; i < sizeof(globals) / sizeof(globals[0]); i++) {
        const struct waypoint_global *global = &globals[i];

        if (!global->is_singleton)
            continue;

        struct wl_proxy **location =
            (struct wl_proxy **)((uintptr_t)&state + global->offset);

        if (!*location) {
            fprintf(stderr, "required interface unsupported by compositor: %s\n",
                global->name);
            return EXIT_FAILURE;
        }
    }

    state.have_all_globals = true;

    struct waypoint_output *output;
    wl_list_for_each (output, &state.outputs, link) {
        output->xdg_output = zxdg_output_manager_v1_get_xdg_output(
            state.xdg_output_manager, output->wl_output);
        zxdg_output_v1_add_listener(output->xdg_output, &xdg_output_listener, output);
    }

    wl_display_roundtrip(state.wl_display);

    static const char output_name[] = "DP-1";

    bool found = false;
    wl_list_for_each (output, &state.outputs, link) {
        if (true || strcmp(output->name, output_name) == 0) {
            found = true;
            break;
        }
    }

    if (!found) {
        fprintf(stderr, "output %s doesn't exist\n", output_name);
        return EXIT_FAILURE;
    }

    state.output = output;

    state.wl_surface = wl_compositor_create_surface(state.wl_compositor);

    struct wl_region *region = wl_compositor_create_region(state.wl_compositor);
    wl_surface_set_input_region(state.wl_surface, region);
    wl_region_destroy(region);
    wl_surface_commit(state.wl_surface);

    state.wlr_layer_surface = zwlr_layer_shell_v1_get_layer_surface(
        state.wlr_layer_shell, state.wl_surface, output->wl_output,
        ZWLR_LAYER_SHELL_V1_LAYER_OVERLAY, "waypoint");

    zwlr_layer_surface_v1_set_size(state.wlr_layer_surface, 0, 0);
    zwlr_layer_surface_v1_set_anchor(state.wlr_layer_surface,
        ZWLR_LAYER_SURFACE_V1_ANCHOR_TOP
        | ZWLR_LAYER_SURFACE_V1_ANCHOR_BOTTOM
        | ZWLR_LAYER_SURFACE_V1_ANCHOR_LEFT
        | ZWLR_LAYER_SURFACE_V1_ANCHOR_RIGHT);
    zwlr_layer_surface_v1_set_exclusive_zone(state.wlr_layer_surface, -1);
    zwlr_layer_surface_v1_set_keyboard_interactivity(
        state.wlr_layer_surface, true);
    zwlr_layer_surface_v1_add_listener(
        state.wlr_layer_surface, &wlr_layer_surface_listener, &state);

    state.wlr_virtual_pointer =
        zwlr_virtual_pointer_manager_v1_create_virtual_pointer_with_output(
            state.wlr_virtual_pointer_manager, NULL, output->wl_output);

    wl_surface_commit(state.wl_surface);
    wl_display_roundtrip(state.wl_display);

    state.running = true;
    while (state.running && wl_display_dispatch(state.wl_display) != -1) {
    }
}
