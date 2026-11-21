# OptimalFS

Optimal's own filesystem: copy-on-write, every block checksummed or
authenticated-encrypted, atomic commits, instant snapshots and rollback.

- Design and reasoning: [docs/DESIGN.md](docs/DESIGN.md)
- Byte-level on-disk format: [docs/FORMAT.md](docs/FORMAT.md)

It is a standalone `no_std` library today, developed and tortured on the
host. It plugs into the kernel's VFS in Phase 4.

Run the tests with `cargo test` in this folder. `OFS_TORTURE_SCALE=<n>`
makes the torture tests run n times longer, and `OFS_SEED=<seed>` replays a
failure (every failure prints its seed).
