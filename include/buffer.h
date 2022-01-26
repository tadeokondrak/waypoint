#pragma once

#include <stdbool.h>
#include <stddef.h>

#include <cairo.h>
#include <wayland-client.h>

struct waypoint_buffer {
    struct wl_list link;
    struct wl_buffer *wl_buffer;
    int width, height, stride;
    unsigned char *data;
    size_t size;
    bool in_use;
    cairo_t *cairo;
    cairo_surface_t *cairo_surface;
};

struct waypoint_buffer *waypoint_buffer_get(
    struct wl_shm *wl_shm, struct wl_list *buffers, int width, int height);
void waypoint_buffer_destroy(struct waypoint_buffer *buffer);
