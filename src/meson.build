waypoint_src += files(
    'buffer.c',
    'waypoint.c',
)

waypoint_bin = executable(
    'waypoint',
    waypoint_src,
    install: true,
    include_directories: waypoint_inc,
    dependencies: [
        wayland_client_dep,
        xkbcommon_dep,
        cairo_dep,
        protocols_dep,
    ],
)
