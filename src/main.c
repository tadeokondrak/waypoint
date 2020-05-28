#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <stdbool.h>

#include <unistd.h>
#include <sys/mman.h>

#include <wayland-client.h>

#include "wlr-layer-shell-unstable-v1-client-protocol.h"
#include "xdg-shell-client-protocol.h"

struct state {
    bool running;

    struct wl_display *wl_display;
    struct wl_registry *wl_registry;

    struct wl_shm *wl_shm;
    struct wl_compositor *wl_compositor;
    struct zwlr_layer_shell_v1 *wlr_layer_shell;

    struct wl_surface *wl_surface;
    struct zwlr_layer_surface_v1 *wlr_layer_surface;

    struct wl_list buffers;
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
        "wl_compositor",
        4,
        offsetof(struct state, wl_compositor),
        &wl_compositor_interface,
    },
    {
        "wl_shm",
        1,
        offsetof(struct state, wl_shm),
        &wl_shm_interface,
    },
    {
        "zwlr_layer_shell_v1",
        2,
        offsetof(struct state, wlr_layer_shell),
        &zwlr_layer_shell_v1_interface,
    },
};

struct buffer {
    struct state *state;
    struct wl_list link;

    struct wl_buffer *wl_buffer;
    int32_t width, height;

    unsigned char *data;
    size_t size;

    bool in_use;
};

static void wl_buffer_release(void *data, struct wl_buffer *wl_buffer) {
    struct buffer *buffer = data;
    buffer->in_use = false;
}

static const struct wl_buffer_listener wl_buffer_listener = {
    .release = wl_buffer_release,
};

static struct buffer *create_buffer(struct state *state,
    int32_t width, int32_t height)
{
    struct buffer *buffer = malloc(sizeof(struct buffer));

    buffer->width = width;
    buffer->height = height;
    buffer->size = buffer->width * 4 * buffer->height;
    buffer->in_use = true;

    int fd = memfd_create("wkeynav", MFD_CLOEXEC);
    int rc = ftruncate(fd, buffer->size); (void)rc;

    buffer->data =
        mmap(NULL, buffer->size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);

    struct wl_shm_pool *pool =
        wl_shm_create_pool(state->wl_shm, fd, buffer->size);

    buffer->wl_buffer = wl_shm_pool_create_buffer(
        pool, 0, buffer->width, buffer->height,
        buffer->width * 4, WL_SHM_FORMAT_ARGB8888);

    close(fd);
    wl_shm_pool_destroy(pool);

    wl_buffer_add_listener(buffer->wl_buffer, &wl_buffer_listener, buffer);
    wl_list_insert(&state->buffers, &buffer->link);

    return buffer;
}

static void destroy_buffer(struct buffer *buffer) {
    munmap(buffer->data, buffer->size);
    wl_buffer_destroy(buffer->wl_buffer);
    wl_list_remove(&buffer->link);
    free(buffer);
}

static struct buffer *get_buffer(struct state *state,
    int32_t width, int32_t height)
{
    bool found = false;
    struct buffer *buffer, *tmp;
    wl_list_for_each_safe (buffer, tmp, &state->buffers, link) {
        if (buffer->in_use)
            continue;

        if (buffer->width != width || buffer->height != height) {
            destroy_buffer(buffer);
            continue;
        }

        found = true;
        break;
    }

    if (!found)
        buffer = create_buffer(state, width, height);

    buffer->in_use = true;
    return buffer;
}

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

static void draw(struct state *state, int32_t width, int32_t height);

static void wlr_layer_surface_configure(void *data,
    struct zwlr_layer_surface_v1 *zwlr_layer_surface_v1, uint32_t serial,
    uint32_t width, uint32_t height)
{
    struct state *state = data;
    draw(state, width, height);
    zwlr_layer_surface_v1_ack_configure(zwlr_layer_surface_v1, serial);
    wl_surface_commit(state->wl_surface);
}

static void wlr_layer_surface_closed(void *data,
    struct zwlr_layer_surface_v1 *zwlr_layer_surface_v1)
{
    struct state *state = data;
    state->running = false;
}

static const struct zwlr_layer_surface_v1_listener wlr_layer_surface_listener = {
    .configure = wlr_layer_surface_configure,
    .closed = wlr_layer_surface_closed,
};

static void draw(struct state *state, int32_t width, int32_t height) {
    struct buffer *buffer = get_buffer(state, width, height);
    memset(buffer->data, 0x5F, buffer->size);
    wl_surface_attach(state->wl_surface, buffer->wl_buffer, 0, 0);
    wl_surface_damage_buffer(state->wl_surface, 0, 0, buffer->width, buffer->height);
}

int main(void) {
    struct state state = {.running = true};
    wl_list_init(&state.buffers);

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
            fprintf(stderr, "required interface unsupported by compositor: %s\n",
                global->name);
            return EXIT_FAILURE;
        }
    }

    state.wl_surface = wl_compositor_create_surface(state.wl_compositor);

    struct wl_region *region = wl_compositor_create_region(state.wl_compositor);
    wl_surface_set_input_region(state.wl_surface, region);
    wl_region_destroy(region);

    state.wlr_layer_surface = zwlr_layer_shell_v1_get_layer_surface(
        state.wlr_layer_shell, state.wl_surface, NULL,
        ZWLR_LAYER_SHELL_V1_LAYER_OVERLAY, "wkeynav");

    zwlr_layer_surface_v1_set_size(state.wlr_layer_surface, 0, 0);
    zwlr_layer_surface_v1_set_anchor(state.wlr_layer_surface,
        ZWLR_LAYER_SURFACE_V1_ANCHOR_TOP | ZWLR_LAYER_SURFACE_V1_ANCHOR_BOTTOM
        | ZWLR_LAYER_SURFACE_V1_ANCHOR_LEFT | ZWLR_LAYER_SURFACE_V1_ANCHOR_RIGHT);
    zwlr_layer_surface_v1_set_exclusive_zone(state.wlr_layer_surface, -1);
    zwlr_layer_surface_v1_set_keyboard_interactivity(state.wlr_layer_surface, false);
    zwlr_layer_surface_v1_add_listener(
        state.wlr_layer_surface, &wlr_layer_surface_listener, &state);

    wl_surface_commit(state.wl_surface);
    wl_display_roundtrip(state.wl_display);

    while (state.running && wl_display_dispatch(state.wl_display) != -1)
        ;
}
