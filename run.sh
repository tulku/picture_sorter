#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "Usage: $0 /absolute/profile.toml /mounted/camera/path [import options]" >&2
  exit 2
fi
profile=$1
card=$2
shift 2
if [[ ! -f "$profile" ]]; then
  echo "Explicit profile missing: $profile; initialize and review it first." >&2
  exit 2
fi
if [[ ! -d "$card" ]]; then
  echo "Camera directory missing: $card" >&2
  exit 3
fi
exec "${PHOTO_SORTER_BIN:-photo_sorter}" --config "$profile" import "$card" "$@"
