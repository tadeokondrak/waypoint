#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <stdbool.h>

#include <time.h>

#include <xkbcommon/xkbcommon.h>
#include <wayland-client.h>
#include <linux/input-event-codes.h>

#include "wlr-layer-shell-unstable-v1-client-protocol.h"
#include "wlr-virtual-pointer-unstable-v1-client-protocol.h"
#include "xdg-output-unstable-v1-client-protocol.h"
#include "xdg-shell-client-protocol.h"

#include "waypoint.h"

static uint32_t time_ms(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_nsec / 1000;
}

void update_pointer(struct state *state) {
    zwlr_virtual_pointer_v1_motion_absolute(
        state->wlr_virtual_pointer, time_ms(),
        (state->output->width * state->x) + (state->output->width * state->width / 2),
        (state->output->height * state->y) + (state->output->height * state->height / 2),
        state->output->width, state->output->height);
    zwlr_virtual_pointer_v1_frame(state->wlr_virtual_pointer);
}

void split_left(struct state *state) {
    state->width *= 0.5;
    update_pointer(state);
    draw(state);
}

void split_right(struct state *state) {
    state->width *= 0.5;
    state->x += state->width;
    update_pointer(state);
    draw(state);
}

void split_up(struct state *state) {
    state->height *= 0.5;
    update_pointer(state);
    draw(state);
}

void split_down(struct state *state) {
    state->height *= 0.5;
    state->y += state->height;
    update_pointer(state);
    draw(state);
}

void click(struct state *state) {
    zwlr_virtual_pointer_v1_button(state->wlr_virtual_pointer, time_ms(), BTN_LEFT,
        WL_POINTER_BUTTON_STATE_PRESSED);
    zwlr_virtual_pointer_v1_frame(state->wlr_virtual_pointer);
    zwlr_virtual_pointer_v1_button(state->wlr_virtual_pointer, time_ms(), BTN_LEFT,
        WL_POINTER_BUTTON_STATE_RELEASED);
    zwlr_virtual_pointer_v1_frame(state->wlr_virtual_pointer);
    wl_display_flush(state->wl_display);
}

void quit(struct state *state) {
    state->running = false;
}

int main(void) {
    struct state state = {
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

    for (size_t i = 0; i < globals_len; i++) {
        const struct global *global = &globals[i];

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

    struct output *output;
    wl_list_for_each (output, &state.outputs, link) {
        output->xdg_output = zxdg_output_manager_v1_get_xdg_output(
            state.xdg_output_manager, output->wl_output);
        zxdg_output_v1_add_listener(output->xdg_output, &xdg_output_listener, output);
    }

    wl_display_roundtrip(state.wl_display);

    static const char output_name[] = "DP-1";
    bool found = false;
    wl_list_for_each (output, &state.outputs, link) {
        if (strcmp(output->name, output_name) == 0) {
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
