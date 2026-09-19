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

- **Remote sync over SSH.** The only gap the README admits to, in three real
      parts. The old "it's only the UI" sizing was wrong — it came from a test
      that passed an explicit `-i <key>` under a `--filesystem` grant — and is
      not to be revived.
      - [x] **Agent access.** `--socket=ssh-auth`; §5 moved with it.
      - [x] **Host-key trust — engine done** (`ssh.rs`). An app-managed
            `known_hosts` in the config dir beside `profiles.ini`, never
            `~/.ssh`. It cannot ride on ssh's default file: **the sandbox home
            is ephemeral**, so `accept-new` writes `~/.ssh/known_hosts` inside
            the sandbox and it is gone on the next launch — verified by writing
            to both files and coming back in a fresh sandbox, where only the
            config-dir one survived. Policy is `StrictHostKeyChecking=yes` +
            `GlobalKnownHostsFile=/dev/null` + `BatchMode=yes`, never
            `accept-new` (that is TOFU without asking) and never a prompt (there
            is no terminal, so a prompt is a hang). The confirmation *dialog*
            lands with the endpoint UI below — until a host can be entered there
            is nothing to confirm.
      - [x] **The endpoint UI — done.** `endpoint.rs` holds the four fields the
            UI collects (user, host, port, path) and renders the operand;
            `remote_dialog.rs` is the form plus the first-contact fingerprint
            confirmation, which is the piece part 2 could not build on its own.
            The port is kept out of the operand deliberately — it is not rsync
            syntax; it rides in `-e ssh -p N` and keys the `known_hosts` entry.
            IPv6 link-local survives with brackets and `%scope` intact, and a
            remote operand is passed verbatim rather than growing the trailing
            slash used for a local mirror dir.

            `Job::remote` is one `Option<Remote>` rather than two fields so that
            "remote at both ends" — which rsync refuses — is **unrepresentable**
            rather than merely checked. The UI enforces the matching rule for
            sources: local paths or one remote operand, never a mix.
- [x] **Preset format — FROZEN at 1.0.** It churned three times on the way here
      (space-joined `excludes` → `exclude_N` → `filter_N` + `filter_N_kind`).
      As of 1.0 the on-disk shape of `profiles.ini` is a compatibility promise,
      not an implementation detail:
      - `filter_N` + `filter_N_kind` is the encoding. It does not change again.
      - **All three readers stay forever.** Deleting the two legacy paths in
        `profiles.rs` would silently discard rules a user saved years ago, which
        is worse than carrying twenty lines of code.
      - A new *field* may be added (absent = a documented default, as
        `sync_contents` already does). Changing or removing an existing key is a
        breaking change and needs a major version, a migration, and a note here.
      - The tests that pin this — round-trip, `;`/`=`/backslash metacharacters,
        150-rule ordering, both legacy encodings — are the promise's teeth. They
        do not get deleted either.
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
| `--share=network` | present | ssh remotes / rsyncd; the runtime's ssh client and a real transfer verified working from inside the sandbox |
| `--socket=ssh-auth` | present | Remote sync over SSH (M5). Forwards the **host agent's socket** only — no private key material enters the sandbox, the agent signs on request and never hands over a key. This is why it is the correct grant and `--filesystem=~/.ssh` is not. Without it there is no key here at all: the sandbox inherits a *dead* `SSH_AUTH_SOCK` (`/run/user/1000/gcr/ssh`, unmounted), so ssh reports "Error connecting to agent" while looking configured. With it the socket appears at `/run/flatpak/ssh-auth` and `ssh-add -l` lists the host agent's keys. Inert until the endpoint UI exists — nothing in the app spawns ssh yet |
| `--filesystem=…` (any) | **absent** | Portals provide file access; revisit only for saved profiles, narrowest scope possible, with written justification |
| `--talk-name=org.freedesktop.secrets` | absent | Add only if a remote *password* or key passphrase is ever stored in the keyring. Key-based auth through the agent needs nothing |
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
8. **`-e` is the one argv element rsync re-tokenises itself.** Everywhere else
   the app's argv discipline means a value with a space is safe; here it is
   not, because rsync splits the remote-shell string a second time. Measured
   against the bundled 3.4.4 by logging what the `-e` command actually received:
   - Single **or** double quotes group a token, so
     `-o "UserKnownHostsFile=/a b/kh"` arrives as **one** argument.
   - The *other* quote character is literal inside a quoted token.
   - **Backslash is not an escape.** `/a\ b` tokenises to `/a\`, and
     `"/a\"b"` to `/a\` — a backslash never protects anything.

   So a value containing both quote characters is genuinely inexpressible;
   `ssh::quote_for_rsh` returns `None` for it rather than emitting a command
   that breaks in a way no error message would explain.
9. **`--info=progress2` and the `%i %n%L` out-format stream identically over
   ssh**, so `rsync-events` parses a remote job with no changes. Verified
   through a real ssh transfer from inside the sandbox.
10. **Remote args are already protected — do not send `-s`.** Since 3.2.4 rsync
    backslash-escapes shell-active characters (spaces included) in the args it
    hands the remote shell; `--old-args` exists to turn that *off*. Confirmed by
    pushing to a remote `my backups/` with and without `-s` and getting the same
    correct result, with no word-split directories on the far side.

    `--secluded-args` (`-s`) is **not** the stronger version it looks like. It
    moves wildcard expansion from the remote shell to the remote *rsync*, which
    still expands them — so it does not buy literal `*` either. What it does buy
    is a real cost: restricted shells such as `rrsync`, which is exactly how a
    careful person locks down a backup target, **refuse** it. Sending it by
    default would break the best-configured destinations to fix something rsync
    already fixed. (This entry replaced the opposite conclusion; the flag's
    "optional secluded-args" line in `--version` says it is compiled in, not
    that it is needed.)
11. **A leading `/` in a filter pattern is the top of the *transfer*, not of
    the disk** — and where that is depends on the trailing slash. With source
    `Photos`, `/Photos/private` matches and `/private` is dead; with `Photos/`
    ("Copy contents") it is exactly the other way round. A pasted full path
    (`/home/me/Photos/private`) therefore matches nothing, and rsync says
    nothing: no warning, exit 0. Found in the 1.0 soak, where it was combined
    with `--remove-source-files` and the "excluded" folder left the source. No
    file is ever destroyed — rsync unlinks a source file only after that file
    arrived — but it is not where the user believes it is. Measured over seven
    spellings; the ones that work from either mode are the unanchored ones
    (`private`, `private/`, `private/**`). `FilterRule::dead_anchor` encodes
    this and a test runs it against the real binary.
12. **`--debug=FILTER` is how to learn which rule matched what.** One line per
    match, on stdout, pattern echoed verbatim (leading and trailing `/`
    intact): `[sender] hiding directory Photos/private because of pattern
    private`. `hiding`/`showing` come from the sender, `protecting`/`risking`
    from the generator under `--delete`. A rule that matched nothing leaves no
    line — that absence is the signal. Two limits, both measured on 3.5.0:
    rsync does **not** forward `--debug` to the far end (the server argv has no
    trace of it), so a **pull** reports no `hiding` lines at all and must not be
    judged by them; and that same fact is why this is safe for `rrsync`, which
    as of 3.5.0 refuses a peer-sent `--debug` outright.

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
