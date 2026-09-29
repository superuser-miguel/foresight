#!/usr/bin/env bash
# capture_fixtures.sh — regenerate tests/fixtures/ from a real rsync binary.
#
# The parser is tested against captured transcripts, not guessed formats.
# Run this whenever the bundled rsync version is bumped, then diff the output:
# a format drift shows up as a test failure, not a runtime surprise.
#
# Usage:  ./scripts/capture_fixtures.sh [path-to-rsync]   (default: rsync in PATH)
set -euo pipefail

R="${1:-rsync}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
FIX="$HERE/tests/fixtures"
LAB="$(mktemp -d)"
# chmod first: step 7 makes a directory unreadable, and if the script dies
# while it is, rm could not otherwise clear it away.
trap 'chmod -R u+rwX "$LAB" 2>/dev/null; rm -rf "$LAB"' EXIT

echo "capturing fixtures with: $($R --version | head -1)"
mkdir -p "$FIX" "$LAB/src/docs" "$LAB/src/media" "$LAB/dst"

# --- source tree -----------------------------------------------------------
echo "hello world" > "$LAB/src/readme.txt"
head -c 3500000 /dev/urandom > "$LAB/src/media/video_part1.bin"
head -c 2200000 /dev/urandom > "$LAB/src/media/video_part2.bin"
head -c 900000  /dev/urandom > "$LAB/src/docs/thesis.pdf"
ln -s docs/thesis.pdf "$LAB/src/latest-thesis"
chmod 750 "$LAB/src/docs"

# seed a destination, then mutate the source to create a delta
$R -a "$LAB/src/" "$LAB/dst_seeded/"
echo "v2 content longer than before" >> "$LAB/src/readme.txt"
head -c 400000 /dev/urandom > "$LAB/src/docs/new_chapter.odt"
rm "$LAB/src/media/video_part2.bin"
chmod 700 "$LAB/src/docs"

# --- fixtures ---------------------------------------------------------------
# 1. dry-run itemized delta (the preview feature)
$R -a -n -i --delete "$LAB/src/" "$LAB/dst_seeded/" \
    > "$FIX/dry_run_itemize.txt" 2>&1; echo "exit=$?" >> "$FIX/dry_run_itemize.txt"

# 2. fresh dry run — everything is a creation, includes symlink %L arrow
$R -a -n -i "$LAB/src/" "$LAB/dst/" > "$FIX/dry_run_fresh.txt" 2>&1

# 3. real run: progress2 + per-file out-format, RAW bytes (\r intact!)
$R -a --info=progress2 --out-format='%i %n' "$LAB/src/" "$LAB/dst/" \
    > "$FIX/progress2_run.raw" 2>&1
echo "exit=$?" > "$FIX/progress2_run.exit"

# 4. --stats summary block
head -c 600000 /dev/urandom > "$LAB/src/media/bonus.bin"
$R -a --stats "$LAB/src/" "$LAB/dst/" > "$FIX/stats_run.txt" 2>&1

# 5. error transcript + exit code (missing source)
#    `|| rc=$?`, not `|| true`: the latter records true's exit status, 0.
rc=0
$R -a "$LAB/nope-does-not-exist/" "$LAB/dst/" \
    > "$FIX/error_missing_source.txt" 2>&1 || rc=$?
echo "exit=$rc" >> "$FIX/error_missing_source.txt"

# 6. filter debug — which rule matched which path (the dead-rule check).
#    Covers an exclude that hides a directory, one that hides a file, an
#    include, a pattern with a space, and `/nomatch`: an anchored rule that
#    matches nothing and must therefore leave NO line behind.
mkdir -p "$LAB/f/Photos/keep" "$LAB/f/Photos/private" "$LAB/f/Photos/my cache" "$LAB/fdst"
echo a > "$LAB/f/Photos/keep/a.jpg"; echo r > "$LAB/f/Photos/keep/r.raw"
echo b > "$LAB/f/Photos/private/b.jpg"; echo z > "$LAB/f/Photos/my cache/z.tmp"
$R -a --exclude=private --exclude=/nomatch '--exclude=my cache/' \
    '--include=*.jpg' '--exclude=*.raw' --debug=FILTER -n -i \
    "$LAB/f/Photos" "$LAB/fdst/" > "$FIX/dry_run_filter_debug.txt" 2>&1

# 7. a source that cannot be read, under --delete — the lines rsync does not
#    prefix with `rsync:`. One unreadable directory sets rsync's I/O error
#    flag, and from there on it deletes nothing MORE and says so once: "IO error
#    encountered -- skipping file deletion". `top-stale.txt` is listed for
#    deletion because its directory was dealt with before the error was met;
#    `ok/stale.txt`, met after it, is not — that absence is the point.
#    Needs a non-root user (root reads a mode-000 directory). The opendir line
#    carries $LAB, so this fixture differs in that path on every capture.
mkdir -p "$LAB/io/src/locked" "$LAB/io/src/ok" "$LAB/io/dst/src/ok"
echo a > "$LAB/io/src/ok/a.txt"
echo s > "$LAB/io/dst/src/ok/stale.txt"
echo t > "$LAB/io/dst/src/top-stale.txt"
chmod 000 "$LAB/io/src/locked"
rc=0
(cd "$LAB/io" && $R -a -n -i --delete src dst/) \
    > "$FIX/dry_run_io_error.txt" 2>&1 || rc=$?
chmod 755 "$LAB/io/src/locked"
echo "exit=$rc" >> "$FIX/dry_run_io_error.txt"

# 8. names that are not ASCII, RAW bytes. In a UTF-8 locale rsync writes valid
#    UTF-8 as it is and escapes what is not (and control characters) as \#ooo;
#    in the C locale it would escape every byte above 0x7f, so the locale is
#    pinned here. 2-, 3- and 4-byte characters, a symlink whose target is one,
#    a Latin-1 name, and a name holding a carriage return.
mkdir -p "$LAB/u/año" "$LAB/udst"
echo n > "$LAB/u/año/ñandú.txt"
echo j > "$LAB/u/日本語.txt"
echo e > "$LAB/u/emoji 😀.bin"
echo l > "$LAB/u/"$'latin1-\xe9-\xff.txt'
echo c > "$LAB/u/"$'cr\rname.txt'
ln -s 日本語.txt "$LAB/u/lien-é"
LC_ALL=C.UTF-8 $R -a --info=progress2 --out-format='%i %n%L' "$LAB/u/" "$LAB/udst/" \
    > "$FIX/progress2_non_ascii.raw" 2>&1

echo "fixtures written to $FIX — now: git diff tests/fixtures, then cargo test --all"
