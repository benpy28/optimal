#!/usr/bin/env bash
# secureboot-case.sh [case...]
# Secure Boot and tamper tests for Optimal Boot, on OVMF's Secure Boot build
# with throwaway test keys (PK, KEK, db) enrolled and Secure Boot on. Every
# case boots the conformance kernel from a GPT disk:
#
#   signed            loader signed with the db key: must boot and PASS
#   unsigned          unsigned loader: the firmware must refuse it
#   wrong-key         loader signed by a key the firmware doesn't trust: refused
#   tampered-loader   signed loader with one byte changed afterwards: refused
#   dbx               signed loader whose hash is in dbx: refused
#   tampered-kernel   kernel changed after the config pinned it: loader refuses
#   tampered-module   module changed after the config pinned it: loader refuses
#   tampered-config   config edited after signing: loader refuses
#   foreign-config    config signed by a key the loader doesn't trust: refused
#   missing-sig       no config signature at all: loader refuses
#   measured          signed chain with swtpm: PCR 8 and 9 as the loader reads
#                     them back must equal a replay computed on the host from
#                     the config, entry, kernel and module bytes
#
# The keys are made fresh on every run and never leave build/.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=$(mkdir -p ../build/ob && cd ../build/ob && pwd)
SB=$OUT/sb
mkdir -p "$SB"
CODE=${OVMF_SECBOOT_CODE:-/usr/share/OVMF/OVMF_CODE_4M.secboot.fd}
VARS_TEMPLATE=${OVMF_VARS_TEMPLATE:-/usr/share/OVMF/OVMF_VARS_4M.fd}
ALL="signed measured unsigned wrong-key tampered-loader dbx tampered-kernel tampered-module tampered-config foreign-config missing-sig"
cases="${*:-$ALL}"
SIGN=target/release/optimal-boot-sign

newkey() {
    openssl req -new -x509 -newkey rsa:3072 -nodes -sha256 -days 30 \
        -subj "/CN=Optimal Boot throwaway test $1/" \
        -keyout "$SB/$1.key" -out "$SB/$1.crt" 2>/dev/null
}
for k in PK KEK db rogue; do newkey "$k"; done
owner=$(python3 -c 'import uuid; print(uuid.uuid4())')
enrol() { # enrol <out vars> [extra virt-fw-vars args]
    local out="$1"; shift
    virt-fw-vars --input "$VARS_TEMPLATE" --output "$out" \
        --set-pk "$owner" "$SB/PK.crt" --add-kek "$owner" "$SB/KEK.crt" \
        --add-db "$owner" "$SB/db.crt" --secure-boot "$@" >/dev/null 2>&1
}
enrol "$SB/VARS.fd"

# The image every case starts from: conformance kernel, one module, signed config.
python3 -c "import sys; sys.stdout.buffer.write(bytes((i*167+13)&0xff for i in range(100003)))" > "$OUT/pattern"
export OB_REVISION=5 OB_EXPECT_CMDLINE="conformance case=secureboot" OB_EXPECT_MODULES=1
scripts/build-conformance.sh "$OUT/conformance-sb"
KERNEL="$OUT/conformance-sb" CMDLINE="$OB_EXPECT_CMDLINE" MODULES="$OUT/pattern" scripts/build-images.sh >/dev/null
cp "$OUT/disk.img" "$SB/base.img"
img="$SB/case.img"
esp="$img@@1M"
mcopy -n -i "$SB/base.img@@1M" ::EFI/BOOT/BOOTX64.EFI "$SB/loader.efi"
sbsign --key "$SB/db.key" --cert "$SB/db.crt" --output "$SB/loader-signed.efi" "$SB/loader.efi" 2>/dev/null
sbsign --key "$SB/rogue.key" --cert "$SB/rogue.crt" --output "$SB/loader-rogue.efi" "$SB/loader.efi" 2>/dev/null

# flip_byte <file> <offset>: XOR one byte with 0x01.
flip_byte() {
    python3 - "$1" "$2" <<'PY'
import sys
p, o = sys.argv[1], int(sys.argv[2])
b = bytearray(open(p, 'rb').read()); b[o] ^= 1; open(p, 'wb').write(b)
PY
}
put() { mcopy -o -i "$esp" "$1" "$2"; }
get() { mcopy -n -i "$esp" "$1" "$2"; }

status=0
for c in $cases; do
    TPM_ARGS=(); tpm_pid=""
    cp "$SB/base.img" "$img"
    vars="$SB/VARS.fd"
    put "$SB/loader-signed.efi" ::EFI/BOOT/BOOTX64.EFI
    expect=boot
    case "$c" in
        signed) ;;
        unsigned) put "$SB/loader.efi" ::EFI/BOOT/BOOTX64.EFI; expect=firmware ;;
        wrong-key) put "$SB/loader-rogue.efi" ::EFI/BOOT/BOOTX64.EFI; expect=firmware ;;
        tampered-loader)
            cp "$SB/loader-signed.efi" "$SB/t.efi"
            # A byte inside .text: 0x400 is past the PE headers.
            flip_byte "$SB/t.efi" 1024
            put "$SB/t.efi" ::EFI/BOOT/BOOTX64.EFI; expect=firmware ;;
        dbx)
            # dbx holding the Authenticode SHA-256 of the signed loader.
            hash=$(python3 -I - "$SB/loader-signed.efi" <<'PY'
import sys
from virt.peutils import pesign
import pefile
print(pesign.pe_authenticode_hash(pefile.PE(sys.argv[1])).hex())
PY
)
            echo "  dbx entry sha256:$hash"
            # virt-fw-vars has no option for a dbx hash, so use its library.
            python3 -I - "$SB/VARS.fd" "$SB/VARS-dbx.fd" "$owner" "$hash" <<'PY'
import sys
from virt.firmware.varstore import autodetect
src, dst, owner, h = sys.argv[1:]
store = autodetect.open_varstore(src)
vl = store.get_varlist()
vl.add_hash('dbx', owner, h)
store.write_varstore(dst, vl)
PY
            vars="$SB/VARS-dbx.fd"; expect=firmware ;;
        tampered-kernel)
            get ::boot/optimal-kernel "$SB/k"; flip_byte "$SB/k" 4096; put "$SB/k" ::boot/optimal-kernel
            expect="FATAL: /boot/optimal-kernel: SHA-512 does not match" ;;
        tampered-module)
            get ::boot/pattern "$SB/m"; flip_byte "$SB/m" 50000; put "$SB/m" ::boot/pattern
            expect="FATAL: /boot/pattern: SHA-512 does not match" ;;
        tampered-config)
            get ::boot/optimal-boot.conf "$SB/conf"; sed -i 's/case=secureboot/case=secureboox/' "$SB/conf"
            put "$SB/conf" ::boot/optimal-boot.conf
            expect="FATAL: /boot/optimal-boot.conf: config signature is invalid" ;;
        foreign-config)
            "$SIGN" keygen "$SB/foreign.secret" "$SB/foreign.pub"
            get ::boot/optimal-boot.conf "$SB/conf"
            "$SIGN" sign "$SB/foreign.secret" "$SB/conf"
            put "$SB/conf.sig" ::boot/optimal-boot.conf.sig
            expect="FATAL: /boot/optimal-boot.conf: config is signed by a key this loader does not trust" ;;
        measured)
            KERNEL="$OUT/conformance-sb" CMDLINE="$OB_EXPECT_CMDLINE" MODULES="$OUT/pattern" \
                CONFIG_EXTRA="report-pcrs on" scripts/build-images.sh >/dev/null
            cp "$OUT/disk.img" "$img"
            put "$SB/loader-signed.efi" ::EFI/BOOT/BOOTX64.EFI
            get ::boot/optimal-boot.conf "$SB/conf"
            # TPM2 extend: new = SHA-256(old || SHA-256(data)), from 32 zero bytes.
            read -r want8 want9 < <(python3 -I - "$SB/conf" "$OUT/conformance-sb" "$OUT/pattern" "$OB_EXPECT_CMDLINE" <<'PY'
import hashlib, sys
conf, kernel, module, cmdline = sys.argv[1:]
def ext(pcr, data):
    return hashlib.sha256(pcr + hashlib.sha256(data).digest()).digest()
p8 = ext(ext(bytes(32), open(conf, 'rb').read()), f"optimal-boot entry optimal cmdline {cmdline}".encode())
p9 = ext(ext(bytes(32), open(kernel, 'rb').read()), open(module, 'rb').read())
print(p8.hex(), p9.hex())
PY
)
            tpmdir=$(mktemp -d)
            swtpm socket --tpmstate dir="$tpmdir" --ctrl type=unixio,path="$tpmdir/sock" --tpm2 &
            tpm_pid=$!
            sleep 1
            TPM_ARGS=(-chardev socket,id=chrtpm,path="$tpmdir/sock" -tpmdev emulator,id=tpm0,chardev=chrtpm -device tpm-tis,tpmdev=tpm0)
            expect=measured ;;
        missing-sig) mdel -i "$esp" ::boot/optimal-boot.conf.sig; expect="FATAL: /boot/optimal-boot.conf.sig:" ;;
        *) echo "unknown case $c"; exit 2 ;;
    esac
    log="$OUT/sb-$c.log"
    cp "$vars" "$SB/run-vars.fd"
    scripts/run-qemu.sh "$log" 120 -machine q35,smm=on -m 512M \
        -global driver=cfi.pflash01,property=secure,value=on \
        -drive if=pflash,format=raw,unit=0,readonly=on,file="$CODE" \
        -drive if=pflash,format=raw,unit=1,file="$SB/run-vars.fd" \
        -drive format=raw,file="$img" "${TPM_ARGS[@]}"
    if [ -n "$tpm_pid" ]; then kill "$tpm_pid" 2>/dev/null || true; wait "$tpm_pid" 2>/dev/null || true; fi
    ran_loader=$(grep -ac "^Optimal Boot " "$log" || true)
    ok=0
    case "$expect" in
        measured)
            got8=$(grep -a "^OPTIMAL-BOOT: PCR8 sha256 " "$log" | awk '{print $4}')
            got9=$(grep -a "^OPTIMAL-BOOT: PCR9 sha256 " "$log" | awk '{print $4}')
            echo "  PCR 8 replay $want8, TPM ${got8:-none}"
            echo "  PCR 9 replay $want9, TPM ${got9:-none}"
            [ -n "$got8" ] && [ "$got8" = "$want8" ] && [ "$got9" = "$want9" ] \
                && grep -aq "OPTIMAL-CONFORMANCE: PASS" "$log" && ok=1 ;;
        boot) grep -aq "OPTIMAL-CONFORMANCE: PASS" "$log" && grep -aq "^Secure Boot: on" "$log" && ok=1 ;;
        # The firmware refused: it says Access Denied, our loader never
        # printed its banner, and nothing else booted.
        firmware) [ "$ran_loader" = 0 ] && ! grep -aq "OPTIMAL-CONFORMANCE" "$log" \
                      && grep -aq "BdsDxe: failed to load .*: Access Denied" "$log" && ok=1 ;;
        *) grep -aqF "OPTIMAL-BOOT: $expect" "$log" && grep -aq "^Secure Boot: on" "$log" \
               && ! grep -aq "OPTIMAL-CONFORMANCE" "$log" && ok=1 ;;
    esac
    if [ $ok = 1 ]; then
        echo "secure boot $c: ok ($expect)"
    else
        echo "secure boot $c: WRONG (expected: $expect)"; tail -15 "$log"; status=1
    fi
done
exit $status
