#!/usr/bin/env bash
# Build → sign (Developer ID + Hardened Runtime) → DMG → notarize → staple.
# Adapted from Templates/AppKitTemplate/Scripts/release.sh for Vibe Factory.
# Plain DMG (no Finder layout, no Sparkle): both come with the first tagged
# release of the app. Not run in CI.
#
# Usage:   ./Scripts/release.sh <version>      (from apps/macos/VibeFactory)
# Example: ./Scripts/release.sh 0.1.0
#
# Reuses Vincent's shared Apple credentials (same account for all Mac apps):
#   - Developer ID Application: Vincent LAURIAT (KFLACS69T9)
#   - notary keychain profile "AppliMacVincentGithub" (apple-id vincent@lauriat.fr)
#
# Overridable via env: APP_NAME, SCHEME, PROJECT, SIGNING_IDENTITY, NOTARY_PROFILE
set -euo pipefail

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
  echo "usage: $0 <version>   (e.g. $0 0.1.0)" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_NAME="${APP_NAME:-Vibe Factory}"     # PRODUCT_NAME / .app bundle name
SCHEME="${SCHEME:-VibeFactory}"
PROJECT="${PROJECT:-VibeFactory.xcodeproj}"

DMG_SLUG="$(echo "$APP_NAME" | tr -d ' ')"
DMG_VOLNAME="$APP_NAME $VERSION"
RELEASE_DIR="$ROOT/release"
mkdir -p "$RELEASE_DIR"
DMG="$RELEASE_DIR/$DMG_SLUG-$VERSION.dmg"
SIGNING_IDENTITY="${SIGNING_IDENTITY:-Developer ID Application: Vincent LAURIAT (KFLACS69T9)}"
NOTARY_PROFILE="${NOTARY_PROFILE:-AppliMacVincentGithub}"
BUILD_NUMBER="$(git rev-list --count HEAD 2>/dev/null || echo 1)"

# 1. MARKETING_VERSION in project.yml must match the requested version.
DECLARED="$(awk -F'"' '/MARKETING_VERSION:/ {print $2; exit}' project.yml)"
if [ "$DECLARED" != "$VERSION" ]; then
  echo "✗ project.yml declares MARKETING_VERSION $DECLARED, not $VERSION" >&2
  exit 1
fi

echo "▶︎ Releasing $APP_NAME $VERSION (build $BUILD_NUMBER)"

# 2. Regenerate the Xcode project from project.yml
echo "▶︎ xcodegen generate"
xcodegen generate >/dev/null

# 3. Build Release unsigned (provenance xattrs break in-place codesign); sign below.
BUILD_LOG="$RELEASE_DIR/build-$VERSION.log"
echo "▶︎ xcodebuild Release (log: $BUILD_LOG)"
if ! xcodebuild -project "$PROJECT" -scheme "$SCHEME" -configuration Release \
  -destination 'platform=macOS' -derivedDataPath build \
  MARKETING_VERSION="$VERSION" CURRENT_PROJECT_VERSION="$BUILD_NUMBER" \
  CODE_SIGNING_ALLOWED=NO \
  build >"$BUILD_LOG" 2>&1; then
  tail -30 "$BUILD_LOG" >&2
  echo "✗ xcodebuild failed, full log: $BUILD_LOG" >&2
  exit 1
fi
APP="$ROOT/build/Build/Products/Release/$APP_NAME.app"
[ -d "$APP" ] || { echo "✗ App not found: $APP" >&2; exit 1; }

# 4. Stage to a clean dir, stripping extended attributes
STAGING_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGING_DIR"' EXIT
STAGING="$STAGING_DIR/$APP_NAME.app"
ditto --norsrc --noextattr --noacl "$APP" "$STAGING"

# 5. Codesign with Hardened Runtime + secure timestamp (retry: Apple TS is flaky).
#    No nested frameworks: VibeAPI is a static Swift package linked into the app.
codesign_ts() {
  local target="$1" i
  for i in 1 2 3 4 5; do
    if codesign --force --options runtime --timestamp --sign "$SIGNING_IDENTITY" \
         --entitlements "$ROOT/VibeFactory/VibeFactory.entitlements" "$target"; then
      return 0
    fi
    echo "  …codesign retry $i/5 (timestamp server) in 5s" >&2
    sleep 5
  done
  echo "✗ codesign failed for $target" >&2
  return 1
}
echo "▶︎ codesign (Developer ID, Hardened Runtime)"
codesign_ts "$STAGING"
codesign --verify --strict --deep --verbose=1 "$STAGING"

# 6. Plain DMG with an /Applications alias
echo "▶︎ build DMG"
DMG_LAYOUT="$STAGING_DIR/dmg-layout"
mkdir -p "$DMG_LAYOUT"
ditto --norsrc --noextattr --noacl "$STAGING" "$DMG_LAYOUT/$APP_NAME.app"
ln -s /Applications "$DMG_LAYOUT/Applications"
hdiutil create -volname "$DMG_VOLNAME" -srcfolder "$DMG_LAYOUT" \
  -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$DMG" >/dev/null

# 7. Notarize + staple
if ! xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1; then
  cat >&2 <<MSG
✗ Notary profile "$NOTARY_PROFILE" not found.
  Create it once (interactive):
    xcrun notarytool store-credentials "$NOTARY_PROFILE" \\
      --apple-id "vincent@lauriat.fr" --team-id "KFLACS69T9"
  The DMG was built and signed at: $DMG (NOT yet notarized).
MSG
  exit 1
fi
echo "▶︎ notarize (this takes a few minutes)"
xcrun notarytool submit "$DMG" --keychain-profile "$NOTARY_PROFILE" --wait
echo "▶︎ staple"
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"

# 8. Keep the signed app next to the DMG (the staging dir goes with the EXIT
#    trap), staple it (the DMG's ticket covers it) and verify it independently.
RELEASED_APP="$RELEASE_DIR/$APP_NAME.app"
rm -rf "$RELEASED_APP"
ditto "$STAGING" "$RELEASED_APP"
xcrun stapler staple "$RELEASED_APP"
echo "▶︎ independent verification"
spctl -a -t exec -vv "$RELEASED_APP"          # expected: accepted, source=Notarized Developer ID
codesign --verify --deep --strict --verbose=2 "$RELEASED_APP"

SIZE="$(du -h "$DMG" | cut -f1 | tr -d ' ')"
echo
echo "✅ Built, signed, notarized & stapled: $(basename "$DMG") ($SIZE)"
echo "   Verified app: $RELEASED_APP"
