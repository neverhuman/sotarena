use anyhow::{ensure, Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "sotarena",
    about = "Download, verify, and rank SOTArena JSON data"
)]
struct Cli {
    #[arg(long, global = true, default_value = ".", hide = true)]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download and verify one dataset, one task, or every downloadable dataset.
    Fetch {
        #[arg(long, required_unless_present = "all", conflicts_with = "all")]
        task: Option<String>,
        #[arg(long, requires = "task", conflicts_with = "all")]
        dataset: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        cache_dir: Option<PathBuf>,
    },
    /// Validate standardized result records and emit deterministic Elo tables.
    Report {
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema: String,
    tasks: Vec<TaskConfig>,
    elo: EloConfig,
    families: Vec<FamilyConfig>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskConfig {
    id: String,
    metric: String,
    direction: Direction,
    weight: f64,
    analysis_blacklist: PathBuf,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Direction {
    Maximize,
    Minimize,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Maximize => "maximize",
            Self::Minimize => "minimize",
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EloConfig {
    initial_rating: f64,
    k_factor: f64,
    tie_tolerance: f64,
    iterations: usize,
    regularization: f64,
    convergence_tolerance: f64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct FamilyConfig {
    id: String,
    display_name: String,
    patterns: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct Dataset {
    schema: String,
    dataset_hash: String,
    dataset_id: String,
    task: String,
    metric: String,
    train: DataFile,
    test: DataFile,
}

#[derive(Clone, Deserialize)]
struct DataFile {
    url: Option<String>,
    sha256: String,
    bytes: u64,
    rows: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultRecord {
    schema: String,
    task: String,
    metric: String,
    method_id: String,
    display_name: String,
    claim_scope: String,
    dataset_id: String,
    dataset_hash: String,
    value: f64,
    test_sha256: String,
    predictions_sha256: Option<String>,
    run_id: String,
}

type DatasetCatalog = BTreeMap<String, BTreeMap<String, Dataset>>;
type MethodScores = BTreeMap<String, f64>;
type TaskResults = BTreeMap<String, MethodScores>;
type ResultCatalog = BTreeMap<String, TaskResults>;

#[derive(Clone)]
struct CanonicalParticipants {
    scores: TaskResults,
    display_names: BTreeMap<String, String>,
}

type ParticipantCatalog = BTreeMap<String, CanonicalParticipants>;
type AnalysisBlacklists = BTreeMap<String, BTreeSet<String>>;

struct LoadedResults {
    catalog: ResultCatalog,
    result_jsons: BTreeMap<String, usize>,
    result_jsons_total: usize,
}

#[derive(Serialize)]
struct Report {
    schema: &'static str,
    datasets_total: usize,
    result_jsons_total: usize,
    score_rows: usize,
    global: Vec<GlobalRow>,
    tasks: Vec<TaskTable>,
}

#[derive(Serialize)]
struct TaskTable {
    task: String,
    metric: String,
    metric_direction: &'static str,
    weight: f64,
    datasets: usize,
    analysis_datasets: usize,
    blacklisted_datasets: usize,
    result_jsons: usize,
    score_rows: usize,
    methods: usize,
    rows: Vec<TaskRow>,
}

#[derive(Serialize)]
struct TaskRow {
    rank: usize,
    method_id: String,
    elo: f64,
    datasets_scored: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnalysisBlacklist {
    schema: String,
    task: String,
    datasets: Vec<String>,
}

#[derive(Deserialize)]
struct ScoreDocument {
    schema: String,
    task: String,
    dataset_hash: String,
    metric: String,
    rows: Vec<ScoreDocumentRow>,
}

#[derive(Deserialize)]
struct ScoreDocumentRow {
    method_id: String,
    value: Option<f64>,
    status: String,
}

#[derive(Serialize)]
struct GlobalRow {
    rank: usize,
    family_id: String,
    display_name: String,
    elo: f64,
}

#[derive(Clone, Copy)]
enum Theme {
    Light,
    Dark,
}

impl Theme {
    fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn palette(self) -> Palette {
        match self {
            Self::Light => Palette {
                background: "#ffffff",
                header_background: "#f6f8fa",
                border: "#d0d7de",
                neutral: "#57606a",
                red: (207, 34, 46),
                amber: (154, 103, 0),
                green: (26, 127, 55),
            },
            Self::Dark => Palette {
                background: "#0d1117",
                header_background: "#161b22",
                border: "#30363d",
                neutral: "#8b949e",
                red: (255, 123, 114),
                amber: (210, 153, 34),
                green: (63, 185, 80),
            },
        }
    }
}

struct Palette {
    background: &'static str,
    header_background: &'static str,
    border: &'static str,
    neutral: &'static str,
    red: (u8, u8, u8),
    amber: (u8, u8, u8),
    green: (u8, u8, u8),
}

struct LeaderboardRow<'a> {
    rank: usize,
    label: &'a str,
    elo: f64,
}

struct Leaderboard<'a> {
    id: &'a str,
    title: String,
    label_header: &'static str,
    rows: Vec<LeaderboardRow<'a>>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Fetch {
            task,
            dataset,
            all: _,
            cache_dir,
        } => fetch(&cli.root, task.as_deref(), dataset.as_deref(), cache_dir),
        Command::Report { out } => {
            if let Some(path) = &out {
                ensure!(
                    path.extension().and_then(OsStr::to_str) == Some("json"),
                    "report output must have a .json extension"
                );
            }
            let (bytes, report) = render_report(&cli.root)?;
            write_leaderboard_assets(&cli.root, &report)?;
            fs::write(cli.root.join("README.md"), render_readme(&report))?;
            if let Some(path) = out {
                fs::write(&path, bytes)?;
            } else {
                std::io::stdout().write_all(&bytes)?;
            }
            Ok(())
        }
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("invalid JSON in {}", path.display()))
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn valid_hash(value: &str) -> bool {
    (value.len() == 16 || value.len() == 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn load_config(root: &Path) -> Result<Config> {
    let config: Config = read_json(&root.join("config.json"))?;
    validate_config(&config)?;
    Ok(config)
}

fn validate_config(config: &Config) -> Result<()> {
    ensure!(config.schema == "sotarena.config", "invalid config schema");
    ensure!(!config.tasks.is_empty(), "config has no tasks");
    ensure!(!config.families.is_empty(), "config has no families");
    ensure!(
        config.elo.initial_rating.is_finite(),
        "invalid initial rating"
    );
    ensure!(
        config.elo.k_factor.is_finite() && config.elo.k_factor > 0.0,
        "invalid K factor"
    );
    ensure!(
        config.elo.tie_tolerance.is_finite() && config.elo.tie_tolerance >= 0.0,
        "invalid tie tolerance"
    );
    ensure!(config.elo.iterations > 0, "invalid Elo iteration count");
    ensure!(
        config.elo.regularization.is_finite() && config.elo.regularization > 0.0,
        "invalid Elo regularization"
    );
    ensure!(
        config.elo.convergence_tolerance.is_finite() && config.elo.convergence_tolerance > 0.0,
        "invalid Elo convergence tolerance"
    );
    let mut task_ids = BTreeSet::new();
    let mut blacklist_paths = BTreeSet::new();
    let mut weights = 0.0;
    for task in &config.tasks {
        ensure!(valid_id(&task.id), "invalid task id {}", task.id);
        ensure!(task_ids.insert(&task.id), "duplicate task {}", task.id);
        ensure!(
            !task.metric.trim().is_empty(),
            "empty metric for {}",
            task.id
        );
        ensure!(
            task.weight.is_finite() && task.weight >= 0.0,
            "invalid weight for {}",
            task.id
        );
        ensure!(
            !task.analysis_blacklist.as_os_str().is_empty()
                && task.analysis_blacklist.is_relative()
                && task
                    .analysis_blacklist
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
                && task.analysis_blacklist.extension().and_then(OsStr::to_str) == Some("json"),
            "invalid analysis blacklist reference for {}",
            task.id
        );
        ensure!(
            blacklist_paths.insert(&task.analysis_blacklist),
            "duplicate analysis blacklist reference {}",
            task.analysis_blacklist.display()
        );
        weights += task.weight;
    }
    ensure!(
        (weights - 1.0).abs() <= 1e-12,
        "task weights must sum to 1.0"
    );

    let mut family_ids = BTreeSet::new();
    let mut patterns = Vec::new();
    for family in &config.families {
        ensure!(valid_id(&family.id), "invalid family id {}", family.id);
        ensure!(
            family_ids.insert(&family.id),
            "duplicate family {}",
            family.id
        );
        ensure!(
            !family.display_name.trim().is_empty(),
            "empty family display name"
        );
        ensure!(
            !family.patterns.is_empty(),
            "family {} has no patterns",
            family.id
        );
        for pattern in &family.patterns {
            validate_pattern(pattern)?;
            patterns.push((family.id.as_str(), pattern.as_str()));
        }
    }
    for left in 0..patterns.len() {
        for right in (left + 1)..patterns.len() {
            ensure!(
                !patterns_overlap(patterns[left].1, patterns[right].1),
                "overlapping patterns {}:{} and {}:{}",
                patterns[left].0,
                patterns[left].1,
                patterns[right].0,
                patterns[right].1
            );
        }
    }
    Ok(())
}

fn validate_pattern(pattern: &str) -> Result<()> {
    let stars = pattern.bytes().filter(|byte| *byte == b'*').count();
    ensure!(
        !pattern.is_empty()
            && (stars == 0 || (stars == 1 && pattern.ends_with('*') && pattern.len() > 1)),
        "invalid method pattern {pattern}"
    );
    Ok(())
}

fn pattern_prefix(pattern: &str) -> Option<&str> {
    pattern.strip_suffix('*')
}

fn patterns_overlap(left: &str, right: &str) -> bool {
    match (pattern_prefix(left), pattern_prefix(right)) {
        (None, None) => left == right,
        (Some(prefix), None) => right.starts_with(prefix),
        (None, Some(prefix)) => left.starts_with(prefix),
        (Some(a), Some(b)) => a.starts_with(b) || b.starts_with(a),
    }
}

fn pattern_matches(pattern: &str, method: &str) -> bool {
    pattern_prefix(pattern).map_or(method == pattern, |prefix| method.starts_with(prefix))
}

fn task_config<'a>(config: &'a Config, task: &str) -> Result<&'a TaskConfig> {
    config
        .tasks
        .iter()
        .find(|candidate| candidate.id == task)
        .with_context(|| format!("unknown task {task}"))
}

fn valid_https_csv_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let Some((authority, path)) = rest.split_once('/') else {
        return false;
    };
    !authority.is_empty()
        && !authority.bytes().any(|byte| byte.is_ascii_whitespace())
        && path.ends_with(".csv")
        && !path.contains(['?', '#'])
}

fn check_data_file(file: &DataFile) -> Result<()> {
    let Some(url) = &file.url else { return Ok(()) };
    ensure!(valid_https_csv_url(url), "invalid CSV URL {url}");
    ensure!(valid_hash(&file.sha256), "invalid data SHA-256");
    ensure!(file.bytes > 0 && file.rows > 0, "invalid data dimensions");
    Ok(())
}

fn load_datasets(root: &Path, config: &Config) -> Result<DatasetCatalog> {
    let mut catalog: DatasetCatalog = config
        .tasks
        .iter()
        .map(|task| (task.id.clone(), BTreeMap::new()))
        .collect();
    for task in &config.tasks {
        let directory = root.join(&task.id);
        let entries = fs::read_dir(&directory)
            .with_context(|| format!("cannot read task directory {}", directory.display()))?;
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() || entry.file_name() == OsStr::new("global") {
                continue;
            }
            let path = entry.path().join("dataset.json");
            let dataset: Dataset = read_json(&path)?;
            ensure!(
                dataset.schema == "sotarena.dataset",
                "invalid dataset schema"
            );
            ensure!(
                dataset.task == task.id && dataset.metric == task.metric,
                "dataset task/metric mismatch"
            );
            ensure!(valid_hash(&dataset.dataset_hash), "invalid dataset hash");
            ensure!(
                entry.file_name() == OsStr::new(&dataset.dataset_hash),
                "dataset directory/hash mismatch"
            );
            check_data_file(&dataset.train)?;
            check_data_file(&dataset.test)?;
            ensure!(
                catalog
                    .get_mut(&task.id)
                    .unwrap()
                    .insert(dataset.dataset_hash.clone(), dataset)
                    .is_none(),
                "duplicate dataset hash"
            );
        }
        ensure!(
            !catalog[&task.id].is_empty(),
            "task {} has no datasets",
            task.id
        );
    }
    Ok(catalog)
}

fn load_analysis_blacklists(
    root: &Path,
    config: &Config,
    datasets: &DatasetCatalog,
) -> Result<AnalysisBlacklists> {
    let mut blacklists = BTreeMap::new();
    for task in &config.tasks {
        let path = root.join(&task.analysis_blacklist);
        let blacklist: AnalysisBlacklist = read_json(&path)?;
        let hashes = validate_analysis_blacklist(task, &datasets[&task.id], blacklist)?;
        blacklists.insert(task.id.clone(), hashes);
    }
    Ok(blacklists)
}

fn validate_analysis_blacklist(
    task: &TaskConfig,
    datasets: &BTreeMap<String, Dataset>,
    blacklist: AnalysisBlacklist,
) -> Result<BTreeSet<String>> {
    ensure!(
        blacklist.schema == "sotarena.analysis-blacklist",
        "invalid analysis blacklist schema for {}",
        task.id
    );
    ensure!(
        blacklist.task == task.id,
        "analysis blacklist task mismatch for {}",
        task.id
    );
    ensure!(
        blacklist.datasets.windows(2).all(|pair| pair[0] < pair[1]),
        "analysis blacklist datasets for {} must be sorted and unique",
        task.id
    );
    for hash in &blacklist.datasets {
        ensure!(
            datasets.contains_key(hash),
            "unknown dataset {hash} in analysis blacklist for {}",
            task.id
        );
    }
    Ok(blacklist.datasets.into_iter().collect())
}

fn dataset_hash<'a>(
    datasets: &'a BTreeMap<String, Dataset>,
    reference: &'a str,
) -> Result<&'a str> {
    if datasets.contains_key(reference) {
        return Ok(reference);
    }
    datasets
        .iter()
        .find(|(_, dataset)| dataset.dataset_id == reference)
        .map(|(hash, _)| hash.as_str())
        .with_context(|| format!("unknown dataset {reference}"))
}

fn result_paths(
    root: &Path,
    task: &str,
    datasets: &BTreeMap<String, Dataset>,
) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for hash in datasets.keys() {
        let directory = root.join(task).join(hash).join("results");
        if !directory.is_dir() {
            continue;
        }
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_file() && path.extension().and_then(OsStr::to_str) == Some("json") {
                paths.push(path);
            }
        }
    }
    paths.sort();
    Ok(paths)
}

fn empty_result_catalog(config: &Config) -> ResultCatalog {
    config
        .tasks
        .iter()
        .map(|task| (task.id.clone(), BTreeMap::new()))
        .collect()
}

fn metric_better(candidate: f64, current: f64, direction: Direction) -> bool {
    match direction {
        Direction::Maximize => candidate > current,
        Direction::Minimize => candidate < current,
    }
}

fn insert_best_score(
    catalog: &mut ResultCatalog,
    task: &TaskConfig,
    method: String,
    dataset: String,
    value: f64,
) {
    let scores = catalog
        .get_mut(&task.id)
        .expect("validated task")
        .entry(method)
        .or_default();
    match scores.get_mut(&dataset) {
        Some(current) if metric_better(value, *current, task.direction) => *current = value,
        Some(_) => {}
        None => {
            scores.insert(dataset, value);
        }
    }
}

fn add_score_document(
    config: &Config,
    datasets: &DatasetCatalog,
    catalog: &mut ResultCatalog,
    document: ScoreDocument,
) -> Result<()> {
    ensure!(document.schema == "sotarena.scores", "invalid score schema");
    let task = task_config(config, &document.task)?;
    ensure!(document.metric == task.metric, "score metric mismatch");
    let task_datasets = &datasets[&task.id];
    let hash = dataset_hash(task_datasets, &document.dataset_hash)?;
    ensure!(hash == document.dataset_hash, "score dataset mismatch");

    for row in document.rows {
        let Some(value) = row.value else { continue };
        if row.status != "ok" {
            continue;
        }
        ensure!(valid_method_id(&row.method_id), "invalid method id");
        ensure!(
            valid_metric_value(&task.metric, value),
            "invalid {} score for {}",
            task.metric,
            row.method_id
        );
        insert_best_score(catalog, task, row.method_id, hash.to_owned(), value);
    }
    Ok(())
}

fn add_result_record(
    config: &Config,
    datasets: &DatasetCatalog,
    catalog: &mut ResultCatalog,
    record: ResultRecord,
) -> Result<()> {
    ensure!(
        record.schema == "sotarena.result" || record.schema == "sotarena.result.v1",
        "invalid result schema"
    );
    let task = task_config(config, &record.task)?;
    ensure!(record.metric == task.metric, "result metric mismatch");
    let task_datasets = &datasets[&task.id];
    let hash = dataset_hash(task_datasets, &record.dataset_hash)?;
    let dataset = &task_datasets[hash];
    ensure!(
        record.dataset_id == dataset.dataset_id,
        "result dataset mismatch"
    );
    ensure!(valid_method_id(&record.method_id), "invalid method id");
    ensure!(!record.display_name.trim().is_empty(), "empty display name");
    ensure!(
        valid_metric_value(&task.metric, record.value),
        "invalid result value"
    );
    ensure!(
        valid_hash(&record.test_sha256) && record.test_sha256 == dataset.test.sha256,
        "test SHA-256 mismatch"
    );
    if let Some(predictions) = &record.predictions_sha256 {
        ensure!(valid_hash(predictions), "invalid prediction SHA-256");
    }
    ensure!(
        !record.run_id.trim().is_empty() && !record.claim_scope.trim().is_empty(),
        "invalid result provenance"
    );
    insert_best_score(
        catalog,
        task,
        record.method_id,
        hash.to_owned(),
        record.value,
    );
    Ok(())
}

fn load_results(root: &Path, config: &Config, datasets: &DatasetCatalog) -> Result<LoadedResults> {
    let mut catalog = empty_result_catalog(config);
    let mut result_jsons: BTreeMap<_, _> = config
        .tasks
        .iter()
        .map(|task| (task.id.clone(), 0))
        .collect();
    let mut paths = Vec::new();
    for task in &config.tasks {
        let task_paths = result_paths(root, &task.id, &datasets[&task.id])?;
        result_jsons.insert(task.id.clone(), task_paths.len());
        paths.extend(task_paths);
    }
    paths.extend(global_result_paths(root, config)?);
    paths.sort();
    paths.dedup();
    let result_jsons_total = paths.len();

    for path in paths {
        let value: Value = read_json(&path)?;
        match value["schema"].as_str() {
            Some("sotarena.scores") => {
                let document: ScoreDocument = serde_json::from_value(value)?;
                add_score_document(config, datasets, &mut catalog, document)?;
            }
            Some("sotarena.result" | "sotarena.result.v1") => {
                let record: ResultRecord = serde_json::from_value(value)?;
                add_result_record(config, datasets, &mut catalog, record)?;
            }
            _ => {}
        }
    }
    Ok(LoadedResults {
        catalog,
        result_jsons,
        result_jsons_total,
    })
}

fn valid_method_id(method: &str) -> bool {
    !method.trim().is_empty() && !method.chars().any(char::is_control)
}

fn valid_metric_value(metric: &str, value: f64) -> bool {
    value.is_finite()
        && match metric {
            "roc_auc" | "macro_f1" => (0.0..=1.0).contains(&value),
            "rmse" | "mase" => value >= 0.0,
            _ => false,
        }
}

fn outcome(left: f64, right: f64, direction: Direction, tolerance: f64) -> f64 {
    let scale = left.abs().max(right.abs()).max(1.0);
    if (left - right).abs() <= tolerance * scale {
        return 0.5;
    }
    match direction {
        Direction::Maximize if left > right => 1.0,
        Direction::Minimize if left < right => 1.0,
        _ => 0.0,
    }
}

fn ratings(direction: Direction, elo: &EloConfig, methods: &TaskResults) -> BTreeMap<String, f64> {
    let method_ids: Vec<_> = methods.keys().map(String::as_str).collect();
    if method_ids.is_empty() {
        return BTreeMap::new();
    }
    let method_index: BTreeMap<_, _> = method_ids
        .iter()
        .enumerate()
        .map(|(index, method)| (*method, index))
        .collect();
    let mut by_dataset: BTreeMap<&str, Vec<(&str, f64)>> = BTreeMap::new();
    for (method, results) in methods {
        for (dataset, value) in results {
            by_dataset
                .entry(dataset)
                .or_default()
                .push((method, *value));
        }
    }

    // Bradley-Terry likelihood depends on each method pair only through its
    // match count and outcome sum. Aggregating those sufficient statistics is
    // exact and avoids materializing millions of repeated pairwise matches.
    let mut pairs: BTreeMap<(usize, usize), (f64, f64)> = BTreeMap::new();
    for scores in by_dataset.values() {
        for left in 0..scores.len() {
            for right in (left + 1)..scores.len() {
                let (a, av) = scores[left];
                let (b, bv) = scores[right];
                let entry = pairs
                    .entry((method_index[a], method_index[b]))
                    .or_insert((0.0, 0.0));
                entry.0 += 1.0;
                entry.1 += outcome(av, bv, direction, elo.tie_tolerance);
            }
        }
    }

    let pair_rows: Vec<_> = pairs
        .into_iter()
        .map(|((left, right), (count, sum))| (left, right, sum / count, count))
        .collect();
    let mut ratings = vec![elo.initial_rating; method_ids.len()];
    let elo_scale = std::f64::consts::LN_10 / 400.0;
    for _ in 0..elo.iterations {
        let mut gradient = vec![0.0; method_ids.len()];
        let mut information = vec![0.0; method_ids.len()];
        for &(left, right, observed, weight) in &pair_rows {
            let delta = (elo_scale * (ratings[left] - ratings[right])).clamp(-700.0, 700.0);
            let probability = 1.0 / (1.0 + (-delta).exp());
            let residual = (observed - probability) * weight;
            let curvature = probability * (1.0 - probability) * weight;
            gradient[left] += residual;
            gradient[right] -= residual;
            information[left] += curvature;
            information[right] += curvature;
        }
        let mut largest_update = 0.0_f64;
        for method in 0..method_ids.len() {
            let regularized_gradient = elo_scale * gradient[method]
                - elo.regularization * (ratings[method] - elo.initial_rating);
            let regularized_information =
                elo_scale * elo_scale * information[method] + elo.regularization;
            let update =
                (regularized_gradient / regularized_information).clamp(-elo.k_factor, elo.k_factor);
            ratings[method] += update;
            largest_update = largest_update.max(update.abs());
        }
        if largest_update < elo.convergence_tolerance {
            break;
        }
    }
    let mean = ratings.iter().sum::<f64>() / ratings.len() as f64;
    let shift = elo.initial_rating - mean;
    method_ids
        .into_iter()
        .zip(ratings)
        .map(|(method, rating)| (method.to_owned(), rating + shift))
        .collect()
}

fn canonical_participants(
    config: &Config,
    task: &TaskConfig,
    results: &TaskResults,
) -> CanonicalParticipants {
    let mut scores: TaskResults = BTreeMap::new();
    let mut display_names = BTreeMap::new();
    for (method, method_scores) in results {
        let family = config.families.iter().find(|family| {
            family
                .patterns
                .iter()
                .any(|pattern| pattern_matches(pattern, method))
        });
        let (key, display_name) = match family {
            Some(family) => (format!("family:{}", family.id), family.display_name.clone()),
            None => (format!("method:{method}"), method.clone()),
        };
        display_names.insert(key.clone(), display_name);
        let participant_scores = scores.entry(key).or_default();
        for (dataset, value) in method_scores {
            match participant_scores.get_mut(dataset) {
                Some(current) if metric_better(*value, *current, task.direction) => {
                    *current = *value;
                }
                Some(_) => {}
                None => {
                    participant_scores.insert(dataset.clone(), *value);
                }
            }
        }
    }
    CanonicalParticipants {
        scores,
        display_names,
    }
}

fn canonical_catalog(config: &Config, results: &ResultCatalog) -> ParticipantCatalog {
    config
        .tasks
        .iter()
        .map(|task| {
            (
                task.id.clone(),
                canonical_participants(config, task, &results[&task.id]),
            )
        })
        .collect()
}

fn global_family_ids(config: &Config, participants: &ParticipantCatalog) -> BTreeSet<String> {
    config
        .families
        .iter()
        .filter(|family| {
            let key = format!("family:{}", family.id);
            config.tasks.iter().all(|task| {
                participants[&task.id]
                    .scores
                    .get(&key)
                    .is_some_and(|scores| !scores.is_empty())
            })
        })
        .map(|family| family.id.clone())
        .collect()
}

fn validate_blacklist_complements(
    config: &Config,
    datasets: &DatasetCatalog,
    participants: &ParticipantCatalog,
    global_families: &BTreeSet<String>,
    blacklists: &AnalysisBlacklists,
) -> Result<()> {
    for task in &config.tasks {
        let task_participants = &participants[&task.id];
        let expected: BTreeSet<_> = datasets[&task.id]
            .keys()
            .filter(|dataset| {
                global_families.iter().any(|family| {
                    !task_participants
                        .scores
                        .get(&format!("family:{family}"))
                        .is_some_and(|scores| scores.contains_key(*dataset))
                })
            })
            .cloned()
            .collect();
        let actual = &blacklists[&task.id];
        if actual != &expected {
            let remove = actual
                .difference(&expected)
                .next()
                .map(|hash| format!("; remove {hash}"))
                .unwrap_or_default();
            let add = expected
                .difference(actual)
                .next()
                .map(|hash| format!("; add {hash}"))
                .unwrap_or_default();
            anyhow::bail!(
                "stale analysis blacklist for {}: {} listed, {} required{remove}{add}",
                task.id,
                actual.len(),
                expected.len()
            );
        }
    }
    Ok(())
}

fn apply_analysis_blacklists(
    participants: &mut ParticipantCatalog,
    blacklists: &AnalysisBlacklists,
) {
    for (task, task_participants) in participants {
        let blacklist = &blacklists[task];
        task_participants.scores.retain(|method, scores| {
            scores.retain(|dataset, _| !blacklist.contains(dataset));
            if scores.is_empty() {
                task_participants.display_names.remove(method);
                false
            } else {
                true
            }
        });
    }
}

fn build_task_table(
    config: &Config,
    task: &TaskConfig,
    datasets: usize,
    blacklisted_datasets: usize,
    result_jsons: usize,
    participants: &CanonicalParticipants,
    global_families: &BTreeSet<String>,
) -> TaskTable {
    let analysis_datasets = datasets - blacklisted_datasets;
    let method_ratings = ratings(task.direction, &config.elo, &participants.scores);
    let mut order: Vec<_> = participants.scores.keys().collect();
    order.retain(|method| participants.scores[*method].len() == analysis_datasets);
    order.sort_by(|left, right| {
        method_ratings[*right]
            .total_cmp(&method_ratings[*left])
            .then_with(|| left.cmp(right))
    });
    let rows = order
        .into_iter()
        .enumerate()
        .filter_map(|(index, method)| {
            let global_family = method
                .strip_prefix("family:")
                .is_some_and(|family| global_families.contains(family));
            (index < 10 || global_family).then(|| TaskRow {
                rank: index + 1,
                method_id: participants.display_names[method].clone(),
                elo: method_ratings[method],
                datasets_scored: participants.scores[method].len(),
            })
        })
        .collect();
    TaskTable {
        task: task.id.clone(),
        metric: task.metric.clone(),
        metric_direction: task.direction.as_str(),
        weight: task.weight,
        datasets,
        analysis_datasets,
        blacklisted_datasets,
        result_jsons,
        score_rows: participants.scores.values().map(BTreeMap::len).sum(),
        methods: participants.scores.len(),
        rows,
    }
}

fn build_global(
    config: &Config,
    participants: &ParticipantCatalog,
    eligible: &BTreeSet<String>,
) -> Vec<GlobalRow> {
    let family_ratings: BTreeMap<_, _> = config
        .tasks
        .iter()
        .map(|task| {
            let scores: TaskResults = eligible
                .iter()
                .map(|family| {
                    (
                        family.clone(),
                        participants[&task.id].scores[&format!("family:{family}")].clone(),
                    )
                })
                .collect();
            (
                task.id.clone(),
                ratings(task.direction, &config.elo, &scores),
            )
        })
        .collect();

    let mut global = Vec::new();
    for family in &config.families {
        if eligible.contains(&family.id) {
            let elo = config
                .tasks
                .iter()
                .map(|task| task.weight * family_ratings[&task.id][&family.id])
                .sum();
            global.push(GlobalRow {
                rank: 0,
                family_id: family.id.clone(),
                display_name: family.display_name.clone(),
                elo,
            });
        }
    }
    global.sort_by(|a, b| {
        b.elo
            .total_cmp(&a.elo)
            .then_with(|| a.family_id.cmp(&b.family_id))
    });
    for (index, row) in global.iter_mut().enumerate() {
        row.rank = index + 1;
    }
    global
}

fn build_report(
    config: &Config,
    datasets: &DatasetCatalog,
    loaded: &LoadedResults,
    blacklists: &AnalysisBlacklists,
) -> Result<Report> {
    let mut participants = canonical_catalog(config, &loaded.catalog);
    let global_families = global_family_ids(config, &participants);
    validate_blacklist_complements(
        config,
        datasets,
        &participants,
        &global_families,
        blacklists,
    )?;
    apply_analysis_blacklists(&mut participants, blacklists);
    let tasks = config
        .tasks
        .iter()
        .map(|task| {
            build_task_table(
                config,
                task,
                datasets[&task.id].len(),
                blacklists[&task.id].len(),
                loaded.result_jsons[&task.id],
                &participants[&task.id],
                &global_families,
            )
        })
        .collect::<Vec<_>>();
    let score_rows = tasks.iter().map(|task| task.score_rows).sum();
    let global = build_global(config, &participants, &global_families);
    Ok(Report {
        schema: "sotarena.report",
        datasets_total: datasets.values().map(BTreeMap::len).sum(),
        result_jsons_total: loaded.result_jsons_total,
        score_rows,
        global,
        tasks,
    })
}

fn render_report(root: &Path) -> Result<(Vec<u8>, Report)> {
    let config = load_config(root)?;
    let datasets = load_datasets(root, &config)?;
    let loaded = load_results(root, &config, &datasets)?;
    let blacklists = load_analysis_blacklists(root, &config, &datasets)?;
    let report = build_report(&config, &datasets, &loaded, &blacklists)?;
    let mut bytes = serde_json::to_vec_pretty(&report)?;
    bytes.push(b'\n');
    Ok((bytes, report))
}

fn global_result_paths(root: &Path, config: &Config) -> Result<Vec<PathBuf>> {
    let mut paths = json_files(&root.join("global/results"))?;
    for task in &config.tasks {
        paths.extend(json_files(&root.join(&task.id).join("global/results"))?);
    }
    paths.sort();
    Ok(paths)
}

fn json_files(directory: &Path) -> Result<Vec<PathBuf>> {
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    Ok(fs::read_dir(directory)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_file() && path.extension().and_then(OsStr::to_str) == Some("json"))
        .collect())
}

const LEADERBOARD_DIRECTORY: &str = "assets/leaderboards";
const HEADER_HEIGHT: u32 = 40;
const ROW_HEIGHT: u32 = 34;
const RANK_WIDTH: u32 = 64;
const ELO_WIDTH: u32 = 96;
const RELATIVE_ELO_WIDTH: u32 = 240;
const CELL_PADDING: u32 = 16;
const BAR_LEFT_DARKENING: f64 = 0.2;

fn title_label(id: &str) -> String {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn leaderboards(report: &Report) -> Vec<Leaderboard<'_>> {
    let mut tables = Vec::with_capacity(report.tasks.len() + 1);
    tables.push(Leaderboard {
        id: "global",
        title: "Global Elo".to_owned(),
        label_header: "Family",
        rows: report
            .global
            .iter()
            .map(|row| LeaderboardRow {
                rank: row.rank,
                label: &row.display_name,
                elo: row.elo,
            })
            .collect(),
    });
    tables.extend(report.tasks.iter().map(|task| {
        Leaderboard {
            id: &task.task,
            title: format!("{} Elo", title_label(&task.task)),
            label_header: "Method",
            rows: task
                .rows
                .iter()
                .map(|row| LeaderboardRow {
                    rank: row.rank,
                    label: &row.method_id,
                    elo: row.elo,
                })
                .collect(),
        }
    }));
    tables
}

fn normalized_performance(elo: f64, min_elo: f64, max_elo: f64) -> f64 {
    if min_elo == max_elo {
        0.5
    } else {
        ((elo - min_elo) / (max_elo - min_elo)).clamp(0.0, 1.0)
    }
}

fn relative_elo_fraction(elo: f64, min_elo: f64, max_elo: f64) -> f64 {
    if min_elo == max_elo {
        1.0
    } else {
        let x_min = min_elo - 0.05 * min_elo.abs().max(1.0);
        ((elo - x_min) / (max_elo - x_min)).clamp(0.0, 1.0)
    }
}

fn interpolate_rgb(left: (u8, u8, u8), right: (u8, u8, u8), amount: f64) -> (u8, u8, u8) {
    let channel = |start: u8, end: u8| {
        (f64::from(start) + (f64::from(end) - f64::from(start)) * amount).round() as u8
    };
    (
        channel(left.0, right.0),
        channel(left.1, right.1),
        channel(left.2, right.2),
    )
}

fn performance_rgb(theme: Theme, performance: f64) -> (u8, u8, u8) {
    let palette = theme.palette();
    let performance = performance.clamp(0.0, 1.0);
    if performance <= 0.5 {
        interpolate_rgb(palette.red, palette.amber, performance * 2.0)
    } else {
        interpolate_rgb(palette.amber, palette.green, (performance - 0.5) * 2.0)
    }
}

fn rgb_color(color: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", color.0, color.1, color.2)
}

fn performance_color(theme: Theme, performance: f64) -> String {
    rgb_color(performance_rgb(theme, performance))
}

fn darkened_performance_color(theme: Theme, performance: f64) -> String {
    rgb_color(interpolate_rgb(
        performance_rgb(theme, performance),
        (0, 0, 0),
        BAR_LEFT_DARKENING,
    ))
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '\'' => escaped.push_str("&apos;"),
            '"' => escaped.push_str("&quot;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn leaderboard_dimensions(table: &Leaderboard<'_>) -> (u32, u32, u32) {
    let longest_label = table
        .rows
        .iter()
        .map(|row| row.label.chars().count())
        .chain(std::iter::once(table.label_header.chars().count()))
        .max()
        .unwrap_or(0) as u32;
    let label_width = (longest_label * 8 + 2 * CELL_PADDING).max(160);
    let width = RANK_WIDTH + label_width + ELO_WIDTH + RELATIVE_ELO_WIDTH;
    let height = HEADER_HEIGHT + ROW_HEIGHT * table.rows.len() as u32;
    (width, height, label_width)
}

fn render_leaderboard_svg(table: &Leaderboard<'_>, theme: Theme) -> String {
    let palette = theme.palette();
    let (width, height, label_width) = leaderboard_dimensions(table);
    let relative_elo_left = width - RELATIVE_ELO_WIDTH;
    let elo_right = RANK_WIDTH + label_width + ELO_WIDTH - CELL_PADDING;
    let min_elo = table
        .rows
        .iter()
        .map(|row| row.elo)
        .min_by(f64::total_cmp)
        .unwrap_or(0.0);
    let max_elo = table
        .rows
        .iter()
        .map(|row| row.elo)
        .max_by(f64::total_cmp)
        .unwrap_or(0.0);
    let mut svg = String::new();
    writeln!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\" role=\"img\" aria-labelledby=\"title\">"
    )
    .unwrap();
    writeln!(
        svg,
        "  <title id=\"title\">{} leaderboard</title>",
        xml_escape(&table.title)
    )
    .unwrap();
    writeln!(svg, "  <defs>").unwrap();
    writeln!(
        svg,
        "    <clipPath id=\"frame-clip\"><rect width=\"{width}\" height=\"{height}\" rx=\"6\"/></clipPath>"
    )
    .unwrap();
    for (index, row) in table.rows.iter().enumerate() {
        let performance = normalized_performance(row.elo, min_elo, max_elo);
        let darkened_color = darkened_performance_color(theme, performance);
        let color = performance_color(theme, performance);
        writeln!(
            svg,
            "    <linearGradient id=\"elo-gradient-{index}\" x1=\"0%\" y1=\"0%\" x2=\"100%\" y2=\"0%\">"
        )
        .unwrap();
        writeln!(
            svg,
            "      <stop offset=\"0%\" stop-color=\"{darkened_color}\"/>"
        )
        .unwrap();
        writeln!(svg, "      <stop offset=\"100%\" stop-color=\"{color}\"/>").unwrap();
        writeln!(svg, "    </linearGradient>").unwrap();
    }
    writeln!(svg, "  </defs>").unwrap();
    writeln!(
        svg,
        "  <rect width=\"{width}\" height=\"{height}\" rx=\"6\" fill=\"{}\"/>",
        palette.background
    )
    .unwrap();
    writeln!(svg, "  <g clip-path=\"url(#frame-clip)\">").unwrap();
    for (index, row) in table.rows.iter().enumerate() {
        let row_top = HEADER_HEIGHT + ROW_HEIGHT * index as u32;
        let bar_width =
            relative_elo_fraction(row.elo, min_elo, max_elo) * f64::from(RELATIVE_ELO_WIDTH);
        writeln!(
            svg,
            "    <rect class=\"elo-bar\" x=\"{relative_elo_left}\" y=\"{row_top}\" width=\"{bar_width:.3}\" height=\"{ROW_HEIGHT}\" fill=\"url(#elo-gradient-{index})\"/>"
        )
        .unwrap();
    }
    writeln!(svg, "  </g>").unwrap();
    writeln!(
        svg,
        "  <path d=\"M 1 6 Q 1 1 6 1 H {} Q {} 1 {} 6 V {} H 1 Z\" fill=\"{}\"/>",
        width - 6,
        width - 1,
        width - 1,
        HEADER_HEIGHT,
        palette.header_background
    )
    .unwrap();
    writeln!(
        svg,
        "  <g font-family=\"-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif\" font-size=\"14\">"
    )
    .unwrap();
    writeln!(
        svg,
        "    <g fill=\"{}\" font-weight=\"600\">",
        palette.neutral
    )
    .unwrap();
    writeln!(
        svg,
        "      <text x=\"{}\" y=\"25\" text-anchor=\"end\">Rank</text>",
        RANK_WIDTH - CELL_PADDING
    )
    .unwrap();
    writeln!(
        svg,
        "      <text x=\"{}\" y=\"25\">{}</text>",
        RANK_WIDTH + CELL_PADDING,
        xml_escape(table.label_header)
    )
    .unwrap();
    writeln!(
        svg,
        "      <text x=\"{elo_right}\" y=\"25\" text-anchor=\"end\">Elo</text>"
    )
    .unwrap();
    writeln!(
        svg,
        "      <text x=\"{}\" y=\"25\">Relative Elo (95% min)</text>",
        relative_elo_left + CELL_PADDING
    )
    .unwrap();
    writeln!(svg, "    </g>").unwrap();
    for (index, row) in table.rows.iter().enumerate() {
        let row_top = HEADER_HEIGHT + ROW_HEIGHT * index as u32;
        let baseline = row_top + 22;
        let color = performance_color(theme, normalized_performance(row.elo, min_elo, max_elo));
        writeln!(
            svg,
            "    <text x=\"{}\" y=\"{baseline}\" text-anchor=\"end\" fill=\"{}\">{}</text>",
            RANK_WIDTH - CELL_PADDING,
            palette.neutral,
            row.rank
        )
        .unwrap();
        writeln!(
            svg,
            "    <text x=\"{}\" y=\"{baseline}\" fill=\"{color}\" font-weight=\"700\">{}</text>",
            RANK_WIDTH + CELL_PADDING,
            xml_escape(row.label)
        )
        .unwrap();
        writeln!(
            svg,
            "    <text x=\"{elo_right}\" y=\"{baseline}\" text-anchor=\"end\" fill=\"{color}\" font-weight=\"700\">{:.1}</text>",
            row.elo
        )
        .unwrap();
    }
    writeln!(svg, "  </g>").unwrap();
    writeln!(
        svg,
        "  <path d=\"M 1 {HEADER_HEIGHT} H {}\" stroke=\"{}\"/>",
        width - 1,
        palette.border
    )
    .unwrap();
    for index in 1..table.rows.len() {
        let row_top = HEADER_HEIGHT + ROW_HEIGHT * index as u32;
        writeln!(
            svg,
            "  <path d=\"M 1 {row_top} H {}\" stroke=\"{}\"/>",
            width - 1,
            palette.border
        )
        .unwrap();
    }
    writeln!(
        svg,
        "  <rect width=\"{width}\" height=\"{height}\" rx=\"6\" fill=\"none\" stroke=\"{}\"/>",
        palette.border
    )
    .unwrap();
    writeln!(svg, "</svg>").unwrap();
    svg
}

fn leaderboard_filename(id: &str, theme: Theme) -> String {
    format!("{id}-{}.svg", theme.name())
}

fn render_leaderboard_assets(report: &Report) -> BTreeMap<String, String> {
    let mut assets = BTreeMap::new();
    for table in leaderboards(report) {
        for theme in [Theme::Light, Theme::Dark] {
            assets.insert(
                leaderboard_filename(table.id, theme),
                render_leaderboard_svg(&table, theme),
            );
        }
    }
    assets
}

fn write_leaderboard_assets(root: &Path, report: &Report) -> Result<()> {
    let directory = root.join(LEADERBOARD_DIRECTORY);
    fs::create_dir_all(&directory)?;
    let assets = render_leaderboard_assets(report);
    let expected: BTreeSet<_> = assets.keys().cloned().collect();
    for (filename, svg) in assets {
        fs::write(directory.join(filename), svg)?;
    }
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let path = entry.path();
        let filename = entry.file_name().to_string_lossy().into_owned();
        if path.is_file()
            && path.extension().and_then(OsStr::to_str) == Some("svg")
            && !expected.contains(&filename)
        {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn render_picture(markdown: &mut String, table: &Leaderboard<'_>) {
    let light = leaderboard_filename(table.id, Theme::Light);
    let dark = leaderboard_filename(table.id, Theme::Dark);
    let alt = xml_escape(&format!("{} leaderboard", table.title));
    writeln!(markdown, "<picture>").unwrap();
    writeln!(
        markdown,
        "  <source media=\"(prefers-color-scheme: dark)\" srcset=\"{LEADERBOARD_DIRECTORY}/{dark}\">"
    )
    .unwrap();
    writeln!(
        markdown,
        "  <source media=\"(prefers-color-scheme: light)\" srcset=\"{LEADERBOARD_DIRECTORY}/{light}\">"
    )
    .unwrap();
    writeln!(
        markdown,
        "  <img alt=\"{alt}\" src=\"{LEADERBOARD_DIRECTORY}/{light}\">"
    )
    .unwrap();
    writeln!(markdown, "</picture>\n").unwrap();
}

fn render_markdown_fallback(markdown: &mut String, table: &Leaderboard<'_>) {
    writeln!(markdown, "<details>").unwrap();
    writeln!(markdown, "<summary>Markdown fallback</summary>\n").unwrap();
    writeln!(
        markdown,
        "| Rank | {} | Elo |\n| ---: | --- | ---: |",
        table.label_header
    )
    .unwrap();
    for row in &table.rows {
        writeln!(
            markdown,
            "| {} | **{}** | **{:.1}** |",
            row.rank,
            markdown_cell(row.label),
            row.elo
        )
        .unwrap();
    }
    writeln!(markdown, "\n</details>\n").unwrap();
}

fn render_readme(report: &Report) -> Vec<u8> {
    let mut markdown = String::from(concat!(
        "# SOTArena\n\n",
        "SOTArena is a JSON-backed benchmark with deterministic Elo rankings. ",
        "Every manifest and result is loaded and validated. Elo uses each task's analysis cohort: ",
        "the datasets with a valid canonical score from every automatically Global-eligible family. ",
        "The referenced blacklists record the auditable complement without removing corpus data.\n\n",
        "## Getting Started\n\n",
        "Clone and build the reporter:\n\n",
        "```sh\n",
        "git clone https://github.com/neverhuman/sotarena.git\n",
        "cd sotarena\n",
        "cargo build --release\n",
        "```\n\n",
        "Download one dataset, a whole task, or every downloadable dataset:\n\n",
        "```sh\n",
        "cargo run --release -- fetch --task binary --dataset ad2b3ffae29d73f6\n",
        "cargo run --release -- fetch --task binary\n",
        "cargo run --release -- fetch --all\n",
        "```\n\n",
        "Downloads default to `$HOME/.cache/sotarena/benchmark`. To use another directory ",
        "outside the repository, pass `--cache-dir /path/to/cache` or set ",
        "`SOTARENA_BENCHMARK_CACHE`.\n\n",
        "Generate the JSON report, leaderboard SVGs, and refresh this README:\n\n",
        "```sh\n",
        "cargo run --release -- report --out report.json\n",
        "```\n\n"
    ));
    let tables = leaderboards(report);
    for (index, table) in tables.iter().enumerate() {
        writeln!(markdown, "## {}\n", table.title).unwrap();
        if index == 0 {
            writeln!(
                markdown,
                "Datasets: {}. Download all: `cargo run --release -- fetch --all`.\n",
                report.datasets_total
            )
            .unwrap();
        } else if let Some(task) = report.tasks.get(index - 1) {
            writeln!(
                markdown,
                "Datasets: {}. Download all: `cargo run --release -- fetch --task {}`.\n",
                task.datasets, task.task
            )
            .unwrap();
        }
        render_picture(&mut markdown, table);
        render_markdown_fallback(&mut markdown, table);
    }
    markdown.pop();
    markdown.into_bytes()
}

fn markdown_cell(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '|' => escaped.push_str("\\|"),
            '\n' | '\r' => escaped.push(' '),
            '*' | '_' => {
                escaped.push('\\');
                escaped.push(character);
            }
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '&' => escaped.push_str("&amp;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn lexical_absolute(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(value) => normalized.push(value),
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
        }
    }
    Ok(normalized)
}

fn external_cache(root: &Path, requested: Option<PathBuf>) -> Result<PathBuf> {
    let path = requested
        .or_else(|| env::var_os("SOTARENA_BENCHMARK_CACHE").map(PathBuf::from))
        .or_else(|| {
            env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/sotarena/benchmark"))
        })
        .context("provide --cache-dir or SOTARENA_BENCHMARK_CACHE")?;
    let repository = fs::canonicalize(root)?;
    let normalized = lexical_absolute(&path)?;
    ensure!(
        !normalized.starts_with(&repository),
        "cache directory must be outside the repository"
    );
    Ok(normalized)
}

fn verify_bytes(bytes: &[u8], expected: &DataFile) -> Result<()> {
    ensure!(bytes.len() as u64 == expected.bytes, "byte count mismatch");
    ensure!(
        format!("{:x}", Sha256::digest(bytes)) == expected.sha256,
        "SHA-256 mismatch"
    );
    Ok(())
}

fn fetch_targets<'a>(
    config: &Config,
    datasets: &'a DatasetCatalog,
    task_id: Option<&str>,
    wanted: Option<&str>,
) -> Result<Vec<(&'a str, &'a Dataset)>> {
    ensure!(
        wanted.is_none() || task_id.is_some(),
        "--dataset requires --task"
    );
    if let Some(task_id) = task_id {
        task_config(config, task_id)?;
        let (task_id, task_datasets) = datasets
            .get_key_value(task_id)
            .expect("validated task catalog");
        if let Some(wanted) = wanted {
            let hash = dataset_hash(task_datasets, wanted)?;
            let dataset = &task_datasets[hash];
            for (split, source) in [("train", &dataset.train), ("test", &dataset.test)] {
                ensure!(
                    source.url.is_some(),
                    "dataset {} has no downloadable {split} URL",
                    dataset.dataset_hash
                );
            }
            return Ok(vec![(task_id, dataset)]);
        }
        return Ok(task_datasets
            .values()
            .filter(|dataset| dataset.train.url.is_some() && dataset.test.url.is_some())
            .map(|dataset| (task_id.as_str(), dataset))
            .collect());
    }

    Ok(datasets
        .iter()
        .flat_map(|(task_id, task_datasets)| {
            task_datasets
                .values()
                .filter(|dataset| dataset.train.url.is_some() && dataset.test.url.is_some())
                .map(move |dataset| (task_id.as_str(), dataset))
        })
        .collect())
}

fn fetch(
    root: &Path,
    task_id: Option<&str>,
    wanted: Option<&str>,
    requested_cache: Option<PathBuf>,
) -> Result<()> {
    let config = load_config(root)?;
    let datasets = load_datasets(root, &config)?;
    let selected = fetch_targets(&config, &datasets, task_id, wanted)?;
    let cache = external_cache(root, requested_cache)?;
    for (task_id, dataset) in selected {
        for (split, source) in [("train", &dataset.train), ("test", &dataset.test)] {
            let url = source.url.as_ref().expect("selected downloadable dataset");
            let destination = cache
                .join(task_id)
                .join(&dataset.dataset_hash)
                .join(format!("{split}.csv"));
            let bytes = if destination.exists() {
                fs::read(&destination)?
            } else {
                let mut bytes = Vec::new();
                ureq::get(url)
                    .call()
                    .with_context(|| format!("download failed: {url}"))?
                    .into_reader()
                    .read_to_end(&mut bytes)?;
                verify_bytes(&bytes, source)?;
                fs::create_dir_all(destination.parent().unwrap())?;
                fs::write(&destination, &bytes)?;
                bytes
            };
            verify_bytes(&bytes, source)?;
            println!("verified {} {split}", dataset.dataset_hash);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(direction: Direction) -> Config {
        Config {
            schema: "sotarena.config".to_owned(),
            tasks: vec![TaskConfig {
                id: "binary".to_owned(),
                metric: "roc_auc".to_owned(),
                direction,
                weight: 1.0,
                analysis_blacklist: "binary/analysis-blacklist.json".into(),
            }],
            elo: EloConfig {
                initial_rating: 1500.0,
                k_factor: 40.0,
                tie_tolerance: 1e-12,
                iterations: 250,
                regularization: 1e-4,
                convergence_tolerance: 1e-8,
            },
            families: vec![FamilyConfig {
                id: "family".to_owned(),
                display_name: "Family".to_owned(),
                patterns: vec!["family-*".to_owned()],
            }],
        }
    }

    fn aspfm_config(direction: Direction) -> Config {
        let mut config = test_config(direction);
        config.families[0] = FamilyConfig {
            id: "aspfm".to_owned(),
            display_name: "ASPFM".to_owned(),
            patterns: vec!["jope-*".to_owned(), "aspfm*".to_owned()],
        };
        config
    }

    fn test_datasets(ids: &[&str]) -> DatasetCatalog {
        let files = || DataFile {
            url: None,
            sha256: "a".repeat(64),
            bytes: 1,
            rows: 1,
        };
        let datasets = ids
            .iter()
            .map(|id| {
                (
                    (*id).to_owned(),
                    Dataset {
                        schema: "sotarena.dataset".to_owned(),
                        dataset_hash: (*id).to_owned(),
                        dataset_id: (*id).to_owned(),
                        task: "binary".to_owned(),
                        metric: "roc_auc".to_owned(),
                        train: files(),
                        test: files(),
                    },
                )
            })
            .collect();
        BTreeMap::from([("binary".to_owned(), datasets)])
    }

    fn score_document(dataset: &str, metric: &str, rows: Vec<ScoreDocumentRow>) -> ScoreDocument {
        ScoreDocument {
            schema: "sotarena.scores".to_owned(),
            task: "binary".to_owned(),
            dataset_hash: dataset.to_owned(),
            metric: metric.to_owned(),
            rows,
        }
    }

    fn score_row(method: &str, value: Option<f64>, status: &str) -> ScoreDocumentRow {
        ScoreDocumentRow {
            method_id: method.to_owned(),
            value,
            status: status.to_owned(),
        }
    }

    fn test_task_table(config: &Config, datasets: usize, results: &TaskResults) -> TaskTable {
        let task = &config.tasks[0];
        let participants = canonical_participants(config, task, results);
        build_task_table(
            config,
            task,
            datasets,
            0,
            0,
            &participants,
            &BTreeSet::new(),
        )
    }

    fn rendering_report() -> Report {
        Report {
            schema: "sotarena.report",
            datasets_total: 9,
            result_jsons_total: 3,
            score_rows: 27,
            global: vec![
                GlobalRow {
                    rank: 1,
                    family_id: "high".to_owned(),
                    display_name: "High".to_owned(),
                    elo: 2000.0,
                },
                GlobalRow {
                    rank: 2,
                    family_id: "middle".to_owned(),
                    display_name: "Middle".to_owned(),
                    elo: 1500.0,
                },
                GlobalRow {
                    rank: 3,
                    family_id: "low".to_owned(),
                    display_name: "Low".to_owned(),
                    elo: 1000.0,
                },
            ],
            tasks: vec![TaskTable {
                task: "binary".to_owned(),
                metric: "roc_auc".to_owned(),
                metric_direction: "maximize",
                weight: 1.0,
                datasets: 9,
                analysis_datasets: 9,
                blacklisted_datasets: 0,
                result_jsons: 3,
                score_rows: 27,
                methods: 3,
                rows: vec![
                    TaskRow {
                        rank: 1,
                        method_id: "A<&\"'".to_owned(),
                        elo: 2000.0,
                        datasets_scored: 9,
                    },
                    TaskRow {
                        rank: 2,
                        method_id: "Middle".to_owned(),
                        elo: 1500.0,
                        datasets_scored: 9,
                    },
                    TaskRow {
                        rank: 3,
                        method_id: "Low".to_owned(),
                        elo: 1000.0,
                        datasets_scored: 9,
                    },
                ],
            }],
        }
    }

    #[test]
    fn leaderboard_normalization_and_palette_stops_are_exact() {
        assert_eq!(normalized_performance(1000.0, 1000.0, 2000.0), 0.0);
        assert_eq!(normalized_performance(1500.0, 1000.0, 2000.0), 0.5);
        assert_eq!(normalized_performance(2000.0, 1000.0, 2000.0), 1.0);
        assert_eq!(normalized_performance(1500.0, 1500.0, 1500.0), 0.5);

        assert_eq!(performance_color(Theme::Light, 0.0), "#cf222e");
        assert_eq!(performance_color(Theme::Light, 0.5), "#9a6700");
        assert_eq!(performance_color(Theme::Light, 1.0), "#1a7f37");
        assert_eq!(performance_color(Theme::Dark, 0.0), "#ff7b72");
        assert_eq!(performance_color(Theme::Dark, 0.5), "#d29922");
        assert_eq!(performance_color(Theme::Dark, 1.0), "#3fb950");
        assert_eq!(darkened_performance_color(Theme::Light, 0.0), "#a61b25");
        assert_eq!(darkened_performance_color(Theme::Light, 0.5), "#7b5200");
        assert_eq!(darkened_performance_color(Theme::Light, 1.0), "#15662c");
        assert_eq!(darkened_performance_color(Theme::Dark, 1.0), "#329440");
        assert_eq!(interpolate_rgb((0, 10, 20), (10, 30, 40), 0.5), (5, 20, 30));
    }

    #[test]
    fn relative_elo_bar_fractions_cover_positive_zero_negative_and_equal_ranges() {
        let close = |actual: f64, expected: f64| {
            assert!((actual - expected).abs() < 1e-12, "{actual} != {expected}");
        };

        close(relative_elo_fraction(1000.0, 1000.0, 2000.0), 50.0 / 1050.0);
        close(
            relative_elo_fraction(1500.0, 1000.0, 2000.0),
            550.0 / 1050.0,
        );
        assert_eq!(relative_elo_fraction(2000.0, 1000.0, 2000.0), 1.0);
        assert_eq!(relative_elo_fraction(1500.0, 1500.0, 1500.0), 1.0);
        close(relative_elo_fraction(0.0, 0.0, 100.0), 0.05 / 100.05);
        close(relative_elo_fraction(-100.0, -100.0, 100.0), 5.0 / 205.0);
        assert_eq!(relative_elo_fraction(-200.0, -100.0, 100.0), 0.0);
        assert_eq!(relative_elo_fraction(200.0, -100.0, 100.0), 1.0);
    }

    #[test]
    fn changing_elo_changes_the_generated_color() {
        let mut report = rendering_report();
        let before = render_leaderboard_assets(&report)["binary-light.svg"].clone();
        assert!(before.contains("fill=\"#9a6700\" font-weight=\"700\">Middle</text>"));

        report.tasks[0].rows[1].elo = 1750.0;
        let after = render_leaderboard_assets(&report)["binary-light.svg"].clone();
        let changed = performance_color(Theme::Light, 0.75);
        let changed_darkened = darkened_performance_color(Theme::Light, 0.75);
        assert_ne!(changed, "#9a6700");
        assert!(after.contains(&format!(
            "fill=\"{changed}\" font-weight=\"700\">Middle</text>"
        )));
        assert!(after.contains(&format!(
            "<stop offset=\"0%\" stop-color=\"{changed_darkened}\"/>"
        )));
        assert!(after.contains(&format!("<stop offset=\"100%\" stop-color=\"{changed}\"/>")));
        assert_ne!(before, after);
    }

    #[test]
    fn svg_assets_are_algorithmic_escaped_bold_and_theme_aware() {
        let report = rendering_report();
        let assets = render_leaderboard_assets(&report);
        assert_eq!(assets.len(), 4);
        assert_eq!(
            assets.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "binary-dark.svg",
                "binary-light.svg",
                "global-dark.svg",
                "global-light.svg"
            ]
        );

        let global = &assets["global-light.svg"];
        let binary = &assets["binary-light.svg"];
        let binary_dark = &assets["binary-dark.svg"];
        assert!(!global.contains(">Coverage</text>"));
        assert!(!binary.contains(">Coverage</text>"));
        assert!(global.contains(">Relative Elo (95% min)</text>"));
        assert!(binary.contains(">Relative Elo (95% min)</text>"));
        assert!(binary.contains("A&lt;&amp;&quot;&apos;</text>"));
        assert!(binary.contains("fill=\"#1a7f37\" font-weight=\"700\">A"));
        assert!(binary.contains("font-weight=\"700\">2000.0</text>"));
        assert!(!binary.contains(">9/9</text>"));
        assert!(binary.contains("fill=\"#ffffff\""));
        assert!(binary_dark.contains("fill=\"#0d1117\""));
        assert!(binary_dark.contains("fill=\"#3fb950\" font-weight=\"700\">A"));
        assert_eq!(
            binary.matches("<linearGradient id=\"elo-gradient-").count(),
            3
        );
        assert!(binary.contains(
            "<linearGradient id=\"elo-gradient-0\" x1=\"0%\" y1=\"0%\" x2=\"100%\" y2=\"0%\">"
        ));
        assert!(binary.contains("<stop offset=\"0%\" stop-color=\"#15662c\"/>"));
        assert!(binary.contains("<stop offset=\"100%\" stop-color=\"#1a7f37\"/>"));

        let tables = leaderboards(&report);
        let (global_width, global_height, global_label_width) = leaderboard_dimensions(&tables[0]);
        let (binary_width, binary_height, binary_label_width) = leaderboard_dimensions(&tables[1]);
        assert_eq!(global_height, HEADER_HEIGHT + 3 * ROW_HEIGHT);
        assert_eq!(binary_height, HEADER_HEIGHT + 3 * ROW_HEIGHT);
        assert_eq!(
            global_width,
            RANK_WIDTH + global_label_width + ELO_WIDTH + RELATIVE_ELO_WIDTH
        );
        assert_eq!(
            binary_width,
            RANK_WIDTH + binary_label_width + ELO_WIDTH + RELATIVE_ELO_WIDTH
        );

        let table_boundary = binary_width - RELATIVE_ELO_WIDTH;
        let bar_origin = format!("<rect class=\"elo-bar\" x=\"{table_boundary}\"");
        assert_eq!(binary.matches("class=\"elo-bar\"").count(), 3);
        assert_eq!(binary.matches(&bar_origin).count(), 3);
        assert_eq!(
            binary
                .matches(&format!("height=\"{ROW_HEIGHT}\" fill="))
                .count(),
            3
        );
        assert!(binary.contains(&format!(
            "x=\"{table_boundary}\" y=\"{HEADER_HEIGHT}\" width=\"{RELATIVE_ELO_WIDTH}.000\" height=\"{ROW_HEIGHT}\" fill=\"url(#elo-gradient-0)\""
        )));
        assert!(
            binary.rfind("fill=\"none\" stroke=").unwrap()
                > binary.rfind("class=\"elo-bar\"").unwrap()
        );

        let mut longer = rendering_report();
        longer.tasks[0].rows[0].method_id = "a method name long enough to expand the column".into();
        let longer_tables = leaderboards(&longer);
        let (longer_width, longer_height, longer_label_width) =
            leaderboard_dimensions(&longer_tables[1]);
        assert!(longer_width > binary_width);
        assert_eq!(longer_height, binary_height);
        assert_eq!(
            longer_width - binary_width,
            longer_label_width - binary_label_width
        );
        let longer_boundary = longer_width - RELATIVE_ELO_WIDTH;
        assert!(longer_boundary > table_boundary);
        let longer_svg = render_leaderboard_svg(&longer_tables[1], Theme::Light);
        assert_eq!(
            longer_svg
                .matches(&format!("<rect class=\"elo-bar\" x=\"{longer_boundary}\""))
                .count(),
            3
        );
        assert!(!longer_svg.contains(">Coverage</text>"));
    }

    #[test]
    fn an_additional_task_automatically_gets_assets_and_readme_content() {
        let mut report = rendering_report();
        let task = TaskTable {
            task: "surprise".to_owned(),
            metric: "metric".to_owned(),
            metric_direction: "maximize",
            weight: 0.0,
            datasets: 2,
            analysis_datasets: 2,
            blacklisted_datasets: 0,
            result_jsons: 1,
            score_rows: 2,
            methods: 1,
            rows: vec![TaskRow {
                rank: 1,
                method_id: "New | method".to_owned(),
                elo: 1777.7,
                datasets_scored: 2,
            }],
        };
        report.tasks.push(task);

        let assets = render_leaderboard_assets(&report);
        assert_eq!(assets.len(), 6);
        assert!(assets.contains_key("surprise-light.svg"));
        assert!(assets.contains_key("surprise-dark.svg"));
        let readme = String::from_utf8(render_readme(&report)).unwrap();
        assert!(readme.contains("## Surprise Elo"));
        assert!(readme.contains("assets/leaderboards/surprise-light.svg"));
        assert!(readme.contains("assets/leaderboards/surprise-dark.svg"));
        assert!(readme.contains(
            "Datasets: 2. Download all: `cargo run --release -- fetch --task surprise`."
        ));
        assert!(readme.contains("**New \\| method**"));
        assert!(readme.contains("**1777.7**"));
    }

    #[test]
    fn direction_and_tolerance_are_deterministic() {
        assert_eq!(outcome(2.0, 1.0, Direction::Maximize, 0.0), 1.0);
        assert_eq!(outcome(2.0, 1.0, Direction::Minimize, 0.0), 0.0);
        assert_eq!(outcome(1.001, 1.0, Direction::Maximize, 0.01), 0.5);
    }

    #[test]
    fn patterns_are_exact_or_one_trailing_prefix_wildcard() {
        assert!(pattern_matches("tabfm*", "tabfm_ensemble_regressor"));
        assert!(pattern_matches("exact", "exact"));
        assert!(!pattern_matches("exact", "exactly"));
        assert!(patterns_overlap("tab*", "tabfm*"));
        assert!(!patterns_overlap("tabfm*", "timesfm*"));
    }

    #[test]
    fn configured_families_match_and_merge_historical_variants() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let config = load_config(root).unwrap();
        let cases = [
            ("jope-dime--attempt-a", "aspfm"),
            ("tabicl--attempt-a", "tabicl"),
            ("mc20_tabicl_v2", "tabicl"),
            ("tabfm_regressor--attempt-a", "tabfm"),
            ("tabfm_ensemble_regressor--attempt-a", "tabfm-ensemble"),
            ("mc20_tabfm_ensemble", "tabfm-ensemble"),
            ("tabpfn_v3_classifier--attempt-a", "tabpfn-v3"),
            ("mc20_tabpfn_v3", "tabpfn-v3"),
            ("tabpfn_ts_3", "tabpfn-v3"),
            ("fable5_regressor--attempt-a", "fable-5"),
            ("grok4p5_regressor--attempt-a", "grok-4-5"),
            ("gpt5p6sol_regressor--attempt-a", "openai-sol"),
            ("kimik3_regressor--attempt-a", "kimi-k3"),
        ];
        for (method, expected) in cases {
            let matched = config
                .families
                .iter()
                .find(|family| {
                    family
                        .patterns
                        .iter()
                        .any(|pattern| pattern_matches(pattern, method))
                })
                .map(|family| family.id.as_str());
            assert_eq!(matched, Some(expected), "unexpected family for {method}");
        }

        let variants = [
            ("jope-dime", "aspfm-binary", "aspfm"),
            ("tabicl--attempt-a", "mc20_tabicl_v2", "tabicl"),
            (
                "tabfm_ensemble_regressor--attempt-a",
                "mc20_tabfm_ensemble",
                "tabfm-ensemble",
            ),
            ("tabpfn_v3_classifier", "tabpfn_ts_3", "tabpfn-v3"),
            ("fable5_regressor", "fable5_regressor--attempt-a", "fable-5"),
            (
                "grok4p5_regressor",
                "grok4p5_regressor--attempt-a",
                "grok-4-5",
            ),
            (
                "gpt5p6sol_regressor",
                "gpt5p6sol_regressor--attempt-a",
                "openai-sol",
            ),
            ("kimik3_regressor", "kimik3_regressor--attempt-a", "kimi-k3"),
        ];
        let results: TaskResults = variants
            .iter()
            .flat_map(|(first, second, _)| {
                [
                    ((*first).to_owned(), BTreeMap::from([("a".to_owned(), 0.6)])),
                    (
                        (*second).to_owned(),
                        BTreeMap::from([("a".to_owned(), 0.9)]),
                    ),
                ]
            })
            .collect();
        let participants = canonical_participants(&config, &config.tasks[0], &results);
        for (_, _, family) in variants {
            assert_eq!(participants.scores[&format!("family:{family}")]["a"], 0.9);
        }
        assert!(!participants.scores.contains_key("family:tabfm"));
    }

    #[test]
    fn hashes_and_urls_fail_closed() {
        assert!(valid_hash("0123456789abcdef"));
        assert!(valid_hash(&"a".repeat(64)));
        assert!(!valid_hash("ABCDEF0123456789"));
        assert!(valid_https_csv_url("https://example.test/data.csv"));
        assert!(!valid_https_csv_url("http://example.test/data.csv"));
        assert!(!valid_https_csv_url("https://example.test/data.json"));
    }

    #[test]
    fn analysis_blacklists_validate_schema_task_order_and_known_hashes() {
        let config = test_config(Direction::Maximize);
        let task = &config.tasks[0];
        let datasets = &test_datasets(&["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb"])["binary"];
        let blacklist = |schema: &str, task: &str, hashes: &[&str]| AnalysisBlacklist {
            schema: schema.to_owned(),
            task: task.to_owned(),
            datasets: hashes.iter().map(|hash| (*hash).to_owned()).collect(),
        };

        let valid = validate_analysis_blacklist(
            task,
            datasets,
            blacklist(
                "sotarena.analysis-blacklist",
                "binary",
                &["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb"],
            ),
        )
        .unwrap();
        assert_eq!(valid.len(), 2);

        for (candidate, message) in [
            (
                blacklist("wrong", "binary", &[]),
                "invalid analysis blacklist schema",
            ),
            (
                blacklist("sotarena.analysis-blacklist", "regression", &[]),
                "analysis blacklist task mismatch",
            ),
            (
                blacklist(
                    "sotarena.analysis-blacklist",
                    "binary",
                    &["bbbbbbbbbbbbbbbb", "aaaaaaaaaaaaaaaa"],
                ),
                "must be sorted and unique",
            ),
            (
                blacklist(
                    "sotarena.analysis-blacklist",
                    "binary",
                    &["aaaaaaaaaaaaaaaa", "aaaaaaaaaaaaaaaa"],
                ),
                "must be sorted and unique",
            ),
            (
                blacklist(
                    "sotarena.analysis-blacklist",
                    "binary",
                    &["cccccccccccccccc"],
                ),
                "unknown dataset",
            ),
        ] {
            let error = validate_analysis_blacklist(task, datasets, candidate)
                .err()
                .unwrap();
            assert!(error.to_string().contains(message), "{error:#}");
        }
    }

    #[test]
    fn analysis_blacklist_must_equal_the_complete_family_complement() {
        let config = test_config(Direction::Maximize);
        let datasets = test_datasets(&["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb"]);
        let results = BTreeMap::from([(
            "binary".to_owned(),
            BTreeMap::from([(
                "family-a".to_owned(),
                BTreeMap::from([("aaaaaaaaaaaaaaaa".to_owned(), 0.9)]),
            )]),
        )]);
        let participants = canonical_catalog(&config, &results);
        let eligible = global_family_ids(&config, &participants);
        let exact = BTreeMap::from([(
            "binary".to_owned(),
            BTreeSet::from(["bbbbbbbbbbbbbbbb".to_owned()]),
        )]);
        validate_blacklist_complements(&config, &datasets, &participants, &eligible, &exact)
            .unwrap();

        for mismatch in [
            BTreeSet::new(),
            BTreeSet::from(["aaaaaaaaaaaaaaaa".to_owned(), "bbbbbbbbbbbbbbbb".to_owned()]),
        ] {
            let blacklists = BTreeMap::from([("binary".to_owned(), mismatch)]);
            let error = validate_blacklist_complements(
                &config,
                &datasets,
                &participants,
                &eligible,
                &blacklists,
            )
            .err()
            .unwrap();
            assert!(error.to_string().contains("stale analysis blacklist"));
        }
    }

    #[test]
    fn blacklist_filtering_removes_scores_before_task_and_global_elo() {
        let mut config = test_config(Direction::Maximize);
        config.families.extend([
            FamilyConfig {
                id: "other".to_owned(),
                display_name: "Other".to_owned(),
                patterns: vec!["other-*".to_owned()],
            },
            FamilyConfig {
                id: "blocker".to_owned(),
                display_name: "Blocker".to_owned(),
                patterns: vec!["blocker-*".to_owned()],
            },
        ]);
        let active = "aaaaaaaaaaaaaaaa";
        let blacklisted = "bbbbbbbbbbbbbbbb";
        let task_results = BTreeMap::from([
            (
                "family-a".to_owned(),
                BTreeMap::from([(active.to_owned(), 0.8), (blacklisted.to_owned(), 0.0)]),
            ),
            (
                "other-a".to_owned(),
                BTreeMap::from([(active.to_owned(), 0.7), (blacklisted.to_owned(), 1.0)]),
            ),
            (
                "blocker-a".to_owned(),
                BTreeMap::from([(active.to_owned(), 0.6)]),
            ),
        ]);
        let results = BTreeMap::from([("binary".to_owned(), task_results)]);
        let datasets = test_datasets(&[active, blacklisted]);
        let mut participants = canonical_catalog(&config, &results);
        let eligible = global_family_ids(&config, &participants);
        let blacklists = BTreeMap::from([(
            "binary".to_owned(),
            BTreeSet::from([blacklisted.to_owned()]),
        )]);
        validate_blacklist_complements(&config, &datasets, &participants, &eligible, &blacklists)
            .unwrap();
        assert_eq!(
            participants["binary"]
                .scores
                .values()
                .map(BTreeMap::len)
                .sum::<usize>(),
            5
        );

        apply_analysis_blacklists(&mut participants, &blacklists);
        assert!(participants["binary"]
            .scores
            .values()
            .all(|scores| !scores.contains_key(blacklisted)));
        let table = build_task_table(
            &config,
            &config.tasks[0],
            2,
            1,
            0,
            &participants["binary"],
            &eligible,
        );
        let global = build_global(&config, &participants, &eligible);

        let baseline_results = BTreeMap::from([(
            "binary".to_owned(),
            results["binary"]
                .iter()
                .map(|(method, scores)| {
                    (
                        method.clone(),
                        scores
                            .iter()
                            .filter(|(dataset, _)| dataset.as_str() == active)
                            .map(|(dataset, value)| (dataset.clone(), *value))
                            .collect(),
                    )
                })
                .collect(),
        )]);
        let baseline_participants = canonical_catalog(&config, &baseline_results);
        let baseline_table = build_task_table(
            &config,
            &config.tasks[0],
            1,
            0,
            0,
            &baseline_participants["binary"],
            &eligible,
        );
        let baseline_global = build_global(&config, &baseline_participants, &eligible);

        assert_eq!(table.score_rows, 3);
        assert_eq!(table.methods, 3);
        assert_eq!(table.rows.len(), 3);
        for (row, baseline) in table.rows.iter().zip(&baseline_table.rows) {
            assert_eq!(row.rank, baseline.rank);
            assert_eq!(row.method_id, baseline.method_id);
            assert_eq!(row.datasets_scored, 1);
            assert_eq!(row.elo, baseline.elo);
        }
        for (row, baseline) in global.iter().zip(&baseline_global) {
            assert_eq!(row.rank, baseline.rank);
            assert_eq!(row.family_id, baseline.family_id);
            assert_eq!(row.elo, baseline.elo);
        }
    }

    #[test]
    fn corpus_dataset_counts_are_exact() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let config = load_config(root).unwrap();
        let datasets = load_datasets(root, &config).unwrap();
        assert_eq!(datasets["binary"].len(), 799);
        assert_eq!(datasets["regression"].len(), 1_564);
        assert_eq!(datasets["multiclass"].len(), 272);
        assert_eq!(datasets["timeseries"].len(), 42);
        assert_eq!(datasets.values().map(BTreeMap::len).sum::<usize>(), 2_677);
    }

    #[test]
    fn duplicate_scores_keep_the_metric_best_value_including_aspfm() {
        let config = test_config(Direction::Maximize);
        let datasets = test_datasets(&["aaaaaaaaaaaaaaaa"]);
        let mut catalog = empty_result_catalog(&config);
        add_score_document(
            &config,
            &datasets,
            &mut catalog,
            score_document(
                "aaaaaaaaaaaaaaaa",
                "roc_auc",
                vec![
                    score_row("aspfm-variant", Some(0.6), "ok"),
                    score_row("aspfm-variant", Some(0.9), "ok"),
                    score_row("aspfm-variant", Some(0.7), "ok"),
                ],
            ),
        )
        .unwrap();
        assert_eq!(catalog["binary"]["aspfm-variant"]["aaaaaaaaaaaaaaaa"], 0.9);

        let minimize = test_config(Direction::Minimize);
        let mut catalog = empty_result_catalog(&minimize);
        add_score_document(
            &minimize,
            &datasets,
            &mut catalog,
            score_document(
                "aaaaaaaaaaaaaaaa",
                "roc_auc",
                vec![
                    score_row("aspfm-variant", Some(0.6), "ok"),
                    score_row("aspfm-variant", Some(0.2), "ok"),
                    score_row("aspfm-variant", Some(0.4), "ok"),
                ],
            ),
        )
        .unwrap();
        assert_eq!(catalog["binary"]["aspfm-variant"]["aaaaaaaaaaaaaaaa"], 0.2);
    }

    #[test]
    fn dataset_count_is_unique_and_only_valid_scores_are_loaded() {
        let config = test_config(Direction::Maximize);
        let datasets = test_datasets(&["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb", "aaaaaaaaaaaaaaaa"]);
        assert_eq!(datasets["binary"].len(), 2);

        let mut catalog = empty_result_catalog(&config);
        add_score_document(
            &config,
            &datasets,
            &mut catalog,
            score_document(
                "aaaaaaaaaaaaaaaa",
                "roc_auc",
                vec![
                    score_row("null", None, "ok"),
                    score_row("ignored\nmethod", Some(f64::NAN), "ineligible"),
                    score_row("loaded", Some(0.9), "ok"),
                ],
            ),
        )
        .unwrap();
        assert_eq!(catalog["binary"].len(), 1);
        assert_eq!(catalog["binary"]["loaded"].len(), 1);
    }

    #[test]
    fn partial_methods_participate_but_are_excluded_from_published_rows() {
        let config = test_config(Direction::Maximize);
        let task = &config.tasks[0];
        let results = BTreeMap::from([
            (
                "complete".to_owned(),
                BTreeMap::from([("a".to_owned(), 0.5), ("b".to_owned(), 0.5)]),
            ),
            (
                "partial".to_owned(),
                BTreeMap::from([("a".to_owned(), 1.0)]),
            ),
        ]);
        let all_ratings = ratings(task.direction, &config.elo, &results);
        assert_ne!(all_ratings["partial"], config.elo.initial_rating);

        let table = test_task_table(&config, 2, &results);
        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.rows[0].method_id, "complete");
    }

    #[test]
    fn task_table_keeps_top_ten_and_appends_global_families_at_true_rank() {
        let config = test_config(Direction::Maximize);
        let mut results = TaskResults::new();
        for index in 0..11 {
            results.insert(
                format!("leader-{index:02}"),
                BTreeMap::from([("dataset".to_owned(), 1.0 - index as f64 / 20.0)]),
            );
        }
        results.insert(
            "family-low".to_owned(),
            BTreeMap::from([("dataset".to_owned(), 0.0)]),
        );
        let participants = canonical_participants(&config, &config.tasks[0], &results);
        let table = build_task_table(
            &config,
            &config.tasks[0],
            1,
            0,
            0,
            &participants,
            &BTreeSet::from(["family".to_owned()]),
        );

        assert_eq!(table.rows.len(), 11);
        assert_eq!(
            table.rows[..10]
                .iter()
                .map(|row| row.rank)
                .collect::<Vec<_>>(),
            (1..=10).collect::<Vec<_>>()
        );
        assert_eq!(table.rows[10].method_id, "Family");
        assert_eq!(table.rows[10].rank, 12);
        assert_eq!(table.rows[10].datasets_scored, 1);
    }

    #[test]
    fn canonical_participants_merge_aspfm_variants_in_both_directions() {
        let results = BTreeMap::from([
            (
                "jope-dime".to_owned(),
                BTreeMap::from([("a".to_owned(), 0.6), ("b".to_owned(), 0.8)]),
            ),
            (
                "aspfm-binary".to_owned(),
                BTreeMap::from([("a".to_owned(), 0.9), ("b".to_owned(), 0.7)]),
            ),
            (
                "baseline".to_owned(),
                BTreeMap::from([("a".to_owned(), 0.5), ("b".to_owned(), 0.5)]),
            ),
        ]);

        let maximize = aspfm_config(Direction::Maximize);
        let participants = canonical_participants(&maximize, &maximize.tasks[0], &results);
        assert_eq!(participants.scores.len(), 2);
        assert_eq!(participants.scores["family:aspfm"]["a"], 0.9);
        assert_eq!(participants.scores["family:aspfm"]["b"], 0.8);
        assert_eq!(participants.display_names["family:aspfm"], "ASPFM");
        assert_eq!(participants.display_names["method:baseline"], "baseline");
        assert!(!participants.scores.contains_key("method:jope-dime"));
        assert!(!participants.scores.contains_key("method:aspfm-binary"));

        let minimize = aspfm_config(Direction::Minimize);
        let participants = canonical_participants(&minimize, &minimize.tasks[0], &results);
        assert_eq!(participants.scores["family:aspfm"]["a"], 0.6);
        assert_eq!(participants.scores["family:aspfm"]["b"], 0.7);
    }

    #[test]
    fn task_elo_uses_each_family_once_and_hides_partial_families() {
        let config = aspfm_config(Direction::Maximize);
        let task = &config.tasks[0];
        let complete = BTreeMap::from([
            (
                "jope-a".to_owned(),
                BTreeMap::from([("a".to_owned(), 0.9), ("b".to_owned(), 0.7)]),
            ),
            (
                "aspfm-b".to_owned(),
                BTreeMap::from([("b".to_owned(), 0.9)]),
            ),
            (
                "baseline".to_owned(),
                BTreeMap::from([("a".to_owned(), 0.5), ("b".to_owned(), 0.5)]),
            ),
        ]);
        let table = test_task_table(&config, 2, &complete);
        assert_eq!(table.methods, 2);
        assert_eq!(table.score_rows, 4);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].method_id, "ASPFM");
        assert_eq!(table.rows[1].method_id, "baseline");

        let partial = BTreeMap::from([
            ("jope-a".to_owned(), BTreeMap::from([("a".to_owned(), 0.9)])),
            (
                "baseline".to_owned(),
                BTreeMap::from([("a".to_owned(), 0.5), ("b".to_owned(), 0.5)]),
            ),
        ]);
        let participants = canonical_participants(&config, task, &partial);
        let participant_ratings = ratings(task.direction, &config.elo, &participants.scores);
        assert_ne!(
            participant_ratings["family:aspfm"],
            config.elo.initial_rating
        );
        let table = test_task_table(&config, 2, &partial);
        assert_eq!(table.methods, 2);
        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.rows[0].method_id, "baseline");
    }

    #[test]
    fn elo_direction_and_method_ties_are_deterministic() {
        let results = BTreeMap::from([
            ("a-high".to_owned(), BTreeMap::from([("a".to_owned(), 0.9)])),
            ("b-low".to_owned(), BTreeMap::from([("a".to_owned(), 0.1)])),
        ]);
        let maximize = test_config(Direction::Maximize);
        let maximize_ratings = ratings(Direction::Maximize, &maximize.elo, &results);
        let minimize_ratings = ratings(Direction::Minimize, &maximize.elo, &results);
        assert!(maximize_ratings["a-high"] > maximize_ratings["b-low"]);
        assert!(minimize_ratings["a-high"] < minimize_ratings["b-low"]);

        let tied = BTreeMap::from([
            ("z".to_owned(), BTreeMap::from([("a".to_owned(), 0.5)])),
            ("a".to_owned(), BTreeMap::from([("a".to_owned(), 0.5)])),
        ]);
        let table = test_task_table(&maximize, 1, &tied);
        assert_eq!(table.rows[0].method_id, "a");
        assert_eq!(table.rows[1].method_id, "z");
    }

    #[test]
    fn solver_matches_the_archived_bradley_terry_fixture() {
        let config = test_config(Direction::Minimize);
        let results = BTreeMap::from([
            (
                "best".to_owned(),
                BTreeMap::from([
                    ("d1".to_owned(), 0.1),
                    ("d2".to_owned(), 0.2),
                    ("d3".to_owned(), 0.3),
                ]),
            ),
            (
                "mid".to_owned(),
                BTreeMap::from([
                    ("d1".to_owned(), 0.5),
                    ("d2".to_owned(), 0.6),
                    ("d3".to_owned(), 0.7),
                ]),
            ),
            (
                "worst".to_owned(),
                BTreeMap::from([
                    ("d1".to_owned(), 0.9),
                    ("d2".to_owned(), 1.0),
                    ("d3".to_owned(), 1.1),
                ]),
            ),
        ]);
        let result = ratings(Direction::Minimize, &config.elo, &results);
        assert!((result["best"] - 1602.296273771328).abs() < 1e-9);
        assert!((result["mid"] - 1500.0).abs() < 1e-9);
        assert!((result["worst"] - 1397.703726228672).abs() < 1e-9);
    }

    #[test]
    fn global_family_discovery_requires_every_task() {
        let mut config = test_config(Direction::Maximize);
        config.tasks = vec![
            TaskConfig {
                id: "binary".to_owned(),
                metric: "roc_auc".to_owned(),
                direction: Direction::Maximize,
                weight: 0.5,
                analysis_blacklist: "binary/analysis-blacklist.json".into(),
            },
            TaskConfig {
                id: "regression".to_owned(),
                metric: "rmse".to_owned(),
                direction: Direction::Minimize,
                weight: 0.5,
                analysis_blacklist: "regression/analysis-blacklist.json".into(),
            },
        ];
        config.families.push(FamilyConfig {
            id: "other".to_owned(),
            display_name: "Other".to_owned(),
            patterns: vec!["other-*".to_owned()],
        });
        config.families.push(FamilyConfig {
            id: "partial".to_owned(),
            display_name: "Partial".to_owned(),
            patterns: vec!["partial-*".to_owned()],
        });
        let results = BTreeMap::from([
            (
                "binary".to_owned(),
                BTreeMap::from([
                    (
                        "family-a".to_owned(),
                        BTreeMap::from([("a".to_owned(), 0.6)]),
                    ),
                    (
                        "family-b".to_owned(),
                        BTreeMap::from([("a".to_owned(), 0.9)]),
                    ),
                    (
                        "other-a".to_owned(),
                        BTreeMap::from([("a".to_owned(), 0.5), ("b".to_owned(), 0.5)]),
                    ),
                    (
                        "partial-a".to_owned(),
                        BTreeMap::from([("a".to_owned(), 1.0)]),
                    ),
                ]),
            ),
            (
                "regression".to_owned(),
                BTreeMap::from([
                    (
                        "family-a".to_owned(),
                        BTreeMap::from([("c".to_owned(), 0.4)]),
                    ),
                    (
                        "other-a".to_owned(),
                        BTreeMap::from([("c".to_owned(), 0.5), ("d".to_owned(), 0.5)]),
                    ),
                ]),
            ),
        ]);

        let participants = canonical_participants(&config, &config.tasks[0], &results["binary"]);
        assert_eq!(participants.scores["family:family"]["a"], 0.9);
        assert_eq!(participants.scores["family:family"].len(), 1);

        let participants = canonical_catalog(&config, &results);
        let eligible = global_family_ids(&config, &participants);
        let global = build_global(&config, &participants, &eligible);
        assert_eq!(global.len(), 2);
        assert!(global.iter().any(|row| row.family_id == "family"));
        assert!(global.iter().all(|row| row.family_id != "partial"));

        let mut without_ineligible = results.clone();
        without_ineligible
            .get_mut("binary")
            .unwrap()
            .remove("partial-a");
        let baseline_participants = canonical_catalog(&config, &without_ineligible);
        let baseline_eligible = global_family_ids(&config, &baseline_participants);
        let baseline = build_global(&config, &baseline_participants, &baseline_eligible);
        for row in global {
            let expected = baseline
                .iter()
                .find(|candidate| candidate.family_id == row.family_id)
                .unwrap();
            assert_eq!(row.elo, expected.elo);
        }
    }

    #[test]
    fn release_report_publishes_the_eight_canonical_global_families() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let config = load_config(root).unwrap();
        let datasets = load_datasets(root, &config).unwrap();
        let loaded = load_results(root, &config, &datasets).unwrap();
        let blacklists = load_analysis_blacklists(root, &config, &datasets).unwrap();
        let participants = canonical_catalog(&config, &loaded.catalog);
        let eligible = global_family_ids(&config, &participants);
        let expected_roster = BTreeSet::from([
            ("aspfm", "ASPFM"),
            ("fable-5", "Fable 5"),
            ("grok-4-5", "Grok 4.5"),
            ("kimi-k3", "Kimi K3"),
            ("openai-sol", "OpenAI *sol"),
            ("tabfm-ensemble", "TabFM Ensemble"),
            ("tabicl", "TabICL"),
            ("tabpfn-v3", "TabPFN v3"),
        ]);
        assert_eq!(
            eligible.iter().map(String::as_str).collect::<BTreeSet<_>>(),
            expected_roster.iter().map(|(family, _)| *family).collect()
        );
        validate_blacklist_complements(&config, &datasets, &participants, &eligible, &blacklists)
            .unwrap();

        let expected_counts = BTreeMap::from([
            ("binary", (799, 799, 0)),
            ("regression", (1_564, 1_564, 0)),
            ("multiclass", (272, 272, 0)),
            ("timeseries", (42, 42, 0)),
        ]);
        for task in &config.tasks {
            let (corpus, active, blacklisted) = expected_counts[task.id.as_str()];
            assert_eq!(datasets[&task.id].len(), corpus);
            assert_eq!(blacklists[&task.id].len(), blacklisted);
            let active_hashes: BTreeSet<_> = datasets[&task.id]
                .keys()
                .filter(|hash| !blacklists[&task.id].contains(*hash))
                .collect();
            assert_eq!(active_hashes.len(), active);
            for family in &eligible {
                let scores = &participants[&task.id].scores[&format!("family:{family}")];
                assert!(active_hashes.iter().all(|hash| scores.contains_key(*hash)));
            }
        }

        let report = build_report(&config, &datasets, &loaded, &blacklists).unwrap();
        for task in &report.tasks {
            let (corpus, active, blacklisted) = expected_counts[task.task.as_str()];
            assert_eq!(
                (
                    task.datasets,
                    task.analysis_datasets,
                    task.blacklisted_datasets
                ),
                (corpus, active, blacklisted)
            );
            assert_eq!(
                task.rows
                    .iter()
                    .filter(|row| row.rank <= 10)
                    .map(|row| row.rank)
                    .collect::<Vec<_>>(),
                (1..=10).collect::<Vec<_>>()
            );
            assert!(task.rows.iter().all(|row| row.datasets_scored == active));
            let displayed: BTreeSet<_> =
                task.rows.iter().map(|row| row.method_id.as_str()).collect();
            for display_name in expected_roster
                .iter()
                .map(|(_, display_name)| *display_name)
            {
                assert!(
                    displayed.contains(display_name),
                    "{display_name} missing from {} table",
                    task.task
                );
            }
        }
        let roster: BTreeSet<_> = report
            .global
            .iter()
            .map(|row| (row.family_id.as_str(), row.display_name.as_str()))
            .collect();
        assert_eq!(roster, expected_roster);

        assert_eq!(render_readme(&report), render_readme(&report));
        assert_eq!(
            serde_json::to_vec_pretty(&report).unwrap(),
            serde_json::to_vec_pretty(&report).unwrap()
        );
    }

    #[test]
    fn fetch_cli_accepts_individual_task_and_all_scopes() {
        for arguments in [
            vec!["sotarena", "fetch", "--task", "binary", "--dataset", "data"],
            vec!["sotarena", "fetch", "--task", "binary"],
            vec!["sotarena", "fetch", "--all"],
        ] {
            assert!(Cli::try_parse_from(arguments).is_ok());
        }
        for arguments in [
            vec!["sotarena", "fetch"],
            vec!["sotarena", "fetch", "--dataset", "data"],
            vec!["sotarena", "fetch", "--all", "--task", "binary"],
            vec!["sotarena", "fetch", "--all", "--dataset", "data"],
        ] {
            assert!(
                Cli::try_parse_from(&arguments).is_err(),
                "accepted invalid arguments: {arguments:?}"
            );
        }
    }

    #[test]
    fn bulk_fetch_skips_metadata_only_datasets_but_explicit_fetch_errors() {
        let config = test_config(Direction::Maximize);
        let mut datasets = test_datasets(&["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb"]);
        let downloadable = datasets
            .get_mut("binary")
            .unwrap()
            .get_mut("aaaaaaaaaaaaaaaa")
            .unwrap();
        downloadable.train.url = Some("https://example.test/train.csv".to_owned());
        downloadable.test.url = Some("https://example.test/test.csv".to_owned());

        let task = fetch_targets(&config, &datasets, Some("binary"), None).unwrap();
        assert_eq!(task.len(), 1);
        assert_eq!(task[0].1.dataset_hash, "aaaaaaaaaaaaaaaa");
        let all = fetch_targets(&config, &datasets, None, None).unwrap();
        assert_eq!(all.len(), 1);

        let error = fetch_targets(&config, &datasets, Some("binary"), Some("bbbbbbbbbbbbbbbb"))
            .err()
            .unwrap();
        assert!(error
            .to_string()
            .contains("dataset bbbbbbbbbbbbbbbb has no downloadable train URL"));
    }

    #[test]
    fn readme_has_exactly_five_ranking_tables() {
        let tasks = ["binary", "regression", "multiclass", "timeseries"]
            .into_iter()
            .map(|task| TaskTable {
                task: task.to_owned(),
                metric: "metric".to_owned(),
                metric_direction: "maximize",
                weight: 0.25,
                datasets: 1,
                analysis_datasets: 1,
                blacklisted_datasets: 0,
                result_jsons: 1,
                score_rows: 1,
                methods: 1,
                rows: vec![TaskRow {
                    rank: 1,
                    method_id: "method".to_owned(),
                    elo: 1500.0,
                    datasets_scored: 1,
                }],
            })
            .collect();
        let report = Report {
            schema: "sotarena.report",
            datasets_total: 4,
            result_jsons_total: 4,
            score_rows: 4,
            global: vec![GlobalRow {
                rank: 1,
                family_id: "family".to_owned(),
                display_name: "Family".to_owned(),
                elo: 1500.0,
            }],
            tasks,
        };
        let readme = String::from_utf8(render_readme(&report)).unwrap();
        for command in [
            "git clone https://github.com/neverhuman/sotarena.git",
            "cd sotarena",
            "cargo build --release",
            "cargo run --release -- fetch --task binary --dataset ad2b3ffae29d73f6",
            "cargo run --release -- fetch --task binary",
            "cargo run --release -- fetch --all",
            "cargo run --release -- report --out report.json",
        ] {
            assert!(readme.contains(command));
        }
        assert!(readme.contains("SOTARENA_BENCHMARK_CACHE"));
        assert_eq!(
            readme
                .lines()
                .filter(|line| line.starts_with("| Rank |"))
                .count(),
            5
        );
        assert_eq!(
            readme
                .lines()
                .filter(|line| line.starts_with("Datasets:"))
                .count(),
            5
        );
        let lowercase = readme.to_ascii_lowercase();
        assert!(!lowercase.contains("track"));
        assert!(lowercase.contains("analysis cohort"));
        assert!(readme.contains("Datasets: 4. Download all: `cargo run --release -- fetch --all`."));
        for task in ["binary", "regression", "multiclass", "timeseries"] {
            assert!(readme.contains(&format!(
                "Datasets: 1. Download all: `cargo run --release -- fetch --task {task}`."
            )));
        }
        assert!(!readme.contains("Coverage"));
        assert!(readme.contains("| Rank | Method | Elo |"));
        let json = String::from_utf8(serde_json::to_vec(&report).unwrap()).unwrap();
        assert!(!json.contains("track"));
        assert!(!json.contains("subgroup"));
    }
}
