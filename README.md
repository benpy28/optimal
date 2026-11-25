# Optimal

A desktop OS built on its own from-scratch kernel, written in Rust. The aim is to beat matched Linux on the published desktop benchmark set and keep every promise in the threat model on the reference machines, while running unmodified Linux programs from each compatibility milestone's suite at the pass rate the scorecard publishes ([design/compat.md](design/compat.md)).

- Plan: [ROADMAP.md](ROADMAP.md)
- Kernel source: [`kernel/`](kernel/)
- Every push is built and boot-tested in QEMU by GitHub Actions. The bootable ISO is attached to each run under Artifacts.
