project(
    'waypoint',
    'c',
    default_options: [
        'c_std=c11',
        'warning_level=2',
    ]
)

add_project_arguments(
    [
        '-D_GNU_SOURCE',
        '-Wno-unused-parameter',
    ],
    language: 'c',
)

cc = meson.get_compiler('c')

rt_dep = cc.find_library('rt')
wayland_client_dep = dependency('wayland-client')
wayland_protocols_dep = dependency('wayland-protocols')
xkbcommon_dep = dependency('xkbcommon')

subdir('protocol')

waypoint_bin = executable(
    'waypoint',
    'waypoint.c',
    dependencies: [
        wayland_client_dep,
        xkbcommon_dep,
        protocols_dep,
    ],
)
