project(
  'foresight',
  version: '0.1.0',
  license: 'GPL-3.0-or-later',
  meson_version: '>= 0.59.0',
)

# Sanity-check the runtime libs are present (cargo does the actual linking).
dependency('gtk4', version: '>= 4.10')
dependency('libadwaita-1', version: '>= 1.4')

gnome = import('gnome')

application_id = 'io.github.superuser_miguel.Foresight'
prefix = get_option('prefix')
bindir = prefix / get_option('bindir')
datadir = prefix / get_option('datadir')
pkgdatadir = datadir / meson.project_name()

# ---------------------------------------------------------------- Blueprint
# .blp is the source of truth; compile to .ui in the build tree (never commit
# the generated XML).
blueprint_compiler = find_program('blueprint-compiler')

blueprints = custom_target(
  'blueprints',
  input: files('src/ui/window.blp'),
  output: 'window.ui',
  command: [
    blueprint_compiler, 'batch-compile',
    '@OUTDIR@',
    meson.current_source_dir() / 'src' / 'ui',
    '@INPUT@',
  ],
)

# ---------------------------------------------------------------- GResource
gnome.compile_resources(
  'foresight',
  'data/foresight.gresource.xml',
  gresource_bundle: true,
  dependencies: blueprints,
  source_dir: meson.current_build_dir(),
  install: true,
  install_dir: pkgdatadir,
)

# ---------------------------------------------------------------- Desktop file
desktop_conf = configuration_data()
desktop_conf.set('app_id', application_id)
configure_file(
  input: 'data/io.github.superuser_miguel.Foresight.desktop.in',
  output: application_id + '.desktop',
  configuration: desktop_conf,
  install: true,
  install_dir: datadir / 'applications',
)

# ---------------------------------------------------------------- Icon
install_data(
  'data/icons/hicolor/scalable/apps/io.github.superuser_miguel.Foresight.svg',
  install_dir: datadir / 'icons' / 'hicolor' / 'scalable' / 'apps',
)

# ---------------------------------------------------------------- AppStream
metainfo_file = 'data/' + application_id + '.metainfo.xml'
install_data(
  metainfo_file,
  install_dir: datadir / 'metainfo',
)

# Validate the metainfo at test time when a validator is available (offline so
# CI/Flatpak builds don't reach the network for the screenshot URLs).
appstreamcli = find_program('appstreamcli', required: false)
if appstreamcli.found()
  test(
    'metainfo-validate',
    appstreamcli,
    args: ['validate', '--no-net', '--explain', files(metainfo_file)],
  )
endif

# ---------------------------------------------------------------- Cargo build
cargo = find_program('cargo')
cargo_build = find_program('build-aux/cargo-build.sh')

custom_target(
  'cargo-build',
  build_by_default: true,
  build_always_stale: true,
  output: meson.project_name(),
  console: true,
  install: true,
  install_dir: bindir,
  command: [
    cargo_build,
    cargo.full_path(),
    meson.current_source_dir() / 'Cargo.toml',
    meson.current_build_dir() / 'cargo-target',
    get_option('profile'),
    meson.project_name(),
    '@OUTPUT@',
    application_id,
    pkgdatadir,
    meson.project_version(),
  ],
)

gnome.post_install(
  gtk_update_icon_cache: true,
  update_desktop_database: true,
)
