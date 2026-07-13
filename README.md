<p align="center">
  <img src="data/icons/hicolor/scalable/apps/io.github.superuser_miguel.Foresight.svg" alt="Foresight icon" width="96" height="96">
</p>

<h1 align="center">Foresight</h1>

<p align="center"><strong>See exactly what rsync will change — before a single byte moves.</strong></p>

Foresight is a GTK4 / libadwaita front-end for [rsync](https://rsync.samba.org/)
on Linux. It is a GNOME-native app built around one idea: **every sync can be
previewed as a grouped change list — created / updated / deleted / attribute-only —
so you know exactly what will happen before you run it.**

Foresight never reimplements rsync. It composes an argv vector, spawns the
**bundled** rsync as a subprocess, and parses its output into structured events.
No shell strings are ever constructed, and the engine version is pinned — the
version pin *is* the behaviour contract.

> Status: working, and building toward a first release. Multi-source transfers,
> the dry-run preview, live progress, cancel, and the advanced flag set all
> function today in a sandboxed Flatpak. See the [roadmap](#roadmap).

---

## Why Foresight?

A GNOME-native rsync front-end whose headline is *seeing changes before they
happen*, running fully sandboxed:

| | Change preview before sync | GNOME-native (GTK4/Adwaita) | Sandboxed (portals only) | Bundled, pinned engine |
|---|:---:|:---:|:---:|:---:|
| **Grsync** | ~ (raw dry-run log) | ✗ (GTK3) | ✗ | ✗ |
| **Back In Time** | ✗ | ✗ | ✗ | ✗ |
| **rsync CLI** (`rsync -n`) | ~ (raw text) | ✗ | ✗ | n/a |
| **Foresight** | ✓ (grouped list) | ✓ | ✓ | ✓ |

Where Foresight aims to *win*, not just match:

- **Dry-run preview as a first-class view** — the change list is grouped by kind,
  deletions rendered destructively, so a mistake is obvious *before* it runs.
- **`--delete` can never surprise you** — turning it on runs a fresh dry run and
  makes you confirm the exact list of files it will remove.
- **Sandboxed by design** — a Flatpak that bundles rsync and reaches your files
  only through XDG portals. No `--filesystem=home`, no blanket host access.

## Features

- **Preview any sync** as a grouped change list (created / updated / deleted /
  attributes-only) from a real `rsync -a -n -i` dry run — before anything moves.
- **Multiple sources from anywhere** — add files *and* folders from different
  locations (e.g. one from Downloads, one from Documents), each with a remove
  button, or drop them in from the file manager.
- **Right transfer semantics, automatically** — a single folder mirrors its
  *contents* into the destination; a file or several items are *collected* into it.
- **Live transfer** — a real progress bar, current-file label, a log that opens
  with the exact `rsync …` command, and **cancel** at any time.
- **`--delete` with a confirmation** that lists the exact deletions from the dry run.
- **Advanced options** for the flags you actually reach for:
  - **Move files** — `--remove-source-files` (remove each source after it transfers).
  - **Bandwidth limit** — `--bwlimit` with a unit picker (**KB/s · MB/s · GB/s**),
    so capping a big transfer to a disk's speed is `85` + `MB/s`, not `85000`.
  - **Exclude patterns** — each becomes a `--exclude=`.
  - **Extra arguments** — a free-text escape hatch for any other rsync switch.
- **New Job** — clear the whole form for the next transfer in one click.
- Ships as a **Flatpak** with rsync **3.4.4** bundled — **portals only, no host
  filesystem access** by design.

## Power-user switches

The Advanced → **Extra arguments** field is a free-text escape hatch: whatever
you type is tokenised on whitespace and passed straight to rsync as argv (never
through a shell). It's for the long tail of rsync flags that don't each earn a
dedicated control. A few useful ones:

| Switch | What it does |
|---|---|
| `--checksum` | Compare by checksum, not size+mtime — catches same-size edits |
| `--compress` (`-z`) | Compress data in transit (useful over slow links) |
| `--partial --append-verify` | Resume interrupted transfers safely |
| `--backup --backup-dir=DIR` | Keep replaced/deleted files instead of losing them |
| `--chmod=…`, `--chown=…` | Rewrite permissions / ownership on the destination |
| `--max-size=`, `--min-size=` | Skip files outside a size range |

> Extra arguments are placed **before** Foresight's own reporting flags
> (`--info=progress2`, `--out-format`, `-n -i`), so they can never break the
> change parser. Tokens are split on spaces — there is no shell, so brace
> expansion like `--exclude={a,b}` does **not** apply; list patterns separately.

## Install

> No binary release yet — build from source below. A `.flatpak` bundle and a
> self-hosted repo with automatic updates are on the [roadmap](#roadmap).

## Build from source

Foresight builds and runs entirely inside the GNOME Flatpak sandbox:

```sh
flatpak install flathub org.gnome.Platform//49 org.gnome.Sdk//49 \
    org.freedesktop.Sdk.Extension.rust-stable//25.08
flatpak-builder --user --install --force-clean build-dir \
    io.github.superuser_miguel.Foresight.yml
flatpak run io.github.superuser_miguel.Foresight
```

For host development (needs `gtk4-devel`, `libadwaita-devel`,
`blueprint-compiler`, Meson, and `rsync` on `PATH`):

```sh
cargo test                                   # parser + argv + engine tests
cargo clippy --all-targets -- -D warnings
meson setup builddir -Dprofile=debug && meson compile -C builddir
```

## Roadmap

- [x] **Dry-run preview** — grouped change list from a real `-n -i` run.
- [x] **Multi-source transfers** — files and folders from different locations,
      with per-item remove and drag-and-drop.
- [x] **Single-file and single-folder-mirror** semantics, chosen automatically.
- [x] **Live progress, cancel, and a verbatim log** of the exact command.
- [x] **`--delete` with a confirmation** listing the deletions from the dry run.
- [x] **Advanced options** — move (`--remove-source-files`), bandwidth limit
      with units (`--bwlimit`), excludes, and a free-form extra-arguments field.
- [ ] **Help / capability disclosure** — an in-app, honest inventory of exactly
      which rsync flags Foresight exposes this release, cross-referenced to the
      bundled `rsync --help` (registry-driven, test-enforced so it can't drift).
- [x] **Saved presets** — store an Advanced-option set (e.g. a throttled
      `--remove-source-files` move) and reapply it in one click.
- [ ] **Excludes editor** — manage exclude/include rules as a list, not a field.
- [ ] **Remote sync over SSH** — rsync to/from a remote host, with credentials
      via the keyring.
- [ ] **AppStream metainfo + screenshots**, and a **`.flatpak` bundle on
      [GitHub Releases](https://github.com/superuser-miguel/foresight/releases)**
      (the landing page is published on GitHub Pages).
- [ ] **Self-hosted Flatpak repo** with automatic updates (a signed OSTree repo
      + `.flatpakref`), so `flatpak update` pulls new releases directly.

## Acknowledgements

- **[rsync](https://rsync.samba.org/)** by Andrew Tridgell, Wayne Davison and
  contributors — the engine Foresight bundles and drives. Foresight is *not* a
  fork; rsync is built unmodified as a separate Flatpak module.
- Built with **[gtk4-rs](https://gtk-rs.org/)**, **libadwaita**,
  **[Blueprint](https://gnome.pages.gitlab.gnome.org/blueprint-compiler/)**, and
  **Meson** — following the conventions of Amberol, Fractal and friends.

## License

Foresight is **GPL-3.0-or-later**. The bundled rsync remains its own
GPL-3.0-or-later work, built as a separate module.
