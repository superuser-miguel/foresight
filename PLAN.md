# PLAN.md — GTK4/libadwaita rsync frontend

> This document is the working spec for the project. It is written to be fed
> to Claude Code: decisions are stated as constraints, tasks have acceptance
> criteria, and the gotchas section encodes hard-won facts about rsync's real
> behavior. When this plan and an ad-hoc idea conflict, this plan wins until
> the plan itself is amended.

## 1. Charter

A thin, modern GTK4/libadwaita frontend for rsync, distributed exclusively as
a Flatpak. The app never reimplements rsync: it composes argv, spawns the
**bundled** rsync via `Gio.Subprocess`, parses its output into events, and
renders state. The signature feature is the **dry-run preview**: every sync
can be inspected as a grouped change list (created / updated / deleted /
attribute-only) before a single byte moves.

**Non-goals (v1):** system backups requiring root, scheduling daemons,
rsyncd server hosting, cloud-storage backends, reimplementing the delta
algorithm, Qt/other-desktop theming.

## 2. Locked decisions

| Decision | Value | Rationale |
|---|---|---|
| Language | **Rust + gtk4-rs + libadwaita-rs** | Type-safe parser/state machine; one toolchain with the Papers contribution track |
| UI | GTK4 + libadwaita, **Blueprint** (`.blp`) files | Declarative, diff-friendly, the modern GNOME way; gtk-rs consumes them via composite templates |
| Build | Meson driving cargo (GNOME Builder Rust-template pattern) + blueprint-compiler | Standard GNOME app pipeline |
| Distribution | Flatpak only; dev happens inside the Flatpak | Environment = ship environment |
| Engine | rsync pinned in the manifest (currently `v3.4.4`, commit `f26f747b…`) | The version pin **is** the output-format contract |
| File access | Portals first (see §5); no blanket `--filesystem` holes | Sandbox integrity + user trust |
| License | GPL-3.0-or-later | Matches rsync; project convention |
| Parser | `crates/rsync-events` — lib crate, deps: `regex` + `once_cell` only, **no GTK** | Testable anywhere; the app crate depends on it, never the reverse |

App id is `io.github.superuser_miguel.Foresight` (named 2026-07-12; the
`io.github.CHANGEME.RsyncGUI` working id was renamed everywhere in one commit).

## 3. Repository layout (target)

```
.
├── PLAN.md
├── io.github.superuser_miguel.Foresight.yml     # Flatpak manifest (in repo root)
├── Cargo.toml                          # workspace root
├── meson.build                         # drives blueprint + cargo + install
├── data/
│   ├── io.github.superuser_miguel.Foresight.desktop.in
│   ├── io.github.superuser_miguel.Foresight.metainfo.xml.in   # appstream (Phase 4)
│   └── icons/
├── crates/
│   ├── rsync-events/           # ← pure parser crate, ALREADY WRITTEN
│   │   ├── src/lib.rs          #    7/7 tests passing (cargo test)
│   │   └── tests/fixtures_test.rs
│   └── foresight/               # the GTK app crate (Milestone 1)
│       └── src/
│           ├── main.rs         # adw::Application entry point
│           ├── window.rs       # #[template] composite for window.blp
│           └── job.rs          # SyncJob: argv builder + gio::Subprocess
├── src/ui/
│   ├── window.blp              # ← starter skeleton ALREADY WRITTEN
│   └── preview_row.blp
├── reference/
│   └── rsync_events.py         # executable spec of the parser (kept in sync)
├── scripts/
│   └── capture_fixtures.sh     # ← ALREADY WRITTEN
└── tests/
    └── fixtures/               # ← captured from real rsync 3.4.4 (shared)
```

## 4. Milestones

### Milestone 0 — Parser core  ✅ (shipped with this kit)

`crates/rsync-events` parses, from **captured real rsync 3.4.4 output**:
itemized changes (`%i %n%L`, including `*deleting` and symlink targets),
`--info=progress2` updates (including `\r` framing and `ir-chk` scanning
phase), `--stats` blocks, error lines, and exit codes via `classify_exit()`.
All 7 integration tests pass (`cargo test`). `reference/rsync_events.py` is
a line-for-line Python spec of the same semantics — update both or neither.

Remaining tasks:
- [x] Add `job.rs::build_argv(&Job) -> Vec<OsString>` in the app crate that
      produces exactly the two contract command lines in the crate docs, plus
      the `--delete` toggle. Unit-test it: no user string is ever
      shell-interpreted (always spawn with an argv vector, never a shell
      string; paths go through `OsString`, never lossy UTF-8).
      → `crates/foresight/src/job.rs`, 8 unit tests.
- [x] Wire `cargo test` + `cargo clippy -- -D warnings` + `cargo fmt --check`
      into CI (GitHub Actions or GitLab CI) on every push.
      → `.github/workflows/ci.yml`.

### Milestone 1 — GTK4 scaffold + Blueprint

Goal: the app launches inside the Flatpak with the three-page ViewStack from
`src/ui/window.blp` (Configure → Preview → Transfer) and non-functional
controls.

- [x] Scaffold `crates/foresight` from the GNOME Builder Rust template
      pattern: `meson.build` compiles `.blp` → `.ui` via blueprint-compiler,
      bundles them into a GResource, invokes cargo, installs the binary,
      desktop file, and icons.
- [x] `window.rs`: `#[derive(CompositeTemplate)]` +
      `#[template(resource = "…/window.ui")]` bound to `$ForesightWindow`;
      `#[template_child]` for every named widget in the Blueprint.
- [x] `flatpak-builder --user --install --force-clean build-dir <manifest>`
      succeeds; `flatpak run` shows the window under Wayland. (Dev builds may
      use the manifest's `--share=network` build-arg for crates.io; a
      reproducible release build requires vendored `cargo-sources.json` instead
      — see the manifest comments.)
- [x] Sanity check the bundled engine:
      `flatpak run --command=rsync <app-id> --version` prints 3.4.4. ✓ 3.4.4

Acceptance: a clean checkout builds and launches with only `flatpak-builder`
installed on the host. **All GTK layout happens in `.blp` files** — Blueprint
is the source of truth; never hand-edit generated `.ui` XML, never build
widget trees in Rust when a Blueprint template can express them.

### Milestone 2 — Portal-based file selection

Goal: Source and Destination rows open the portal folder picker and remember
the choice for the session.

- [x] Use `gtk::FileDialog::select_folder()` (async — pair with
      `glib::spawn_future_local`). Inside a Flatpak, GTK routes this through
      the FileChooser portal automatically — there is no separate "portal
      API" to call. Do not use deprecated `FileChooserDialog`.
- [x] Display the selection in the row subtitle. Expect `gio::File` paths
      under `/run/user/$UID/doc/…` for locations outside the sandbox —
      cosmetically map them for display (`file.basename()` + tooltip with
      full path) but always pass the **real returned path** to rsync argv.
      → `describe_path()`; real path stored in `imp.source`/`imp.dest`.
- [x] Verify the bundled rsync can read/write the portal-granted paths with a
      real small sync (this validates the whole sandbox model — do it early).
      → validated via `flatpak document-export` doc paths: dry-run + real
        sync succeed with **no `--filesystem`**, output matches the parser.
- [x] Drag-and-drop of a folder onto either row sets it (GTK4
      `gtk::DropTarget` with the `gio::File` GType).

Known limitation to design around, not against: **portal folder grants do not
persist across app restarts.** Session-scoped selection is fine for v0.1.
Saved profiles (Phase 2 of the roadmap) will need one of: re-prompting on
first use per session (acceptable), the Documents portal persist flags, or an
opt-in static `--filesystem` grant for specific trees (see §5). Decide when
profiles land, not before.

### Milestone 3 — Wire the engine

Goal: Preview button runs a real dry run; a Start button runs a real sync
with live progress.

- [x] `job.rs`: spawn bundled rsync with `gio::Subprocess`
      (`STDOUT_PIPE | STDERR_PIPE`), read stdout with async
      `read_bytes_future` in a loop on the main context
      (`glib::spawn_future_local`), decode lossily
      (`String::from_utf8_lossy`), feed chunks to
      `rsync_events::StreamParser::feed()`, and dispatch the returned
      `Event`s to the widgets. Never block the main loop; never collect all
      output before parsing.
      → `spawn_rsync()` (STDERR_MERGE so errors share the stream). Covered by
        an end-to-end test driving real rsync through a glib main loop.
- [x] Preview page: `gio::ListStore` of a small `glib::Object` wrapper
      around `ItemizedChange`, `gtk::ListView` with section headers per
      `ChangeKind`. Deletions render in destructive style.
      → `change_object.rs` + sectioned `SortListModel`/header factory.
- [x] Transfer page: overall `ProgressBar` from `Progress::bytes_done` /
      `percent`; current-file label from `ItemizedChange` events; log
      expander appends `Message` events verbatim.
      → fraction from `percent`, `Scanning…` during `ir-chk`.
- [x] Completion = **process exit**, mapped through `classify_exit()`:
      `Success` → toast; `Partial` (23/24) → warning banner with the
      collected error Messages; `Error` → `adw::AlertDialog` with details.
      Never treat exit 23/24 as a failure wall.
      → `show_completion()`; collected `rsync:` lines attached to the dialog.
- [x] `--delete` runs require the switch ON **and** an `adw::AlertDialog`
      listing the exact deletions taken from the dry run. No dry run yet →
      run one implicitly first.
      → Start-with-delete always runs a fresh dry run, then confirms.
- [x] Cancellation: `gio::Cancellable` + `send_signal(SIGTERM)`; surface
      exit 20 as "cancelled", not an error.
      → `Runner::cancel()` (SIGTERM + cancelled flag → `Severity::Cancelled`);
        covered by the `cancel_maps_to_cancelled` test.

### Milestone 4 — polish gate (defer until 0–3 are done)

Saved presets (done), filter-rule editor (done — excludes first, then include
rules), appstream metainfo + screenshots, and a
`.flatpak` bundle published on **GitHub Releases** (distribution is GitHub
Releases + a GitHub Pages landing page — **not** Flathub). Tracked in the
roadmap deck; not specced here yet.

- [x] **Filter rules are one ordered list**, not an includes set beside an
      excludes set. rsync evaluates filter rules in argv order and the first
      match decides a path, so the two arrangements are not equivalent: a
      two-list UI can only ever emit every include before every exclude, which
      silently forbids "exclude this subtree even from an include that would
      otherwise pull files out of it". The list carries a kind per rule and
      move-up/move-down, and `build_argv` emits it verbatim — the order on
      screen is the precedence rsync gets. `Job::filters` is the only encoding;
      preset storage keeps a `_kind` beside each pattern and reads both older
      exclude-only encodings so no saved rule set is lost on upgrade.

- [x] **Help / capability disclosure.** A Help surface (dialog opened from the
      primary menu) that states *explicitly and honestly* which rsync
      capabilities Foresight exposes **in this release** — no aspirational
      claims. Design constraints:
      - **Registry-driven, not hand-written.** Define one in-code table of
        exposed capabilities next to `job.rs::build_argv` — each entry is
        `{ rsync flag(s), the UI control it maps to, one-line description,
        man-rsync option name }`. The Help renders from this table, and
        `build_argv` should only ever emit flags that appear in it.
      - **Enforce truthfulness with a test:** every flag `build_argv` can emit
        must have a registry entry (and vice versa). This makes drift a test
        failure, so the Help can never lie about what the app does.
      - **Compare to the real engine, not a copy of the man page.** Don't
        reproduce `man rsync`. Reference the exact option names, and offer a
        "Full rsync options" action that runs the **bundled** rsync
        (`rsync --help`, pinned 3.4.4) and shows its output — accurate to the
        shipped version by construction.
      - **Name the boundary.** List common flags Foresight does *not* expose as
        dedicated controls yet (e.g. `--checksum`, `--compress`, `--backup`,
        remote/ssh, a filter-rules file) and point users to the Advanced →
        Extra arguments field for them.
      - Stamp the page with the app version + bundled rsync version, since the
        version pin **is** the behavior contract (§2).

### Milestone 5 — the 1.0 gate

1.0 is not "more features". It is the point where the advertised surface is
complete and the **formats stop moving**. Three things, and no more:

- [ ] **Remote sync over SSH.** The only gap the README admits to, in its three
      real parts: agent access (`--socket=ssh-auth`, which is a `finish-args`
      change and therefore moves §5 with it), an app-managed `known_hosts` with
      the fingerprint shown for confirmation, and the `user@host:/path` endpoint
      UI. The old "it's only the UI" sizing was wrong and is not to be revived.
- [ ] **Freeze the preset format.** It has now churned three times
      (space-joined → `exclude_N` → `filter_N` + `_kind`). 1.0 is the promise
      that it stops. The migration chain already in `profiles.rs` is what makes
      that promise cheap to keep — every future reader must keep reading all
      three encodings, and adding a fourth needs a written reason here.
- [ ] **Current screenshots**, and one pass confirming the Help still cannot
      lie (the registry test covers the flags; the prose is on us).

### Beyond 1.0 — what a 2.0 would be

A 2.0 is a change in what the app **is**, not a longer flag list. Today a job is
transient window state: presets deliberately store options but **not paths**,
because portal grants do not survive the session. Inverting exactly that is the
whole of 2.0.

> **The thesis: jobs become durable, schedulable, auditable objects.**
> That is the pivot from "a nicer way to type an rsync command" to a backup
> application.

In dependency order — each item is unbuildable before the one above it:

1. **Durable jobs.** A named job that stores its source *and* destination and
   survives a restart. This is the hard one and it is the gate on everything
   below: portal paths are handles, not locations, but document IDs persist and
   `org.freedesktop.portal.Documents.GetHostPaths` resolves them. Solve it here,
   once, honestly — §5 gets *amended*, not abandoned. If the only workable
   answer turns out to be a blanket `--filesystem`, the answer is no and 2.0
   stops at this line.
2. **Scheduling.** "Every night at 02:00", or "when this drive appears" —
   generated systemd **user** timers and `.path`/mount units, not a daemon of
   our own. The drive-appears trigger is the more useful of the two for the
   external-disk case this app is really used for.
3. **Run history.** What ran, when, what changed, what failed. Not optional once
   (2) exists: an unattended run nobody watched is worthless without a record,
   so history and scheduling are one feature wearing two names.
4. **Snapshot backups via `--link-dest`.** Hardlinked incremental trees — the
   thing that turns copies into *backups*. Pure rsync, so it stays inside the
   charter, and it composes with (2) and (3) rather than replacing them.
5. **Remote as a first-class endpoint.** Saved hosts and key/`known_hosts`
   management, once 1.0's SSH support has proven the plumbing.

**Still non-goals at 2.0**, and these are load-bearing:

- **Root / whole-system backups.** Would gut the sandbox story the entire
  project is built on. This is the one that will be asked for most.
- **Cloud-storage backends.** That is rclone's job, not rsync's.
- **rsyncd hosting** and reimplementing any part of the delta algorithm (§1).

**The honest cost:** items 1–3 give Foresight *background behavior*. Today the
app is inert when closed, and that inertness is a real part of why it can be
trusted with a `--delete`. A 2.0 that acts while nobody is watching has to buy
that trust back — with the history view, and by holding the dry-run-first
discipline for scheduled runs exactly as hard as for interactive ones.

## 5. Flatpak permissions policy

The manifest's `finish-args` are a contract. Claude Code must never add a
permission without also updating this table and the manifest comment block.

| finish-arg | Status | Justification |
|---|---|---|
| `--socket=wayland`, `--socket=fallback-x11`, `--share=ipc`, `--device=dri` | present | Standard GTK4 display stack |
| `--share=network` | present | ssh remotes / rsyncd (Phase 3); harmless before then |
| `--filesystem=…` (any) | **absent** | Portals provide file access; revisit only for saved profiles, narrowest scope possible, with written justification |
| `--talk-name=org.freedesktop.secrets` | absent | Add with Phase 3 if remote credentials are stored in the keyring |
| `--talk-name=org.freedesktop.Flatpak` / `flatpak-spawn` | **forbidden** | Defeats the sandbox; the engine is bundled precisely to avoid this |

## 6. Facts learned from real rsync 3.4.4 (do not re-litigate)

These were discovered by building the pinned rsync and capturing transcripts
(`tests/fixtures/`). The tests encode them.

1. **Progress lines end in `\r`, not `\n`.** Any line-based reader that
   splits only on newlines will buffer the entire progress stream until the
   end. `StreamParser` splits on both; keep it that way.
2. **The final progress line can read 99%, not 100%** (integer truncation),
   even with `to-chk=0`. Completion is signaled by process exit. The UI must
   never wait for 100%.
3. **`ir-chk` vs `to-chk`:** while incremental recursion is still scanning,
   the trailer reads `ir-chk` and totals grow. Show "scanning…" state until
   `to-chk` appears (`Progress.scanning`).
4. **Itemize flags are 11 chars** (`YXcstpoguax`); deletions arrive as
   `*deleting   path` with no file-type information. Symlinks carry
   ` -> target` via `%L`.
5. **Exit 23 is normal life**, e.g. one unreadable file in a big tree. It
   arrives with specific `rsync:` error lines on the stream — collect and
   attach them to the warning UI.
6. **GitHub tag tarballs lack the generated `./configure`** — a git-sourced
   Flatpak module needs the SDK's autotools (present in org.gnome.Sdk). The
   official samba.org release tarball ships configure pre-built.
7. **`-a` implies `-og`**, which cannot apply ownership without privileges.
   Userland syncs may emit attribute warnings — downgrade these to calm,
   grouped notices, not per-file error spam.

## 7. Claude Code operating notes

Build, run, test:

```bash
# full build + install (from repo root)
flatpak-builder --user --install --force-clean build-dir io.github.superuser_miguel.Foresight.yml
flatpak run io.github.superuser_miguel.Foresight

# parser tests + lint (host, no flatpak needed — the crate is pure)
cargo test
cargo clippy -- -D warnings && cargo fmt --check

# check a .blp compiles without building everything
blueprint-compiler compile src/ui/window.blp > /dev/null

# regenerate parser fixtures after bumping the rsync pin
./scripts/capture_fixtures.sh /path/to/new/rsync && cargo test

# regenerate vendored crates for offline/reproducible release builds after Cargo.lock changes
python3 flatpak-cargo-generator.py Cargo.lock -o cargo-sources.json
```

Conventions and guardrails:

1. `crates/rsync-events` depends on `regex` and `once_cell` only — zero
   gtk/glib/gio dependencies, ever. The app crate imports it; it never
   imports the app crate. `reference/rsync_events.py` is its executable
   spec: change both or neither.
2. Every rsync invocation goes through `job.rs::build_argv()`. No inline
   argv vectors scattered through UI code, no shell strings, ever. Paths are
   `OsString`/`PathBuf` end to end — never lossy-converted before reaching
   argv.
3. UI layout lives in Blueprint; Rust touches widgets only through
   `#[template_child]` bindings and signal handlers. Async UI work runs via
   `glib::spawn_future_local` — no threads touching widgets.
4. Any change to rsync flags used by the app requires: update the contract
   docs in `crates/rsync-events/src/lib.rs`, extend `capture_fixtures.sh` to
   cover the new output, re-capture, and add a test.
5. Permissions changes follow §5's table-first rule. The `--share=network`
   **build-arg** is a dev convenience only and must be replaced by
   `cargo-sources.json` vendoring before any public release build.
6. Commit style: conventional-ish, imperative, one concern per commit;
   the manifest, PLAN.md table, and code change together atomically.
