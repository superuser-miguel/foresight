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

Rust · GTK4 · gtk4-rs · libadwaita · Blueprint · Meson · Flatpak, with rsync
**3.4.4** bundled and version-pinned. See [`PLAN.md`](PLAN.md) for the phased
build plan and the guardrails it holds to.

> **Status: released and self-hosted.** Multi-source transfers, the grouped
> dry-run preview, live progress with a structured streaming log, cancel,
> `--delete` with confirmation, the advanced flag set with saved presets, a
> managed exclude-rule list, and an in-app capability inventory all work today
> in a sandboxed Flatpak, covered by **44 tests** across the workspace — with
> `rsync-events` staying UI-free. Installed from the project's own **GPG-signed
> repository** so `flatpak update` works, with a standalone bundle on
> [GitHub Releases](https://github.com/superuser-miguel/foresight/releases) for
> offline installs and the project page on
> [GitHub Pages](https://superuser-miguel.github.io/foresight/) — **not** Flathub.

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
- **What you add is what you get** — a folder lands in the destination *as that
  folder*, a file as that file. No surprise spills. If you want rsync's other
  form — a single folder's *contents* copied straight into the destination —
  Advanced → **Sync folder contents** does exactly that.
- **Live transfer** — a real progress bar, current-file label, and a **structured
  streaming log** (one typed row per file and rsync message, not a wall of text)
  that opens with the exact `rsync …` command — with **cancel** at any time.
- **`--delete` with a confirmation** that lists the exact deletions from the dry run.
- **Saved presets** — store an Advanced-option set (e.g. a throttled
  `--remove-source-files` move) and reapply it in one click. Paths are never saved.
- **Know exactly what it can do** — a *What Foresight Can Do* dialog: an honest,
  in-app inventory of every rsync flag this build exposes, driven by a registry
  that a test keeps in lockstep with the code, plus the bundled `rsync --help`.
- **Advanced options** for the flags you actually reach for:
  - **Move files** — `--remove-source-files` (remove each source after it transfers).
  - **Bandwidth limit** — `--bwlimit` with a unit picker (**KB/s · MB/s · GB/s**),
    so capping a big transfer to a disk's speed is `85` + `MB/s`, not `85000`.
  - **Exclude rules** — a managed list, one `--exclude=` each. Rules are passed
    whole, so `My Documents/` is one rule rather than two broken ones.
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

**Not on Flathub** — Foresight ships from its own **signed repository**, with
the project page on [GitHub Pages](https://superuser-miguel.github.io/foresight/).

### Recommended — the repository (gets `flatpak update`)

```sh
flatpak install --user https://superuser-miguel.github.io/foresight/foresight.flatpakref
flatpak run io.github.superuser_miguel.Foresight
```

That one command adds the remote and installs the app, so new versions arrive
with a normal `flatpak update` — no re-downloading a bundle. The remote is
GPG-signed with key `D67DB8E03D50A8C0`; flatpak verifies every pull against the
key embedded in the `.flatpakref` and refuses the remote if it doesn't match.

To add the remote without installing anything:

```sh
flatpak remote-add --user --if-not-exists \
    foresight https://superuser-miguel.github.io/foresight/foresight.flatpakrepo
```

### Alternative — the standalone bundle

A `Foresight.flatpak` bundle is also published on
[GitHub Releases](https://github.com/superuser-miguel/foresight/releases/latest)
for offline or air-gapped installs:

```sh
flatpak install --user Foresight.flatpak
```

> A bundle install has **no origin to pull from**, so `flatpak update` cannot
> upgrade it — moving versions means downloading the next bundle by hand. Prefer
> the repository unless you specifically need a single offline file.

Either way you need the GNOME runtime it builds against; if you don't have it:

```sh
flatpak install flathub org.gnome.Platform//49
```

Release tags are GPG-signed with the same key. Verify with
`git verify-tag v0.1.2`.

## Layout

```
crates/rsync-events/   UI-free parser: itemize / progress / stats + exit classifier
crates/foresight/      GTK4/libadwaita app (binary: `foresight`)
data/                  Blueprint UI, gresource, desktop + AppStream metainfo, icon
build-aux/             Meson → cargo bridge
reference/             rsync_events.py — the parser's executable spec
```

`rsync-events` has **zero** GTK/GLib dependencies (regex + `once_cell` only), so
the output-parsing contract is testable on its own — and
`reference/rsync_events.py` is kept in lockstep with it.

## Build & run

### Flatpak (how it ships)

```sh
flatpak install flathub org.gnome.Platform//49 org.gnome.Sdk//49 \
    org.freedesktop.Sdk.Extension.rust-stable//25.08
flatpak-builder --user --install --force-clean build-dir \
    io.github.superuser_miguel.Foresight.yml
flatpak run io.github.superuser_miguel.Foresight
```

### Host (fast dev iteration)

Needs `gtk4-devel`, `libadwaita-devel`, `blueprint-compiler`, Meson, and `rsync`
on `PATH` (the engine tests drive real rsync).

```sh
cargo test                                   # parser + argv + engine tests
cargo clippy --all-targets -- -D warnings
meson setup builddir -Dprofile=debug && meson compile -C builddir
meson test -C builddir                       # includes AppStream metainfo validation
```

### Release bundle

The published bundle is built from a separate **release manifest**,
`io.github.superuser_miguel.Foresight.release.yml`: it takes its source from the
signed release tag rather than the working tree, and builds with **no network**
against the vendored crate graph in `cargo-sources.json`.

```sh
flatpak-builder --user --force-clean --repo=repo-release build-dir-release \
    io.github.superuser_miguel.Foresight.release.yml
flatpak build-bundle repo-release Foresight.flatpak \
    io.github.superuser_miguel.Foresight stable \
    --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo
```

Regenerate `cargo-sources.json` whenever `Cargo.lock` changes
(`python3 flatpak-cargo-generator.py Cargo.lock -o cargo-sources.json`; needs a
venv with `tomlkit` + `aiohttp`).

> Released bundles ship on the **`stable`** branch, pinned by `branch: stable`
> in the release manifest. This is load-bearing: flatpak-builder's default is
> `master`, and a bundle on a different branch than the one users already have
> installs *beside* it instead of upgrading it. The dev manifest stays on
> `master` on purpose, so a local `flatpak-builder --install` can't clobber a
> real install.

## Roadmap

### Shipped

- [x] **Dry-run preview** — grouped change list from a real `-n -i` run.
- [x] **Multi-source transfers** — files and folders from different locations,
      with per-item remove and drag-and-drop.
- [x] **Predictable placement** — everything you add lands *inside* the
      destination, with an opt-in **Sync folder contents** for the other form.
- [x] **Live progress, cancel, and a structured streaming log** of the run.
- [x] **`--delete` with a confirmation** listing the deletions from the dry run.
- [x] **Advanced options** — move (`--remove-source-files`), unit-aware bandwidth
      limit (`--bwlimit`), excludes, and a free-form extra-arguments field.
- [x] **Saved presets** for Advanced-option sets.
- [x] **Help / capability disclosure** — a registry-driven, test-enforced in-app
      inventory of the flags Foresight exposes, cross-referenced to the bundled
      `rsync --help`.
- [x] **AppStream metainfo, screenshots, and a landing page.**
- [x] **First `.flatpak` release** — an offline, reproducible bundle built from a
      GPG-signed tag, published on GitHub Releases.
- [x] **Self-hosted repo** — a GPG-signed OSTree remote served from GitHub
      Pages, plus a `.flatpakref`, so `flatpak update` pulls new versions
      instead of re-downloading a bundle.
- [x] **Excludes editor** — exclude rules as a managed list with per-rule
      removal, replacing the space-separated field that could not express a
      pattern containing a space.

### Next
- [ ] **Include rules** — `--include` alongside excludes, so a rule can carve an
      exception out of a broader exclude.
- [ ] **Remote sync over SSH** — rsync to/from a `user@host:/path` endpoint,
      key-based auth first. Three parts, not one:
      1. **Agent access.** `--share=network` and the runtime's `ssh` are already
         there, but `SSH_AUTH_SOCK` does not resolve inside the sandbox and
         `~/.ssh` is (correctly) invisible, so no key is reachable today. Adding
         **`--socket=ssh-auth`** fixes it — verified: the agent's keys become
         usable while no private key material ever enters the sandbox. It is a
         `finish-args` change, so PLAN.md §5 must move with it.
      2. **Host-key trust.** `known_hosts` is not readable either, so first
         contact has nothing to verify against. Preference is an app-managed
         `known_hosts` in the config dir with the fingerprint shown for
         confirmation, rather than granting access to `~/.ssh`.
      3. **The endpoint UI** — parsing and validating `user@host:/path`.

### Distant future / speculative

- **Saved remote credentials via the system keyring.** For password-based SSH
  or an rsync-daemon password, optionally remember it in the login keyring
  (Secret Service / `org.freedesktop.secrets`) instead of a config file — a
  deliberate, opt-in sandbox permission we'd only add for this convenience.
  Key-based auth would never need it.

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
