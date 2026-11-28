#!/usr/bin/env bash
# Runs every workload on OptimalFS and on ext4, on the same partition, and
# appends one JSON line per (run, workload, target) to $OUT.
#
#   sudo DEV=/dev/sdb1 DISK=sdb MODE=plain RUNS=5 OUT=results.jsonl ./run.sh
#
# DEV     partition both file systems use (everything on it is destroyed)
# DISK    the whole disk under DEV, whose /sys/block counters are read
# MODE    plain: OptimalFS unencrypted (keyed BLAKE3 on every block) vs
#               ext4 on the bare partition
#         encrypted: OptimalFS encrypted (XChaCha20-Poly1305) vs ext4 on
#               dm-crypt LUKS2 with cryptsetup's defaults (aes-xts-plain64)
# SIZE_GIB  size of the region both use, from the start of DEV (default 16)
# RUNS    repeats (default 5); SCALE multiplies every workload's count
#         (default 1; smaller only for trying the script out)
# BIN     the benchmark binary (default target/release/optimalfs-bench)
#
# Order: within a run, workloads go in a fixed order and the two targets
# alternate which goes first (by run and workload), so drift on the VM
# lands on both. Each target gets a fresh file system for each workload,
# and caches are dropped before each.
set -euo pipefail

: "${DEV:?}" "${DISK:?}" "${MODE:?}" "${OUT:?}"
SIZE_GIB=${SIZE_GIB:-16}
RUNS=${RUNS:-5}
SCALE=${SCALE:-1}
BIN=${BIN:-$(dirname "$0")/target/release/optimalfs-bench}
MNT=${MNT:-/mnt/ofs-bench}
BLOCKS=$((SIZE_GIB * 262144))
KEYFILE=$(mktemp)
printf 'benchmark passphrase' >"$KEYFILE"
CRYPT=ofsbench

mkdir -p "$MNT"

drop() {
  sync
  echo 3 >/proc/sys/vm/drop_caches
}

cleanup() {
  mountpoint -q "$MNT" && umount "$MNT"
  [ -e /dev/mapper/$CRYPT ] && cryptsetup close $CRYPT
  return 0
}
trap cleanup EXIT

# The block device ext4 goes on: the partition, or LUKS2 on it.
ext4_dev() {
  if [ "$MODE" = encrypted ]; then
    cryptsetup luksFormat --batch-mode --type luks2 --sector-size 4096 \
      --key-file "$KEYFILE" "$DEV" >/dev/null
    cryptsetup open --key-file "$KEYFILE" "$DEV" $CRYPT
    echo /dev/mapper/$CRYPT
  else
    echo "$DEV"
  fi
}

run_ext4() {
  local w=$1 run=$2 dev
  dev=$(ext4_dev)
  # Ubuntu's mke2fs.conf defaults, except: no discard (OptimalFS issues
  # none either) and inode tables and journal initialised now instead of by
  # a background thread after mount, which would write during the runs.
  mkfs.ext4 -F -q -E nodiscard,lazy_itable_init=0,lazy_journal_init=0 "$dev" "${SIZE_GIB}G"
  mount "$dev" "$MNT" # default mount options
  if [ "$run" = 1 ] && [ "$w" = page-rewrite ]; then
    echo "ext4 mount: $(grep " $MNT " /proc/mounts)" >&2
    dumpe2fs -h "$dev" 2>/dev/null | grep -E 'features|Block size|Inode size|Journal size|Inode count' >&2 || true
    [ "$MODE" = encrypted ] && { cryptsetup status $CRYPT >&2; dmsetup table $CRYPT >&2; }
  fi
  drop
  "$BIN" "$w" --target ext4 --mnt "$MNT" --stat-disk "$DISK" --mode "$MODE" \
    --run "$run" --scale "$SCALE" --out "$OUT"
  umount "$MNT"
  [ "$MODE" = encrypted ] && cryptsetup close $CRYPT
  return 0
}

run_ofs() {
  local w=$1 run=$2 enc=()
  [ "$MODE" = encrypted ] && enc=(--encrypted)
  drop
  "$BIN" "$w" --target ofs --dev "$DEV" --blocks "$BLOCKS" "${enc[@]}" \
    --stat-disk "$DISK" --mode "$MODE" --run "$run" --scale "$SCALE" --out "$OUT"
}

mapfile -t WORKLOADS < <("$BIN" --list)
for run in $(seq 1 "$RUNS"); do
  i=0
  for w in "${WORKLOADS[@]}"; do
    if (( (run + i) % 2 == 0 )); then order=(ofs ext4); else order=(ext4 ofs); fi
    for t in "${order[@]}"; do
      echo "== run $run/$RUNS, $w, $t ($MODE)" >&2
      "run_$t" "$w" "$run" >/dev/null
    done
    i=$((i + 1))
  done
done
