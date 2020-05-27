#include <stdlib.h>
#include <stdio.h>
#include <string.h>

#include <wayland-client.h>

#include "wlr-layer-shell-unstable-v1-client-protocol.h"
#include "xdg-shell-client-protocol.h"

struct wkeynav {
    struct wl_display *wl_display;
    struct wl_registry *wl_registry;
    struct wl_shm *wl_shm;
    struct wl_compositor *wl_compositor;
    struct zwlr_layer_shell_v1 *wlr_layer_shell;
};

struct global {
    const char *name;
    int version;
    size_t offset;
    const struct wl_interface *interface;
};

static int global_compare(const void *a, const void *b) {
    return strcmp(
        ((struct global *)a)->name,
        ((struct global *)b)->name
    );
}

struct global globals[] = {
    /* must stay sorted */
    {
        "wl_compositor", 4,
        offsetof(struct wkeynav, wl_compositor),
        &wl_compositor_interface,
    },
    {
        "wl_shm", 1,
        offsetof(struct wkeynav, wl_shm),
        &wl_shm_interface,
    },
    {
        "zwlr_layer_shell_v1", 2,
        offsetof(struct wkeynav, wlr_layer_shell),
        &zwlr_layer_shell_v1_interface,
    },
};

static void wl_registry_global(void *data, struct wl_registry *wl_registry,
    uint32_t name, const char *interface, uint32_t version)
{
    struct global global = { interface, 0, 0, NULL };

    struct global *found = bsearch(&global, globals,
        sizeof(globals) / sizeof(struct global),
        sizeof(struct global), global_compare);

    if (!found)
        return;

    struct wl_proxy **location =
        (struct wl_proxy **)((uintptr_t)data + found->offset);

    if (!*location) {
        *location = wl_registry_bind(
            wl_registry, name, found->interface, found->version);
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

int main(void) {
    struct wkeynav state = {0};

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

    for (struct global *global = &globals[0];
        global != &globals[sizeof(globals) / sizeof(struct global)];
        global++)
    {
        struct wl_proxy **location =
            (struct wl_proxy **)((uintptr_t)&state + global->offset);

        if (!*location) {
            fprintf(stderr, "required protocol unsupported by compositor: %s\n",
                global->name);
            return EXIT_FAILURE;
        }
    }
}
