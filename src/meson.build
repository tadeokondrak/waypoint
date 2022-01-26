waypoint_src += files(
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
        protocols_dep,
    ],
)
