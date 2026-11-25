#!/usr/bin/env bash
# Fetches other builds of OVMF (UEFI firmware for QEMU) so the tough-conditions
# run boots on older and newer firmware than the CI host ships, the way real
# PCs run firmware of every age. Each build comes from its distribution's own
# package repository, inside a throwaway container, into
# build/firmware/<name>/OVMF_CODE*.fd.
#
# Needs docker (GitHub's Ubuntu runners have it). Locally, missing builds are
# skipped with a warning. With STRICT=1 (CI) any missing build fails the step,
# so coverage can never quietly shrink.
set -uo pipefail
cd "$(dirname "$0")/.."

OUT=build/firmware
missing=0
mkdir -p "$OUT"

if ! command -v docker >/dev/null; then
    echo "WARN: docker not available, skipping extra firmware"
    [ "${STRICT:-0}" = 1 ] && exit 1
    exit 0
fi

# fetch <name> <image> <package> <code path> <vars path> (paths without the
# leading /). Each firmware build gets its own matching variable store, as on
# a real PC: some builds hang at power-on without one.
fetch() {
    local name="$1" image="$2" pkg="$3" path="$4" vars="$5"
    mkdir -p "$OUT/$name"
    if docker run --rm -v "$PWD/$OUT/$name:/out" "$image" sh -c "
        set -e
        if command -v apt-get >/dev/null; then
            apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends $pkg
        else
            dnf -q -y install $pkg
        fi
        cp /$path /out/
        cp /$vars /out/OVMF_VARS.fd
    " >/dev/null 2>&1 && ls "$OUT/$name"/OVMF_CODE*.fd "$OUT/$name/OVMF_VARS.fd" >/dev/null 2>&1; then
        echo "firmware: $name ($(basename "$path"))"
    else
        echo "WARN: could not fetch firmware $name"
        missing=$((missing + 1))
        rm -rf -- "${OUT:?}/$name"
    fi
}

fetch edk2-2019-ubuntu20.04 ubuntu:20.04 ovmf usr/share/OVMF/OVMF_CODE.fd usr/share/OVMF/OVMF_VARS.fd
fetch edk2-2022.02-ubuntu22.04 ubuntu:22.04 ovmf usr/share/OVMF/OVMF_CODE.fd usr/share/OVMF/OVMF_VARS.fd
fetch edk2-2022.11-debian12 debian:12 ovmf usr/share/OVMF/OVMF_CODE_4M.fd usr/share/OVMF/OVMF_VARS_4M.fd
fetch edk2-2025.02-debian13 debian:13 ovmf usr/share/OVMF/OVMF_CODE_4M.fd usr/share/OVMF/OVMF_VARS_4M.fd
fetch edk2-latest-fedora fedora:latest edk2-ovmf usr/share/edk2/ovmf/OVMF_CODE.fd usr/share/edk2/ovmf/OVMF_VARS.fd
ls "$OUT"
if [ "${STRICT:-0}" = 1 ] && [ "$missing" -gt 0 ]; then
    echo "ERROR: $missing firmware builds missing"
    exit 1
fi
