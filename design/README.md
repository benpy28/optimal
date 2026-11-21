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
