#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

src="/run/media/tulku/OM SYSTEM/"
dst="/mnt/common/Camera/OM1-II/"

if [[ ! -d "$src" ]]; then
  echo "Camera not mounted: $src" >&2
  exit 1
fi
if [[ ! -d "$dst" ]]; then
  echo "Destination missing: $dst" >&2
  exit 1
fi

cargo run --release -- --incremental "$src" "$dst"
#cargo run --release -- --override "$src" "$dst"
