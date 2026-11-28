Kernel tests: the frame drain test no longer makes a lazily backed 32 GiB
guest use all of the host's memory. QEMU backs guest memory with 2 MiB
transparent huge pages, so the old sampling (one write per 256 KiB above
1 GiB) backed all 32 GiB, and the tough-conditions runner swapped past the
30 s deadline or killed QEMU. The test now writes every frame below 1 GiB and
whole 2 MiB windows above it, at most 128 windows spread evenly over memory,
so it writes at most 1.25 GiB on any machine size. The 32 GiB scenarios now
fail if QEMU's peak host memory goes over 3 GiB (QEMU_MAX_RSS_MB in
scripts/qemu.sh); with the fix it peaks at about 1.5 GiB.
