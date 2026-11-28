#!/usr/bin/env bash
# Stops the CI runner's time synchronisation for the rest of the job, so the
# host's monotonic clock runs at one fixed rate against its TSC.
#
# Why: QEMU drives the guest's HPET, PM timer and PIT from the host's
# CLOCK_MONOTONIC, which NTP steers, while the guest's TSC is the host's
# TSC (TCG reads it directly; KVM offsets it). While chrony slews the host
# clock, the two run at rates that differ by tens of ppm and change from
# second to second, which no real machine does: there the TSC and the HPET
# are two crystals. The nightly soak of 27 Nov 2026 (run 45919589077) saw
# exactly that, 3 times in 1207 boots: two TSC measurements against the
# HPET 46 and 55 ppm apart a few seconds apart (each good to 8.5 ppm), and
# the 10 s refinement finding the TSC's rate "changed by 23.4 ppm" on a
# steady TSC. Stopping the daemon leaves the kernel's last frequency
# correction in place (chronyd puts back its base frequency on exit), which
# is a constant rate and is measured away like any crystal's error.
#
# The kernel's tolerances are not loosened for this; the host is made to
# behave like hardware instead. Prints what it found, for the job log.
set -uo pipefail

echo "host clock before:"
if command -v chronyc >/dev/null; then
    chronyc -n tracking 2>&1 | sed 's/^/  /'
fi
timedatectl show 2>/dev/null | grep -E '^(NTP|NTPSynchronized)=' | sed 's/^/  /'

stopped=()
for svc in chrony chronyd systemd-timesyncd ntp ntpsec; do
    if systemctl is-active --quiet "$svc" 2>/dev/null; then
        sudo systemctl stop "$svc" && stopped+=("$svc")
    fi
done
sudo timedatectl set-ntp false 2>/dev/null || true
echo "host clock steering: stopped ${stopped[*]:-nothing (none was running)}"
