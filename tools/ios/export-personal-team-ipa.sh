#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  tools/ios/export-personal-team-ipa.sh \
    --archive PATH_TO_RUNNER_XCARCHIVE_OR_ZIP \
    --team-id TEAM_ID \
    --bundle-id UNIQUE_BUNDLE_ID \
    [--output DIR]

This script is intended for a real macOS machine with:
  - Xcode installed
  - your Apple ID already signed in to Xcode
  - a free Personal Team or paid Apple Developer team

It rewrites the archived bundle identifier, then asks Xcode to export
an automatically signed development IPA.
EOF
}

die() {
  printf '[ios-export][error] %s\n' "$1" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "Missing required command: $1"
}

ARCHIVE_INPUT=""
TEAM_ID=""
BUNDLE_ID=""
OUTPUT_DIR=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive)
      [[ $# -ge 2 ]] || die "--archive requires a value"
      ARCHIVE_INPUT="$2"
      shift 2
      ;;
    --team-id)
      [[ $# -ge 2 ]] || die "--team-id requires a value"
      TEAM_ID="$2"
      shift 2
      ;;
    --bundle-id)
      [[ $# -ge 2 ]] || die "--bundle-id requires a value"
      BUNDLE_ID="$2"
      shift 2
      ;;
    --output)
      [[ $# -ge 2 ]] || die "--output requires a value"
      OUTPUT_DIR="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "Unknown argument: $1"
      ;;
  esac
done

[[ -n "${ARCHIVE_INPUT}" ]] || {
  usage
  die "--archive is required"
}
[[ -n "${TEAM_ID}" ]] || die "--team-id is required"
[[ -n "${BUNDLE_ID}" ]] || die "--bundle-id is required"

uname -s | grep -qx 'Darwin' || die "This script must run on macOS"

require_cmd xcodebuild
require_cmd unzip
require_cmd mktemp
require_cmd /usr/libexec/PlistBuddy

archive_abs="$(cd -- "$(dirname -- "${ARCHIVE_INPUT}")" && pwd)/$(basename -- "${ARCHIVE_INPUT}")"
[[ -e "${archive_abs}" ]] || die "Archive input not found: ${ARCHIVE_INPUT}"

workdir="$(mktemp -d "${TMPDIR:-/tmp}/ios-export.XXXXXX")"
cleanup() {
  rm -rf "${workdir}"
}
trap cleanup EXIT

if [[ "${archive_abs}" == *.zip ]]; then
  unzip -q "${archive_abs}" -d "${workdir}/unzipped"
  archive_path="$(find "${workdir}/unzipped" -type d -name '*.xcarchive' -print -quit)"
  [[ -n "${archive_path}" ]] || die "No .xcarchive found inside ${archive_abs}"
else
  archive_path="${archive_abs}"
fi

[[ -d "${archive_path}" ]] || die "Archive directory not found: ${archive_path}"

staging_archive="${workdir}/Runner.xcarchive"
cp -R "${archive_path}" "${staging_archive}"

archive_info="${staging_archive}/Info.plist"
app_info="${staging_archive}/Products/Applications/Runner.app/Info.plist"
[[ -f "${archive_info}" ]] || die "Missing archive Info.plist"
[[ -f "${app_info}" ]] || die "Missing app Info.plist inside archive"

/usr/libexec/PlistBuddy -c "Set :ApplicationProperties:CFBundleIdentifier ${BUNDLE_ID}" "${archive_info}" >/dev/null
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier ${BUNDLE_ID}" "${app_info}" >/dev/null

export_dir="${OUTPUT_DIR:-$(pwd)/build/ios-personal-team-export}"
mkdir -p "${export_dir}"

export_options="${workdir}/ExportOptions.plist"
cat > "${export_options}" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>destination</key>
  <string>export</string>
  <key>method</key>
  <string>development</string>
  <key>signingStyle</key>
  <string>automatic</string>
  <key>teamID</key>
  <string>${TEAM_ID}</string>
  <key>stripSwiftSymbols</key>
  <true/>
</dict>
</plist>
EOF

xcodebuild -exportArchive \
  -archivePath "${staging_archive}" \
  -exportPath "${export_dir}" \
  -exportOptionsPlist "${export_options}" \
  -allowProvisioningUpdates \
  -allowProvisioningDeviceRegistration

find "${export_dir}" -maxdepth 1 -name '*.ipa' -print -quit | grep -q . || \
  die "Xcode export finished but no .ipa was produced in ${export_dir}"

printf '[ios-export] Exported IPA to %s\n' "${export_dir}"
