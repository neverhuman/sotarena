# ASPFM Reproducible Submission

> [!IMPORTANT]
> **Current status: replay complete; release packaging blocked.** Correct replay
> covers all **1,790 datasets**, with **564 binary**, **987 regression**, **210
> multiclass**, and **29 timeseries** adaptive winners. Packaging is blocked by
> **129 binary recovery regressions** that do not yet have genuine
> 10-generation exhaustion receipts. The staged `release.json` remains
> `awaiting-recipient`; no encrypted archive or populated public score manifest
> is published.

This README is a current-state and final-workflow preview, not a verified-release
claim. Canonical genomes, fitted programs, and solution source must never be
published as plaintext.

## Release contents and publication state

The verified release is intended to use the following layout. This preview
publishes only this README.

| Path | State in this preview | Purpose |
| --- | --- | --- |
| `README.md` | Available | Public status, replay instructions, and verification contract. |
| `recipient.txt` | Pending | The single public age X25519 recipient used for the final archive. |
| `release.json` | Pending (`awaiting-recipient` in staging) | Final commit marker containing archive, replay, source-tree, snapshot, and task-count provenance. |
| `run.sh` | Pending | Public launcher that gates execution on a `verified` release manifest. |
| `binary/scores.json` | Pending and unpopulated | Public binary scores generated from the packaged replay. |
| `regression/scores.json` | Pending and unpopulated | Public regression scores generated from the packaged replay. |
| `multiclass/scores.json` | Pending and unpopulated | Public multiclass scores generated from the packaged replay. |
| `timeseries/scores.json` | Pending and unpopulated | Public timeseries scores generated from the packaged replay. |
| `aspfm-solutions-v1.tar.zst.age` | Pending, encrypted | Authenticated age-encrypted, self-contained replay workspace. |
| `verification-<sha256>.json` | Pending, external | Content-addressed verification receipt; it is written outside the repository. |

The encrypted workspace will contain the packaged `replay.json`,
`source-files.json`, `expected-rankings.json`, the solution catalog and
hash-addressed solution programs, the replay and ranking-verifier binaries'
source, the evaluator runtime, its locked dependency graph, and vendored
dependencies. These files remain encrypted at rest in the public release;
identities and private keys are never included.

Public scores use exactly `<task>/scores.json` for each of the four tasks.

## Pinned hashes

| Object | SHA-256 |
| --- | --- |
| Input snapshot | `9b70d9cf5b908b7d4af64a8ffde1d79a2c8bf572c46cb1ec721ff0a1287c56fe` |
| Encrypted archive | Pending packaging |
| Packaged `replay.json` | Pending packaging |
| Packaged source tree (`source-files.json` content hash) | Pending packaging |
| Verification receipt | Pending successful verification |

The snapshot hash identifies replay input; it does not imply that the pending
archive, source tree, or release has been verified.

## Maintainer CLI

Run these commands from the repository root. All output, cache, work, runtime,
and evidence paths below are neutral examples and must resolve to the actual
release inputs. Snapshot, replay, package-work, and verification output
directories must be outside the repository where required by the CLI.

Create a content-addressed snapshot:

```sh
cargo run --release --locked -- aspfm snapshot \
  --output /srv/aspfm/cutoffs \
  --quant-root /srv/aspfm/quant \
  --flywheel-root /srv/aspfm/flywheel \
  --jope-dime-root /srv/aspfm/jope-dime \
  --jope-test-adaptive-root /srv/aspfm/jope-test-adaptive \
  --tabarena-evolution-root /srv/aspfm/tabarena-evolution \
  --sotarena-pull-artifacts /srv/aspfm/sotarena-pull-artifacts \
  --index /srv/aspfm/additional-index.json
```

`--index` is optional and repeatable. The other source-root options are
optional at the CLI level, but an auditable release should pass them explicitly
instead of relying on host-specific defaults.

Replay the pinned snapshot:

```sh
cargo run --release --locked -- aspfm replay \
  --snapshot /srv/aspfm/cutoffs/cutoff-9b70d9cf5b908b7d4af64a8ffde1d79a2c8bf572c46cb1ec721ff0a1287c56fe \
  --output /srv/aspfm/replays/replay-9b70d9cf \
  --evaluator /opt/jope-dime/target/release/corpus_eval \
  --data-root /srv/sotarena-data \
  --cache-dir /srv/aspfm/cache \
  --jobs auto
```

`replay` also accepts `--task <task>` and `--dataset <dataset-hash>`;
`--dataset` requires `--task`. `--jobs` accepts `auto` or a positive worker
count.

Package after all replay and genuine recovery-exhaustion evidence passes:

```sh
cargo run --release --locked -- aspfm package \
  --replay /srv/aspfm/replays/replay-9b70d9cf/replay.json \
  --recipient <age1-public-recipient> \
  --output submission/ASPFM/aspfm-solutions-v1.tar.zst.age \
  --runtime-source /opt/jope-dime \
  --work-dir /srv/aspfm/package-work
```

`--recipient` accepts exactly one public age X25519 recipient, never an
identity. Packaging verifies complete adaptive coverage, recovery targets,
rankings, semantic bindings, private-path exclusion, and an offline locked
build before it writes public scores and writes `release.json` last.

Verify the decrypted workspace and completed release replay:

```sh
cargo run --release --locked -- aspfm verify \
  --workspace /srv/aspfm/decrypted \
  --receipts /srv/aspfm/release-run \
  --output /srv/aspfm/verification
```

## Public replay CLI

These launcher commands become usable only after the archive and a
`release.json` with status `verified` are published. The identity argument is a
filesystem path, not identity text; the file must be a regular, non-symlink
file with mode `0600`.

Full release replay:

```sh
submission/ASPFM/run.sh \
  --identity-file /secure/aspfm.identity \
  --data-root /srv/sotarena-data \
  --cache-dir /srv/aspfm/cache \
  --work-dir /srv/aspfm/replay-work \
  --jobs auto
```

One task:

```sh
submission/ASPFM/run.sh \
  --identity-file /secure/aspfm.identity \
  --task binary \
  --data-root /srv/sotarena-data \
  --cache-dir /srv/aspfm/cache \
  --work-dir /srv/aspfm/replay-work \
  --jobs auto
```

One dataset (`--dataset` requires `--task`):

```sh
submission/ASPFM/run.sh \
  --identity-file /secure/aspfm.identity \
  --task regression \
  --dataset 0123456789abcdef \
  --data-root /srv/sotarena-data \
  --cache-dir /srv/aspfm/cache \
  --work-dir /srv/aspfm/replay-work \
  --jobs auto
```

The 95 metadata-only SOTArena datasets require `--data-root`; final
verification does not permit unavailable-data skips. Multiclass and timeseries
also require their hash-pinned `meta.json` sidecars under
`<data-root>/multi_label/<dataset-id>/` and
`<data-root>/timeseries/<dataset-id>/`.

The launcher checks the archive hash before age decryption, verifies every
decrypted source file, builds the Rust 1.88 workspace with `--offline --locked`,
and runs selected winners in deterministic order. `auto` uses
`floor(0.8 * available_parallelism)`, with at least one worker; evaluator
children are limited to one Rayon, OpenMP, and BLAS thread. Atomic receipts make
reruns resumable.

## Integrity checklist

All boxes remain unchecked for this preview. A release is verified only after
every item succeeds against the same packaged replay:

- [ ] Hash `aspfm-solutions-v1.tar.zst.age` and match `archive_sha256` in the final `release.json`.
- [ ] Decrypt the archive with the intended age identity into a new directory outside the repository.
- [ ] Match the `source-files.json` content hash to `source_files_sha256`, then verify every listed source-file SHA-256.
- [ ] Build every packaged binary with Rust 1.88 using `cargo build --offline --locked --release --bins`.
- [ ] Complete and validate replay receipts for every selected adaptive and benchmark-valid winner, including genuine 10-generation receipts for unresolved recovery regressions.
- [ ] Recompute every prediction tape, verify its row count, and match its SHA-256 to the packaged winner and receipt.
- [ ] Regenerate global, per-task, and benchmark-valid rankings and match `expected-rankings.json` without exceeding the accepted rank, Elo, or win-rate regressions.
- [ ] Regenerate and byte-check `binary/scores.json`, `regression/scores.json`, `multiclass/scores.json`, and `timeseries/scores.json` against the packaged replay.
- [ ] Write the external content-addressed verification receipt, then publish the immutable final manifest and artifacts without any identity, private key, token, host-private path, or plaintext solution program.
