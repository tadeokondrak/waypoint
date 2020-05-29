#include "waypoint.h"

#include <wayland-client.h>

#include <string.h>

static void draw_box(struct buffer *buffer,
    int x, int y, int width, int height, int32_t color)
{
    unsigned int *data = (unsigned int *)buffer->data;
    for (int y_ = y; y_ < y + height; y_++) {
        for (int x_ = x; x_ < x + width; x_++) {
            data[y_ * buffer->width + x_] = color;
        }
    }
}

static void draw_outline(struct buffer *buffer,
    int x, int y, int width, int height, int32_t color, int stroke)
{
    draw_box(buffer, x, y, width, stroke, color);
    draw_box(buffer, x, y, stroke, height, color);
    draw_box(buffer, x, y + height - stroke, width, stroke, color);
    draw_box(buffer, x + width - stroke, y, stroke, height, color);
}

void draw(struct state *state) {
    int factor = state->output->scale_factor;
    int width = state->surface_width * factor;
    int height = state->surface_height * factor;
    struct buffer *buffer = get_buffer(state, width, height);
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
