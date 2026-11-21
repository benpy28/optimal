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
| [features.md](features.md) | the owner's optional features (sensor indicators, privacy dashboard, hidden vault, panic lock, travel mode, leak guard, tamper alert, game overlay, focus boost, power meter, system undo, file time-travel, safe mode, no-surprise updates, ad blocking, export, customisation), each with its default, mechanism, honest limits, phase and test |
| [channels.toml](channels.toml) | every portal and channel, checked by CI |

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
| 4 | 2026-11-21 | round-3 fixes broke or missed things: raising the floor or resealing PCR 7 killed every sealed object except the disk secret; the hibernation key had three incompatible definitions and resume from suspend had no TPM object; attacker-controlled LUKS2 JSON and the volume map were parsed before capping; the secure-attention key could come from a compromised Bluetooth or HID process; apps without network could exfiltrate through open-URI, the resolver and loopback; identity hashed only the main executable; the per-app home broke dotfile apps and the file chooser had no document portal; early-boot DMA and devices ignoring their own BME; a slow disk could panic the kernel; AMD-Vi left unassigned requester IDs open; the KVM drift test needed a driver that doesn't exist in Phase 1; gate statistics too weak for boot-to-boot noise; proofs gated before their code exists, and no target for crypto, packet parsers or the floor state machine; the own file system's format capped volumes at 512 GiB | all fixed in this directory and the roadmap; file system findings sent to its track |
| 5 | 2026-11-21 | the resume and hibernation objects let a TPM unseal skip the PIN; signed PCR 8/9-only approvals bypassed PCR 4; the PCR 7 reseal's test-unseal couldn't run while the system was up; the volume tag missed the LUKS2 secondary header; the 4-slot superblock couldn't reach back K commits; AML could rewrite any device's PCI config through ECAM; the transitive one-principal rule merged every session into one component; private homes keyed by path contradicted the identity rules; Phase 5 needed networking that only arrives in Phase 6; tail-latency gates broke their own statistics and budget; plus the owner's new approve-before-access feature (USB authorization, per-app network, untrusted downloads, prompt levels with a floor) | all fixed in this directory and the roadmap; file system findings sent to its track |
| 6 | 2026-11-21 | apps that look up a host name never raised the first-use network request; "downloaded" had two definitions and one blocked every self-built program; camera, microphone, location and screenshots could reach the network with no prompt; Verus couldn't reason about the shipped atomics; a replay bump wrote all four superblock slots in one flush; one volume tag over both LUKS2 copies had no atomic update or recovery; the PIN was typed before the loader's DMA protection; 2 witnesses with threshold 2 could stop all updates; plus the owner's 17 optional features, whose panic lock zeroed pages frozen apps still mapped and whose wipe lists missed vault and travel keys | all fixed in this directory and the roadmap; file system findings sent to its track |

## Decisions log

One line each: what was chosen, why, and what was rejected.

- Tier-2 panics poison and park the component instead of unwinding, because stable `no_std` Rust can't unwind; rejected: unstable unwinding, making every tier-2 panic a kernel crash.
- KASLR entropy is tested on 10^6 host draws plus distinct bases in 100 boots, because 100 samples can't show 9 bits; rejected: a boot-count-only test. (Round 3: at least 2^16 slots, 90B at least 9 bits, at least 95 distinct bases.)
- Secure Boot has two paths, a Microsoft-signed shim or owner keys; rejected: MOK-only, because shim is the thing MOK depends on.
- PCR 7 is bound at install and resealed through a PolicyOR, because it can't be predicted in CI; rejected: signing PCR 7 values.
- Phase 4a gets read-write ext4 only on volumes our own mkfs created, with a fixed feature list and a differential `e2fsck` gate; rejected: read-only until the own file system, or read-write on any ext4.
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
- Every sealed TPM object is listed once, and every floor raise or PCR 7 reseal reseals and test-unseals all of them before the counter moves; rejected: binding the secondary objects with no floor check, which would let old releases use them.
- Every sealed TPM object seals only a random value and the keys are encrypted under it mixed with the second PIN half, so no unseal skips the PIN; rejected: sealing keys directly in the resume and hibernation objects.
- PCR-4-per-machine objects use their own policy reference, the only one PCR 8/9-only approvals are signed under; rejected: one reference for all machines, which let those approvals bypass PCR 4.
- Each LUKS2 header copy has its own tag, updated through a pending tag set, with a recovery-key re-tag flow, and a header is never chosen by seqid; rejected: one tag over both copies (a single changed byte or a Linux `cryptsetup` write bricked the volume), trusting the higher-seqid copy.
- The own file system keeps the previous 8 commits' root sets in every superblock body; rejected: a separate root ring, and a 30 s time hold.
- Metadata stored twice uses 64-byte logical pointers and a chunk table; rejected: widening pointers to 80 bytes, which costs fanout on every volume.
- All ACPI bytecode access to the ECAM window goes through the PCI config filter; rejected: confining only port I/O config access.
- Channels are one-way and the one-principal rule runs on a directed graph; rejected: the undirected rule, which merged every session.
- Apps get no network by default, with one first-use request per app; rejected: network on by default (reverses round 4).
- Prompts have three levels, "Always ask", "Only when suspicious" (default) and "Almost never", over a fixed floor (inbound remote access, new keyboard-like devices, files plus network for one identity) that Settings explains; rejected: a level that turns everything off.
- A remembered keyboard on the same port needs no code and the clone case is a listed residual, because USB can't authenticate devices; rejected: a code on every replug (prompt fatigue).
- Tail metrics pool events per boot with boots as clusters over rolling 5-night windows; rejected: one long release run.
- The hibernation key is a per-boot key sealed before capping with the full disk policy, and resume from suspend uses an object bound to this boot's capped PCRs plus the PIN; rejected: post-cap PolicySecret for hibernation, which drops the PCR 4 check.
- Only a fixed binary header block is parsed before capping; LUKS2 JSON and the volume map wait until after; rejected: parsing the attacker-controllable JSON with secrets live.
- The secure-attention key and prompt answers are accepted only from in-kernel input parsers or confirmed per-device HID processes, never Bluetooth by default; rejected: trusting every tier-3 input stack.
- Services that send traffic for an app check the app's own grants, and the one-principal rule is transitive over a session; rejected: pairwise checks, which a relay app defeats.
- Each app gets a persistent private home and a document portal; rejected: a home containing only the app's own config directories, which breaks Wine, Steam and every dotfile app.
- D1 needs firmware pre-boot DMA protection or the loader's deny-all set-up, and DMA is considered stopped only by the IOMMU and upstream bridges; rejected: trusting a device's own Bus Master Enable bit.
- Benchmarks use at least 20 boots per build with t-based intervals and a between-boot noise ceiling; rejected: 10 boots with a normal-approximation bootstrap, which overstated power.
- The KDF floor scales with RAM (min(1 GiB, RAM/4), at least 64 MiB, at least 4 passes) with 256 MiB as the minimum for an encrypted install; rejected: a fixed 1 GiB floor that can't unlock small machines.
- A name lookup is a first-use network event, silent decisions are synchronous, and a pending human decision holds the call for up to 30 s; rejected: refusing lookups with `ECONNREFUSED`, which meant the request never fired.
- "Downloaded" has one definition (download folders, portal transfers from networked identities, writes by a process that read a non-loopback socket), and the terminal running its own files never prompts; rejected: tagging every file written by any networked identity, which broke every build.
- The prompt floor counts a component's whole data set (files, camera, microphone, screen capture and screenshots, location, background clipboard) with network; rejected: counting file grants only.
- The shipping kernel uses a vendored, audited exec subset of vstd so Verus proofs cover the real atomics; rejected: proving an abstract model only, unless the Phase 3 spike fails.
- The loader installs its deny-all IOMMU tables before the PIN prompt and any unseal; rejected: doing all firmware I/O first.
- At least 4 transparency witnesses with threshold 2 and an offline-root emergency change; rejected: 2 of 2, where losing one witness stops all updates.
- Panic lock unmaps and evicts page-cache pages before zeroing them, closes vaults and lists what stays in RAM; rejected: zeroing pages under frozen apps' mappings.
- One key inventory table in storage.md gives every key's fate at lock, suspend, panic, hibernate and shutdown, and every canary set comes from it; rejected: per-section wipe lists, which kept missing keys.
- The typed-code floor applies only to HID interfaces with keyboard or system-control usages, and every HID identity includes its port path; rejected: asking for every HID device (mice and headsets can't type a code).
