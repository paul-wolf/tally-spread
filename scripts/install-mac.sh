#!/usr/bin/env bash
# Build Tally and install it on this Mac.
#
#   scripts/install-mac.sh            build, install to /Applications, open it
#   scripts/install-mac.sh --cli      also install the `tally` command (cargo install)
#   scripts/install-mac.sh --test     run the engine and app tests first
#   scripts/install-mac.sh --no-open  don't start Tally afterwards
#   scripts/install-mac.sh --dest ~/Applications
#
# Your data is not touched: it lives in ~/Tally (or $TALLY_HOME), separate from the app.
# The first build compiles DuckDB and takes several minutes; later builds are quick.

set -euo pipefail

APP_NAME="Tally"
BUNDLE_ID="io.yew.tally"
DEST="/Applications"
OPEN=1
CLI=0
TEST=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --cli) CLI=1 ;;
    --test) TEST=1 ;;
    --no-open) OPEN=0 ;;
    --dest) DEST="${2:?--dest needs a folder}"; shift ;;
    -h|--help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "Unknown option: $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done

say() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
die() { printf '\033[31mError:\033[0m %s\n' "$*" >&2; exit 1; }

[[ "$(uname -s)" == "Darwin" ]] || die "this script is for macOS. On Linux use: npm run tauri build"

# Run from the repository root, wherever the script is called from.
cd "$(dirname "$0")/.."
[[ -f src-tauri/tauri.conf.json ]] || die "run this from the tally repository (scripts/install-mac.sh)"

command -v cargo >/dev/null || die "cargo not found. Install Rust from https://rustup.rs"
command -v npm >/dev/null || die "npm not found. Install Node 20 or later"

# JavaScript packages: install when missing or when package-lock.json changed.
if [[ ! -d node_modules || package-lock.json -nt node_modules/.package-lock.json ]]; then
  say "Installing JavaScript packages"
  npm ci --no-audit --no-fund
fi

if [[ $TEST -eq 1 ]]; then
  say "Running tests"
  npm run typecheck
  cargo test -p tally-core -p tally-app
fi

say "Building $APP_NAME (release)"
# Only the .app: a .dmg is slower and not needed to install locally.
npm run tauri build -- --bundles app

BUILT="target/release/bundle/macos/$APP_NAME.app"
[[ -d "$BUILT" ]] || die "build finished but $BUILT is missing"

# Local builds aren't signed with a developer certificate. An ad-hoc signature keeps
# macOS from calling the app damaged; nothing is sent anywhere.
codesign --force --deep --sign - "$BUILT" >/dev/null 2>&1 || echo "(ad-hoc signing skipped)"

# Quit a running Tally so its files can be replaced; wait up to 10 seconds.
if pgrep -f "$APP_NAME.app/Contents/MacOS" >/dev/null; then
  say "Quitting the running $APP_NAME"
  osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
  for _ in $(seq 1 20); do
    pgrep -f "$APP_NAME.app/Contents/MacOS" >/dev/null || break
    sleep 0.5
  done
  pgrep -f "$APP_NAME.app/Contents/MacOS" >/dev/null && die "$APP_NAME is still running. Quit it and run this again."
fi

# Install: copy next to the old app first, then swap, so a failed copy leaves the old one.
mkdir -p "$DEST"
[[ -w "$DEST" ]] || die "can't write to $DEST. Try: $0 --dest ~/Applications"
TARGET="$DEST/$APP_NAME.app"
say "Installing to $TARGET"
rm -rf "$TARGET.new"
ditto "$BUILT" "$TARGET.new"
rm -rf "$TARGET"
mv "$TARGET.new" "$TARGET"
xattr -dr com.apple.quarantine "$TARGET" 2>/dev/null || true

if [[ $CLI -eq 1 ]]; then
  say "Installing the tally command"
  cargo install --path crates/tally-cli --locked
fi

VERSION=$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' "$TARGET/Contents/Info.plist" 2>/dev/null || echo "?")
COMMIT=$(git rev-parse --short HEAD 2>/dev/null || echo "?")
DIRTY=$([[ -n "$(git status --porcelain 2>/dev/null)" ]] && echo " + uncommitted changes" || echo "")
say "Installed $APP_NAME $VERSION (commit $COMMIT$DIRTY)"

if [[ $OPEN -eq 1 ]]; then
  open "$TARGET"
fi
