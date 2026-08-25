# Contributing to Foresight

Thanks for helping. This document is short on purpose: it covers the few
things that matter, not everything.

## Reporting bugs

Open an issue with what you did, what you expected, and what happened. If a
sync went wrong, include the transfer log if you can — the exact rsync command
shown at the top of a run is the single most useful piece of information.

## Translations

Foresight uses gettext, wired up the standard GNOME way:

- `po/POTFILES` lists every source that carries translatable strings: the
  Blueprint UI (`src/ui/window.blp`, whose `_("…")` calls become
  `translatable="yes"`) and the Rust sources that call `i18n`, `i18n_f`,
  `i18n_k`, `ni18n` or `gettext_noop` (see `crates/foresight/src/i18n.rs`).
- `po/LINGUAS` lists the shipped languages, one per line.
- A new or updated `po/<lang>.po` is compiled by Meson and installed into the
  `locale` directory at build time; `--keyword=*` flags in `po/meson.build`
  tell `xgettext` which call shapes to extract.

### Adding or updating a language

1. Regenerate the catalogue against the current sources:
   `xgettext --keyword=_ --keyword=i18n --keyword=i18n_f --keyword=i18n_k
   --keyword=ni18n:1,2 --keyword=gettext_noop --keyword=gettext
   --from-code=UTF-8 -o po/foresight.pot $(cat po/POTFILES)`.
2. Create or update your `.po` from the template:
   `msginit --locale=<locale> --input=po/foresight.pot` for a new language,
   or `msgmerge -U po/<lang>.po po/foresight.pot` for an existing one.
3. Translate, keeping the `{}`, `{name}` and `{n}` placeholders intact.
4. Add your locale to `po/LINGUAS` if new, and to the `<languages>` block in
   `data/io.github.superuser_miguel.Foresight.metainfo.xml`.
5. Validate before committing: `msgfmt --check --check-format po/<lang>.po`.

### Where each string lives

- UI text in `src/ui/window.blp` is marked `_("…")`; blueprint-compiler turns
  that into `translatable="yes"` and GTK resolves it at runtime.
- Strings in Rust that are shown verbatim call `i18n("…")`.
- Strings with positional placeholders (`{}`, like `format!`) call
  `i18n_f("… {}", &[arg])`.
- Strings with named placeholders (`{name}`) call
  `i18n_k("… {name} …", &[("name", value)])`.
- Singular/plural pairs use `ni18n(singular, plural, n)`.
- Strings stored in static tables (the capability registry in
  `crates/foresight/src/capabilities.rs`) are wrapped in `gettext_noop("…")`
  and translated at render time with `i18n(...)`.

## Code style

- `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings` must be
  clean; CI enforces both.
- `cargo test --workspace` must pass. One test (`cancel_maps_to_cancelled`)
  is timing-sensitive and can flake on fast machines; re-run it before
  assuming your change broke it.
- Commit messages are imperative, lowercase, ≤ 72 chars, no emoji.
- Run `meson setup builddir && meson test -C builddir` after touching
  Blueprint, Meson or AppStream files — the Meson suite validates the
  metainfo and the compiled UI.

## Building

See the README's "Build & run" section. Host builds need GTK4/libadwaita dev
headers, `blueprint-compiler`, Meson and rsync on `PATH`.
