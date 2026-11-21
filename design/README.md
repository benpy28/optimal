# Optimal design

The [roadmap](../ROADMAP.md) says what gets built and in what order. These
files say how, precisely enough that every claim can be checked. Where the
roadmap and a design file disagree, that's a bug in one of them; fix it.

| File | Covers |
|---|---|
| [threat-model.md](threat-model.md) | attackers, assets, the trusted computing base, hardware security tiers, lockdown, and every attack path with its defence and test |
| [core.md](core.md) | the `unsafe` core, the soundness rule for its APIs, crate layout, lint and inventory enforcement, panic policy, outside audits |
| [verification.md](verification.md) | proof targets, assurance levels, tools, the assumption ledger, keeping proofs in sync |
| [drivers.md](drivers.md) | trust tiers and placement, hardware access handles, the IOMMU, the driver message interface, budgets, faults and restart, hotplug, GPUs |
| [capabilities.md](capabilities.md) | the capability system and how Linux fds, paths, uids, sharing and permissions map onto it; portals and app manifests |
| [compat.md](compat.md) | the Linux ABI: decisions, milestones, `/proc` and `/sys`, and how compatibility is measured |
| [scheduler.md](scheduler.md) | preemption, scheduling classes, topology, time and idle, memory management performance |
| [storage.md](storage.md) | block layer, durability contract, page cache, encryption layering, ext4 rules, crash testing |
| [boot.md](boot.md) | the boot chain, KASLR, microcode, Secure Boot paths, measured boot and TPM policy, the verified system image, updates, anti-rollback and recovery |
| [benchmarks.md](benchmarks.md) | where numbers come from, the regression gate, the Linux baseline, targets, the benchmark set and the scorecard |

Component designs that live next to their code: `bootloader/DESIGN.md` (Optimal
Boot internals) and `fs/docs/DESIGN.md` and `fs/docs/FORMAT.md` (the own file
system).

## Rules for the design

- **Every claim has a test.** If a property is stated here, a test or proof
  for it is named here, with the phase it becomes a gate.
- **Numbers, not adjectives.** "Fast", "small" and "secure" are replaced by
  a metric, a threshold and the hardware it's measured on.
- **Say what isn't done.** Anything not yet built or measured is labelled
  that way on the scorecard. Nothing is claimed early.
- **Design changes are reviewed changes.** Loosening a target, a proof bound
  or a security default needs a written reason in the same change.

## Review log

The design is reviewed in rounds by independent, hostile reviewers, each from
a different angle (security, verification, drivers, compatibility,
scheduling and benchmarks, storage, boot, claims against evidence), until a
round finds nothing blatant.

| Round | Date | Main holes found | Result |
|---|---|---|---|
| 1 | 2026-11-21 | no threat model; disk encryption sealed to a TPM before any TPM driver or measured boot; IOMMU only in Phase 4; KASLR claimed on a static kernel; tier 2 trusted third-party code in the kernel; no ia32, namespaces, seccomp or audio despite later phases needing them; performance gate on noisy shared VMs and a Phase 2 comparison with Linux before user space; ext4 read-write with no safety rules; no durability contract; no update keys, anti-rollback or recovery; proofs with no assurance levels or axioms | all fixed in this directory and the roadmap |
| 2 | 2026-11-21 | several round-1 fixes were themselves wrong: a KASLR test that 100 boots can't pass, unwinding tier-2 panics (impossible on stable `no_std` Rust), a MOK Secure Boot path, PCR 7 signed in CI, no writable file system in Phase 4, a terminal home grant that broke the threat model, a tier-1 storage restart gate; also missing user-space TCB and mediated channels (D-Bus, PipeWire, XWayland, clipboard), a gate rule too loose to catch 2% regressions, tests scheduled before the things they test exist, no IOMMU free-after-invalidation rule, no GPU isolation proof, no PI/placement/tick tests | all fixed; old S1 to S4 tiers replaced by boot tiers B1 to B4 and DMA tiers D1 and D2 |
| 3 | 2026-11-21 | two round-2 fixes still wrong: the anti-rollback floor could never be raised because PCRs 8 and 9 are capped before the health check, and the KASLR entropy gate was unpassable for an ideal 9-bit picker; SMT policy contradicted itself; fds had no open file description layer, and passed fds could be widened through `/proc` or `..`; unpackaged identity by path let terminal code inherit network grants; no-IOMMU DMA claim ignored descriptor and register addresses, and D2 put Wi-Fi parsing in the kernel; AES-GCM nonces repeated after a commit fallback; proofs didn't reach the `unsafe` shims and had no rule for combining levels; the bare-metal benchmark gate had no way to run; PCR capping only after a successful unseal; a minimum product that couldn't be patched; proof gates and attack tests missing from the roadmap | all fixed |

## Decisions log

One line each: what was chosen, why, and what was rejected.

- Tier-2 panics poison and park the component instead of unwinding, because stable `no_std` Rust can't unwind; rejected: unstable unwinding, making every tier-2 panic a kernel crash.
- KASLR entropy is tested on 10^6 host draws plus distinct bases in 100 boots, because 100 samples can't show 9 bits; rejected: a boot-count-only test. (Round 3: at least 2^16 slots, 90B at least 9 bits, at least 95 distinct bases.)
- Secure Boot has two paths, a Microsoft-signed shim or owner keys; rejected: MOK-only, because shim is the thing MOK depends on.
- PCR 7 is bound at install and resealed through a PolicyOR, because it can't be predicted in CI; rejected: signing PCR 7 values.
- Phase 4 gets read-write ext4 only on volumes our own mkfs created, with a fixed feature list and a differential `e2fsck` gate; rejected: read-only until the own file system, or read-write on any ext4.
- Phase 4 splits into 4a (storage) and 4b (trusted chain, entered once Optimal Boot measures), so storage isn't blocked by the bootloader track; rejected: one phase gated on both.
- The terminal gets home minus protected paths and no network by default; rejected: whole home with network.
- Gate rule is 2% point estimate or 1% CI bound, Holm–Bonferroni corrected, plus a 3% per-release budget; rejected: the round-1 "CI entirely beyond 3%" rule, which let steady 2% losses through.
- Desktop-dependent targets (memory pressure, foreground-during-I/O, idle memory and CPU) move to Phase 7 and 144 Hz to Phase 8, because there's nothing to measure earlier; rejected: keeping early phases with no workload.
- Base image uses dash and uutils, and allows only GCC-runtime-exception libraries; GPL programs like bash come from the store; rejected: no glibc (breaks compatibility) or shipping GPL programs.
- The anti-rollback floor is raised by the loader on the next boot, before PCRs are capped, from a request written after the health check, and the floor is counted from a write-once base because TPM counters don't start at 0; rejected: raising it from user space after capping (impossible), signing post-cap PCR values (ties every release to every boot path).
- KASLR picks the virtual base from at least 2^16 slots so a 9-bit 90B estimate is reachable; rejected: lowering the gate to fit 512 slots.
- SMT is off by default on CPUs whose siblings leak kernel data, with core scheduling as an opt-in for app-to-app isolation; rejected: core scheduling as the default defence, which doesn't protect kernel data.
- Before Phase 6, a polled boot-protocol USB keyboard reader moves into 4a so an encrypted disk can be unlocked on USB-only desktops; rejected: naming a PS/2-only reference machine and leaving USB desktops unable to boot.
- fds point to shared open file descriptions that carry offset, flags and locks; rejected: copying (object, rights) per fd, which breaks `dup`, `fork` and shell redirection.
- Network policy is a per-identity egress filter with a separate inbound grant; rejected: per-app network namespaces, which break localhost development and can't change live sockets.
- Unpackaged app identity is path plus content hash, and only the launcher and service manager can spawn a process with an identity; rejected: identity by install location alone.
- Wi-Fi and Bluetooth always split into a tier-2 transport and a tier-3 protocol stack; on no-IOMMU machines radios stay off until the user accepts a named exposure; rejected: an in-kernel Wi-Fi build on D2.
- Encryption nonces mix a random per-mount salt into the subkey; rejected: (address, generation) alone, which repeats after a fallback.
- The `unsafe` shims are a separate, tested-only target with named axioms and a size cap, and a proof's reported level is the weakest level it rests on; rejected: claiming the shims are covered by the pure-crate proofs.
- The signed update client and transparency checks move into Phase 7; rejected: a "secure" minimum product with no way to ship fixes.
- Benchmarks run from a controller host that netboots and power-cycles the machine under test; rejected: a GitHub runner on the machine under test, which can't run before Optimal has user space and networking.
