#include "waypoint.h"

#include <stdlib.h>

#include <wayland-client.h>

#include <unistd.h>
#include <sys/mman.h>

static struct buffer *create_buffer(struct state *state,
    int width, int height)
{
    struct buffer *buffer = malloc(sizeof(struct buffer));

    buffer->width = width;
    buffer->height = height;
    buffer->size = buffer->width * 4 * buffer->height;
    buffer->in_use = true;

    int fd = memfd_create("waypoint", MFD_CLOEXEC);
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

struct buffer *get_buffer(struct state *state, int width, int height) {
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

