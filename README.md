# SOTArena

SOTArena is a JSON-backed benchmark with deterministic Elo rankings. Every manifest and result is loaded and validated. Elo uses each task's analysis cohort: the datasets with a valid canonical score from every automatically Global-eligible family. The referenced blacklists record the auditable complement without removing corpus data. All benchmark content lives under `data/`.

## Getting Started

Clone and build the reporter:

```sh
git clone https://github.com/neverhuman/sotarena.git
cd sotarena
cargo build --release
```

Download one dataset, a whole task, or every downloadable dataset:

```sh
cargo run --release -- fetch --task binary --dataset ad2b3ffae29d73f6
cargo run --release -- fetch --task binary
cargo run --release -- fetch --all
```

Downloads default to `$HOME/.cache/sotarena/benchmark`. To use another directory outside the repository, pass `--cache-dir /path/to/cache` or set `SOTARENA_BENCHMARK_CACHE`.

Generate the JSON report, leaderboard SVGs, and refresh this README:

```sh
cargo run --release -- report --out report.json
```

The retained corpus has 1,790 manifests. Of those, 1,695 datasets are downloadable (3,390 files and 592,077,600 expected bytes); 95 metadata-only datasets are reported and skipped by bulk fetches. Manifests with fewer than 300 combined train/test rows are rejected.

## Secure Solution Archives

Solution directories can be streamed through `tar`, zstd, and authenticated age v1 X25519 encryption without writing a plaintext archive:

```sh
cargo run --release -- keygen --identity /secure/sotarena.identity --recipient sotarena.recipient
cargo run --release -- encrypt --input solutions --output solutions.tar.zst.age --recipient age1...
cargo run --release -- decrypt --input solutions.tar.zst.age --output decrypted-solutions --identity /secure/sotarena.identity
```

Repeat `--recipient age1...` to encrypt for multiple recipients. Keep private identities outside this repository; only public recipients and binary `.age` archives are safe to track. Lost identities are unrecoverable, and changing recipients requires re-encryption. A private identity sent by email is only as secure as that email channel. X25519 age is not post-quantum encryption and no encryption scheme provides absolute security. Decryption only extracts files: inspect demos before running them manually.

## Global Elo

Datasets: 1790. Download all: `cargo run --release -- fetch --all`.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/leaderboards/global-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="assets/leaderboards/global-light.svg">
  <img alt="Global Elo leaderboard" src="assets/leaderboards/global-light.svg">
</picture>

<details>
<summary>Markdown fallback</summary>

| Rank | Family | Elo |
| ---: | --- | ---: |
| 1 | **ASPFM** | **1894.2** |
| 2 | **TabFM Ensemble** | **1633.0** |
| 3 | **TabICL** | **1600.1** |
| 4 | **TabPFN v3** | **1582.7** |
| 5 | **Fable 5** | **1438.1** |
| 6 | **OpenAI \*sol** | **1332.8** |
| 7 | **Grok 4.5** | **1312.3** |
| 8 | **Kimi K3** | **1206.7** |

</details>

## Binary Elo

Datasets: 564. Download all: `cargo run --release -- fetch --task binary`.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/leaderboards/binary-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="assets/leaderboards/binary-light.svg">
  <img alt="Binary Elo leaderboard" src="assets/leaderboards/binary-light.svg">
</picture>

<details>
<summary>Markdown fallback</summary>

| Rank | Method | Elo |
| ---: | --- | ---: |
| 1 | **ASPFM** | **2095.8** |
| 2 | **TabFM Ensemble** | **1788.2** |
| 3 | **TabICL** | **1763.7** |
| 4 | **TabPFN v3** | **1723.9** |
| 5 | **catboost** | **1689.8** |
| 6 | **ebm** | **1665.0** |
| 7 | **Fable 5** | **1648.6** |
| 8 | **ada\_boost** | **1622.5** |
| 9 | **gradient\_boosting** | **1599.8** |
| 10 | **voting\_soft** | **1584.9** |
| 16 | **Grok 4.5** | **1528.9** |
| 17 | **OpenAI \*sol** | **1526.5** |
| 26 | **Kimi K3** | **1445.7** |

</details>

## Regression Elo

Datasets: 987. Download all: `cargo run --release -- fetch --task regression`.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/leaderboards/regression-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="assets/leaderboards/regression-light.svg">
  <img alt="Regression Elo leaderboard" src="assets/leaderboards/regression-light.svg">
</picture>

<details>
<summary>Markdown fallback</summary>

| Rank | Method | Elo |
| ---: | --- | ---: |
| 1 | **ASPFM** | **2334.5** |
| 2 | **TabFM Ensemble** | **1932.8** |
| 3 | **TabPFN v3** | **1877.6** |
| 4 | **TabICL** | **1877.3** |
| 5 | **catboost** | **1761.4** |
| 6 | **Fable 5** | **1760.4** |
| 7 | **stacking\_trees\_meta** | **1723.1** |
| 8 | **gaussian\_process** | **1683.4** |
| 9 | **Grok 4.5** | **1646.3** |
| 10 | **gradient\_boosting** | **1641.6** |
| 15 | **OpenAI \*sol** | **1575.6** |
| 45 | **Kimi K3** | **1350.0** |

</details>

## Multiclass Elo

Datasets: 210. Download all: `cargo run --release -- fetch --task multiclass`.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/leaderboards/multiclass-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="assets/leaderboards/multiclass-light.svg">
  <img alt="Multiclass Elo leaderboard" src="assets/leaderboards/multiclass-light.svg">
</picture>

<details>
<summary>Markdown fallback</summary>

| Rank | Method | Elo |
| ---: | --- | ---: |
| 1 | **TabFM Ensemble** | **1763.6** |
| 2 | **ASPFM** | **1725.4** |
| 3 | **TabICL** | **1715.6** |
| 4 | **TabPFN v3** | **1675.0** |
| 5 | **CatBoost** | **1651.0** |
| 6 | **mitra\_classifier\_default** | **1628.6** |
| 7 | **mc20\_tabdpt** | **1623.2** |
| 8 | **limix\_2m\_classifier\_default** | **1612.4** |
| 9 | **mc20\_xgboost\_tuned** | **1577.1** |
| 10 | **mc20\_xgboost\_hist\_240** | **1558.9** |
| 18 | **Fable 5** | **1523.7** |
| 31 | **OpenAI \*sol** | **1420.3** |
| 36 | **Grok 4.5** | **1291.2** |
| 37 | **Kimi K3** | **1270.1** |

</details>

## Timeseries Elo

Datasets: 29. Download all: `cargo run --release -- fetch --task timeseries`.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/leaderboards/timeseries-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="assets/leaderboards/timeseries-light.svg">
  <img alt="Timeseries Elo leaderboard" src="assets/leaderboards/timeseries-light.svg">
</picture>

<details>
<summary>Markdown fallback</summary>

| Rank | Method | Elo |
| ---: | --- | ---: |
| 1 | **ASPFM** | **2020.5** |
| 2 | **TimesFM** | **1693.5** |
| 3 | **chronos\_2\_small** | **1666.0** |
| 4 | **TabPFN v3** | **1645.6** |
| 5 | **TabICL** | **1616.3** |
| 6 | **moirai\_2\_small** | **1600.2** |
| 7 | **TabFM Ensemble** | **1539.8** |
| 8 | **sktime\_reduction\_lightgbm** | **1526.1** |
| 9 | **auto\_arima** | **1461.4** |
| 10 | **seasonal\_naive** | **1460.8** |
| 15 | **OpenAI \*sol** | **1429.7** |
| 16 | **Grok 4.5** | **1420.9** |
| 17 | **Fable 5** | **1412.0** |
| 18 | **Kimi K3** | **1396.7** |

</details>
