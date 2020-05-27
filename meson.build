project(
    'wkeynav',
    'c',
    default_options: [
        'c_std=c11',
        'warning_level=2',
    ]
)

add_project_arguments(
    [
        # '-D_POSIX_C_SOURCE=200809L',
        '-Wno-unused-parameter',
    ],
    language: 'c',
)

wayland_client_dep = dependency('wayland-client')
wayland_protocols_dep = dependency('wayland-protocols')

subdir('protocol')
subdir('src')

wkeynav_bin = executable(
    'wkeynav',
    src,
    include_directories: [inc],
    dependencies: [
        wayland_client_dep,
        protocols_dep,
    ],
)
