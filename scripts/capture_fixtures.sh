#!/usr/bin/env bash
# capture_fixtures.sh — regenerate tests/fixtures/ from a real rsync binary.
#
# The parser is tested against captured transcripts, not guessed formats.
# Run this whenever the bundled rsync version is bumped, then run pytest:
# a format drift shows up as a test failure, not a runtime surprise.
#
# Usage:  ./scripts/capture_fixtures.sh [path-to-rsync]   (default: rsync in PATH)
set -euo pipefail

R="${1:-rsync}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
FIX="$HERE/tests/fixtures"
LAB="$(mktemp -d)"
trap 'rm -rf "$LAB"' EXIT

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
$R -a "$LAB/nope-does-not-exist/" "$LAB/dst/" \
    > "$FIX/error_missing_source.txt" 2>&1 || true
echo "exit=$?" >> "$FIX/error_missing_source.txt"

echo "fixtures written to $FIX — now run: python3 -m pytest tests/"
