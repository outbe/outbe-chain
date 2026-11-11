#!/bin/bash
# Build the Outbe validator guest image on top of a pinned dstack checkout.
#
# The dstack image build accepts only a committed tree, so this script works in
# a throwaway clone: it applies the patches, adds the Outbe rootfs overlay and
# the binaries, commits locally with fixed metadata, and runs the dstack
# reproducible build.
#
# Usage: build-image.sh [--check]
#
#   --check   build the image twice and compare the results byte for byte
#             (dstack `repro-check`)
#
# Environment:
#   WORK          working directory (default: $HOME/outbe-tdx-mvp)
#   DSTACK_REPO   dstack git URL or local clone (default: $WORK/dstack)
#   BIN_DIR       output of build-binaries.sh: a directory with bin/ and lib/
#                 (default: $WORK/bin-repro)
#   FLAVORS       dstack image flavors (default: dev)
#   JOBS          build parallelism (default: 4)
#
# The image content is a function of the pinned dstack revision, the files in
# this directory and the files in BIN_DIR. The overlay commit uses a fixed
# author and the dstack revision date, so the same inputs give the same commit
# and the same image.
set -euo pipefail

DSTACK_COMMIT=c88f60e781a62d542e0843b333f18a48d1458b88
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
WORK=${WORK:-$HOME/outbe-tdx-mvp}
DSTACK_REPO=${DSTACK_REPO:-$WORK/dstack}
BIN_DIR=${BIN_DIR:-$WORK/bin-repro}
SRC=$WORK/dstack-outbe
OUT=$WORK/build-outbe

mode=image
case "${1:-}" in
    "") ;;
    --check) mode=check ;;
    *) echo "unsupported argument: $1" >&2; exit 2 ;;
esac

BINARIES=(
    outbe-chain
    outbe-radicle
    outbe-cli
    outbe-feeder
    outbe-tee-enclave
    outbe-tee-enclave-mock
    outbe-tee-enclave-local-e2e
)

for binary in "${BINARIES[@]}"; do
    [ -x "$BIN_DIR/bin/$binary" ] || { echo "missing binary: $BIN_DIR/bin/$binary" >&2; exit 1; }
done
[ -d "$BIN_DIR/lib" ] || { echo "missing library directory: $BIN_DIR/lib" >&2; exit 1; }

# A previous image build leaves root-owned files behind.
for dir in "$SRC" "$OUT"; do
    if [ -e "$dir" ]; then
        rm -rf "$dir" 2>/dev/null || sudo rm -rf --one-file-system "$dir"
    fi
done

git clone --quiet --no-hardlinks "$DSTACK_REPO" "$SRC"
git -C "$SRC" checkout --quiet "$DSTACK_COMMIT"

for patch in "$HERE"/patches/*.patch; do
    git -C "$SRC" apply "$patch"
done

skeleton=$SRC/os/mkosi/mkosi.skeleton
cp -a "$HERE/rootfs/." "$skeleton/"
install -d "$skeleton/usr/local/bin" "$skeleton/usr/local/lib/outbe/lib"
for binary in "${BINARIES[@]}"; do
    install -m 755 "$BIN_DIR/bin/$binary" "$skeleton/usr/local/bin/$binary"
done
for library in "$BIN_DIR"/lib/*; do
    [ -e "$library" ] || continue
    install -m 644 "$library" "$skeleton/usr/local/lib/outbe/lib/$(basename "$library")"
done

commit_date=$(git -C "$SRC" log -1 --format=%cI "$DSTACK_COMMIT")
git -C "$SRC" add -A
GIT_AUTHOR_DATE=$commit_date GIT_COMMITTER_DATE=$commit_date \
    git -C "$SRC" -c user.name=outbe-image-build -c user.email=outbe-image-build@localhost \
    commit --quiet -m "outbe validator image overlay"
echo "image source revision: $(git -C "$SRC" rev-parse HEAD)"

args=(-o "$OUT")
[ "$mode" = check ] && args=(-c "${args[@]}")
FLAVORS=${FLAVORS:-dev} JOBS=${JOBS:-4} \
    nice -n 19 "$SRC/os/mkosi/repro-build/repro-build.sh" "${args[@]}"
