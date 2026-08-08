use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

const FIRST_TAG: &str = "v8.0.0";
const LAST_TAG: &str = "v9.5.0";
const RECENCY_DAYS: [i64; 3] = [30, 90, 180];

type AppResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct LabelRow {
    commit: String,
    release_tag: Option<String>,
    label: &'static str,
    session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct Metrics {
    n_beliefs: usize,
    tp: usize,
    fp: usize,
    tn: usize,
    fn_: usize,
    precision_stale: f64,
    recall_stale: f64,
    f1_stale: f64,
}

fn derive_label_rows(
    commits: &[String],
    shipped: &BTreeMap<String, String>,
    reverted: &BTreeSet<String>,
    sessions: &BTreeMap<String, String>,
) -> Vec<LabelRow> {
    let mut seen = BTreeSet::new();
    commits
        .iter()
        .filter(|commit| seen.insert((*commit).clone()))
        .map(|commit| {
            let release_tag = shipped.get(commit).cloned();
            let label = if reverted.contains(commit) {
                "reverted"
            } else if release_tag.is_some() {
                "shipped"
            } else {
                "unreleased"
            };
            let session_id = sessions.get(commit).cloned();
            LabelRow {
                commit: commit.clone(),
                release_tag,
                label,
                session_id,
            }
        })
        .collect()
}

fn score_stale_predictions(predicted: &[bool], actual: &[bool]) -> Result<Metrics, String> {
    if predicted.len() != actual.len() {
        return Err("prediction and ground-truth belief sets differ".into());
    }
    let (mut tp, mut fp, mut tn, mut fn_) = (0, 0, 0, 0);
    for (&prediction, &truth) in predicted.iter().zip(actual) {
        match (prediction, truth) {
            (true, true) => tp += 1,
            (true, false) => fp += 1,
            (false, false) => tn += 1,
            (false, true) => fn_ += 1,
        }
    }
    let precision = ratio(tp, tp + fp);
    let recall = ratio(tp, tp + fn_);
    let f1 = if precision + recall == 0.0 {
        0.0
    } else {
        2.0 * precision * recall / (precision + recall)
    };
    Ok(Metrics {
        n_beliefs: actual.len(),
        tp,
        fp,
        tn,
        fn_,
        precision_stale: precision,
        recall_stale: recall,
        f1_stale: f1,
    })
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn baseline_grep(symbol: &str, final_content: Option<&str>) -> bool {
    let unsuffixed = strip_collision_suffix(symbol);
    let source_name = match (unsuffixed.rfind("::"), unsuffixed.rfind('.')) {
        (Some(colons), Some(dot)) if colons > dot => &unsuffixed[colons + 2..],
        (Some(_), Some(dot)) => &unsuffixed[dot + 1..],
        (Some(colons), None) => &unsuffixed[colons + 2..],
        (None, Some(dot)) => &unsuffixed[dot + 1..],
        (None, None) => unsuffixed,
    };
    final_content.is_none_or(|content| !content.contains(source_name))
}

fn strip_collision_suffix(symbol: &str) -> &str {
    let Some(hash) = symbol.rfind('#') else {
        return symbol;
    };
    let suffix = &symbol[hash + 1..];
    if !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        &symbol[..hash]
    } else {
        symbol
    }
}

#[derive(Debug)]
struct LabelsOptions {
    repo: PathBuf,
    csr_db: Option<PathBuf>,
    out: Option<PathBuf>,
}

#[derive(Debug)]
struct BenchOptions {
    repo: PathBuf,
    binary: PathBuf,
    scratch_dir: Option<PathBuf>,
    tags_count: usize,
    first_tag: String,
    last_tag: String,
    out: Option<PathBuf>,
    keep_db: bool,
    skip_baselines: bool,
}

fn main() {
    if let Err(error) = run_main() {
        eprintln!("codewitness: {error}");
        std::process::exit(2);
    }
}

fn run_main() -> AppResult<()> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        print_help();
        return Ok(());
    };
    let remaining: Vec<String> = args.collect();
    match command.as_str() {
        "labels" => run_labels(parse_labels(&remaining)?)?,
        "bench" => run_bench(parse_bench(&remaining)?)?,
        "help" | "--help" | "-h" => print_help(),
        other => {
            return Err(format!("unknown subcommand {other:?}; expected labels or bench").into())
        }
    }
    Ok(())
}

fn print_help() {
    println!(
        "codewitness <labels|bench> [OPTIONS]\n\n\
         labels: --repo PATH [--csr-db PATH] [--out FILE]\n\
         bench:  --repo PATH --binary PATH [--scratch-dir PATH] [--tags-count N]\n\
                 [--first-tag TAG] [--last-tag TAG] [--out FILE] [--keep-db]\n\
                 [--skip-baselines]"
    );
}

fn parse_labels(args: &[String]) -> AppResult<LabelsOptions> {
    let mut repo = PathBuf::from(".");
    let mut csr_db = None;
    let mut out = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--repo" => repo = PathBuf::from(option_value(args, &mut index, "--repo")?),
            "--csr-db" => {
                csr_db = Some(PathBuf::from(option_value(args, &mut index, "--csr-db")?));
            }
            "--out" => out = Some(PathBuf::from(option_value(args, &mut index, "--out")?)),
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown labels option {other:?}").into()),
        }
        index += 1;
    }
    Ok(LabelsOptions { repo, csr_db, out })
}

fn parse_bench(args: &[String]) -> AppResult<BenchOptions> {
    let mut repo = PathBuf::from(".");
    let mut binary = PathBuf::from("csr-engine");
    let mut scratch_dir = None;
    let mut tags_count = 13;
    let mut first_tag = FIRST_TAG.to_string();
    let mut last_tag = LAST_TAG.to_string();
    let mut out = None;
    let mut keep_db = false;
    let mut skip_baselines = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--repo" => repo = PathBuf::from(option_value(args, &mut index, "--repo")?),
            "--binary" => binary = PathBuf::from(option_value(args, &mut index, "--binary")?),
            "--scratch-dir" => {
                scratch_dir = Some(PathBuf::from(option_value(
                    args,
                    &mut index,
                    "--scratch-dir",
                )?));
            }
            "--tags-count" => {
                tags_count = option_value(args, &mut index, "--tags-count")?.parse()?;
            }
            "--first-tag" => first_tag = option_value(args, &mut index, "--first-tag")?.into(),
            "--last-tag" => last_tag = option_value(args, &mut index, "--last-tag")?.into(),
            "--out" => out = Some(PathBuf::from(option_value(args, &mut index, "--out")?)),
            "--keep-db" => keep_db = true,
            "--skip-baselines" => skip_baselines = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown bench option {other:?}").into()),
        }
        index += 1;
    }
    if tags_count < 2 {
        return Err("--tags-count must be >= 2 (need both endpoints)".into());
    }
    Ok(BenchOptions {
        repo,
        binary,
        scratch_dir,
        tags_count,
        first_tag,
        last_tag,
        out,
        keep_db,
        skip_baselines,
    })
}

fn option_value<'a>(args: &'a [String], index: &mut usize, option: &str) -> AppResult<&'a str> {
    *index += 1;
    args.get(*index)
        .map(String::as_str)
        .ok_or_else(|| format!("{option} requires a value").into())
}

fn run_labels(options: LabelsOptions) -> AppResult<()> {
    let repo = absolute(&options.repo)?;
    let tags = list_all_tags(&repo)?;
    if tags.is_empty() {
        return Err(format!("no v* tags found in {}", repo.display()).into());
    }
    let commits = rev_list(&repo, "HEAD")?;
    let all_commits: BTreeSet<String> = commits.iter().cloned().collect();
    let shipped = build_shipped_map(&repo, &tags)?;
    let reverted = find_reverts(&repo, &all_commits)?;
    let (sessions, fanout) = match &options.csr_db {
        Some(path) => load_git_to_session(path),
        None => (BTreeMap::new(), BTreeMap::new()),
    };
    let rows = derive_label_rows(&commits, &shipped, &reverted, &sessions);
    let output = labels_json(&repo, options.csr_db.as_deref(), &tags, &rows, &fanout);
    write_json(&output, options.out.as_deref())
}

fn list_all_tags(repo: &Path) -> AppResult<Vec<String>> {
    Ok(
        run_git(repo, &["tag", "--list", "v*", "--sort=version:refname"])?
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn rev_list(repo: &Path, revspec: &str) -> AppResult<Vec<String>> {
    let mut seen = BTreeSet::new();
    Ok(run_git(repo, &["rev-list", revspec])?
        .lines()
        .filter(|line| !line.is_empty())
        .filter(|line| seen.insert((*line).to_string()))
        .map(str::to_string)
        .collect())
}

fn build_shipped_map(repo: &Path, tags: &[String]) -> AppResult<BTreeMap<String, String>> {
    let mut claimed = BTreeMap::new();
    let mut already = BTreeSet::new();
    for (index, tag) in tags.iter().enumerate() {
        let revspec = if index == 0 {
            tag.clone()
        } else {
            format!("{}..{tag}", tags[index - 1])
        };
        for sha in rev_list(repo, &revspec)? {
            if already.insert(sha.clone()) {
                claimed.insert(sha, tag.clone());
            }
        }
    }
    Ok(claimed)
}

fn find_reverts(repo: &Path, all_commits: &BTreeSet<String>) -> AppResult<BTreeSet<String>> {
    let mut reverted = BTreeSet::new();
    for sha in all_commits {
        let log = run_git(repo, &["log", "-1", "--format=%s%x00%B", sha])?;
        let (subject, body) = log.split_once('\0').unwrap_or((&log, ""));
        if !is_revert_subject(subject) {
            continue;
        }
        reverted.insert(sha.clone());
        if let Some(prefix) = reverted_commit_prefix(body) {
            let matches: Vec<&String> = all_commits
                .iter()
                .filter(|candidate| candidate.starts_with(&prefix))
                .collect();
            match matches.as_slice() {
                [target] => {
                    reverted.insert((*target).clone());
                }
                many if many.len() > 1 => eprintln!(
                    "[labels] WARNING: revert {} names ambiguous prefix {} ({} candidates); original not relabeled",
                    &sha[..sha.len().min(12)],
                    prefix,
                    many.len()
                ),
                _ => {}
            }
        }
    }
    Ok(reverted)
}

fn is_revert_subject(subject: &str) -> bool {
    let mut chars = subject.chars();
    let prefix: String = chars.by_ref().take(6).collect();
    prefix.eq_ignore_ascii_case("revert")
        && chars
            .next()
            .is_none_or(|next| !(next.is_alphanumeric() || next == '_'))
}

fn reverted_commit_prefix(body: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let marker = "this reverts commit ";
    let start = lower.find(marker)? + marker.len();
    let prefix: String = lower[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_hexdigit())
        .take(40)
        .collect();
    (prefix.len() >= 7).then_some(prefix)
}

fn load_git_to_session(db_path: &Path) -> (BTreeMap<String, String>, BTreeMap<String, usize>) {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let result = (|| -> rusqlite::Result<Vec<(String, String)>> {
        let connection = Connection::open_with_flags(db_path, flags)?;
        let mut statement = connection.prepare(
            "SELECT g.source_id, t.source_id \
             FROM code_node_attribution g \
             JOIN code_node_attribution t ON t.node_id = g.node_id \
             WHERE g.channel = 'git' AND t.channel = 'transcript' \
             ORDER BY g.source_id, t.source_id",
        )?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect();
        rows
    })();
    let rows = match result {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!(
                "[labels] WARNING: could not read {} read-only ({error}); session linkage will be empty",
                db_path.display()
            );
            return (BTreeMap::new(), BTreeMap::new());
        }
    };
    let mut by_sha: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (sha, session) in rows {
        by_sha.entry(sha).or_default().insert(session);
    }
    let mapping = by_sha
        .iter()
        .filter_map(|(sha, sessions)| {
            sessions
                .first()
                .map(|session| (sha.clone(), session.clone()))
        })
        .collect();
    let fanout = by_sha
        .into_iter()
        .filter_map(|(sha, sessions)| (sessions.len() > 1).then_some((sha, sessions.len())))
        .collect();
    (mapping, fanout)
}

fn labels_json(
    repo: &Path,
    csr_db: Option<&Path>,
    tags: &[String],
    rows: &[LabelRow],
    fanout: &BTreeMap<String, usize>,
) -> Value {
    let n_total = rows.len();
    let n_labeled = rows
        .iter()
        .filter(|row| matches!(row.label, "shipped" | "reverted"))
        .count();
    let n_shipped = rows.iter().filter(|row| row.label == "shipped").count();
    let n_reverted = rows.iter().filter(|row| row.label == "reverted").count();
    let n_unreleased = rows.iter().filter(|row| row.label == "unreleased").count();
    let n_with_session = rows.iter().filter(|row| row.session_id.is_some()).count();
    let mut per_tag = BTreeMap::<String, usize>::new();
    for row in rows {
        if let Some(tag) = &row.release_tag {
            *per_tag.entry(tag.clone()).or_default() += 1;
        }
    }
    json!({
        "repo": repo.to_string_lossy(),
        "csr_db": csr_db.map(|path| path.to_string_lossy()),
        "n_tags": tags.len(),
        "first_tag": tags.first(),
        "last_tag": tags.last(),
        "n_commits_reachable": n_total,
        "n_labeled_shipped_or_reverted": n_labeled,
        "pct_labeled": percentage(n_labeled, n_total),
        "n_shipped": n_shipped,
        "n_reverted": n_reverted,
        "n_unreleased": n_unreleased,
        "n_with_session_linkage": n_with_session,
        "pct_with_session_linkage": percentage(n_with_session, n_total),
        "n_commits_with_session_fanout_gt1": fanout.len(),
        "per_tag_shipped_counts": tags.iter().map(|tag| json!({
            "release_tag": tag,
            "commits_shipped": per_tag.get(tag).copied().unwrap_or(0),
        })).collect::<Vec<_>>(),
        "labels": rows.iter().map(|row| json!({
            "commit": row.commit,
            "release_tag": row.release_tag,
            "label": row.label,
            "session_id": row.session_id,
        })).collect::<Vec<_>>(),
    })
}

fn percentage(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator != 0).then(|| {
        let value = 100.0 * numerator as f64 / denominator as f64;
        round_binary64_to_two_decimals(value)
    })
}

fn round_binary64_to_two_decimals(value: f64) -> f64 {
    debug_assert!(value.is_finite() && value >= 0.0);
    // CPython rounds the exact binary64 value, while multiplying by 100.0
    // first can round that intermediate and move it across a decimal tie.
    // Decode the float as significand * 2^exponent and round exact cents.
    let bits = value.to_bits();
    let exponent_bits = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1_u64 << 52) - 1);
    let (significand, binary_exponent) = if exponent_bits == 0 {
        (u128::from(fraction), -1074)
    } else {
        (
            u128::from((1_u64 << 52) | fraction),
            exponent_bits - 1023 - 52,
        )
    };
    let scaled = significand * 100;
    let cents = if binary_exponent >= 0 {
        scaled << u32::try_from(binary_exponent).expect("nonnegative exponent")
    } else {
        let shift = u32::try_from(-binary_exponent).expect("negated exponent is nonnegative");
        if shift >= u128::BITS {
            0
        } else {
            let quotient = scaled >> shift;
            let remainder = scaled - (quotient << shift);
            let halfway = 1_u128 << (shift - 1);
            quotient
                + u128::from(remainder > halfway || (remainder == halfway && quotient % 2 == 1))
        }
    };
    cents as f64 / 100.0
}

#[derive(Debug, Clone)]
struct StampRun {
    tag: String,
    at_oid: String,
    stats: BTreeMap<String, u64>,
    stderr: String,
}

#[derive(Debug, Clone)]
struct BeliefRow {
    tag: String,
    file: String,
    symbol: String,
    dream_stale: bool,
    ground_truth_stale: bool,
}

fn run_bench(options: BenchOptions) -> AppResult<()> {
    let repo = absolute(&options.repo)?;
    let binary = absolute(&options.binary)?;
    if !binary.is_file() {
        return Err(format!("binary not found: {}", binary.display()).into());
    }
    let owns_scratch = options.scratch_dir.is_none();
    let scratch = options.scratch_dir.unwrap_or_else(|| {
        env::temp_dir().join(format!("codewitness-bench-{}", std::process::id()))
    });
    let db_dir = scratch.join("scratch_db");
    let projects_dir = scratch.join("scratch_projects");
    reset_dir(&db_dir)?;
    reset_dir(&projects_dir)?;
    let db_path = db_dir.join("tierA.db");

    let all_tags = list_tags_in_range(&repo, &options.first_tag, &options.last_tag)?;
    let sampled = evenly_sample(&all_tags, options.tags_count)?;
    let mut tag_to_oid = BTreeMap::new();
    let mut runs = Vec::new();
    for tag in &sampled {
        let expected_oid = resolve_commit(&repo, tag)?;
        let run = stamp_at(&binary, &db_path, &projects_dir, &repo, tag)?;
        if run.at_oid != expected_oid {
            return Err(format!(
                "stamp-spans resolved {tag} to {}, expected {expected_oid}",
                run.at_oid
            )
            .into());
        }
        tag_to_oid.insert(tag.clone(), expected_oid);
        runs.push(run);
    }

    let by_oid = load_ledger_by_oid(&db_path)?;
    let ledger_counts = committed_symbol_counts_by_oid(&db_path)?;
    for run in &runs {
        validate_stamp_counter_coherence(run)?;
        let rows = ledger_counts.get(&run.at_oid).copied().unwrap_or(0);
        let spans_stamped = run.stats["spans_stamped"];
        if rows != i64::try_from(spans_stamped)? {
            return Err(format!(
                "stamp-spans ledger row count for {} @ {} was {rows}, but spans_stamped was {spans_stamped}",
                run.tag, run.at_oid
            )
            .into());
        }
    }
    let maps: BTreeMap<String, BTreeMap<(String, String), String>> = sampled
        .iter()
        .map(|tag| {
            let map = by_oid.get(&tag_to_oid[tag]).cloned().unwrap_or_default();
            (tag.clone(), map)
        })
        .collect();
    for run in &runs {
        let unique_beliefs = maps[&run.tag].len();
        let spans_stamped = usize::try_from(run.stats["spans_stamped"])?;
        if unique_beliefs != spans_stamped {
            return Err(format!(
                "stamp-spans unique belief count for {} @ {} was {unique_beliefs}, but spans_stamped was {spans_stamped}; duplicate (file, symbol) rows collapsed",
                run.tag, run.at_oid
            )
            .into());
        }
    }
    let final_tag = sampled.last().ok_or("tag sample is empty")?;
    let final_map = &maps[final_tag];
    if final_map.is_empty() {
        return Err(format!("final belief set is empty at {final_tag}").into());
    }
    let mut per_tag_metrics = Vec::new();
    let mut survival_curve = Vec::new();
    let mut beliefs = Vec::new();

    for (index, tag) in sampled.iter().enumerate() {
        let map = &maps[tag];
        let intact = map
            .iter()
            .filter(|(key, stamp)| final_map.get(*key) == Some(*stamp))
            .count();
        survival_curve.push(json!({
            "tag": tag,
            "n_beliefs": map.len(),
            "n_intact_at_final": intact,
            "survival_fraction": (!map.is_empty()).then(|| intact as f64 / map.len() as f64),
        }));
        if index == 0 || index + 1 == sampled.len() {
            continue;
        }
        let later_maps: Vec<&BTreeMap<(String, String), String>> = sampled[index + 1..]
            .iter()
            .map(|later| &maps[later])
            .collect();
        let mut predictions = Vec::new();
        let mut truths = Vec::new();
        let mut gt_counts = label_counts();
        let mut pred_counts = label_counts();
        for (key, stamp) in map {
            let (prediction, truth) = classify_belief(stamp, key, &later_maps, final_map);
            *pred_counts
                .get_mut(prediction)
                .expect("known prediction label") += 1;
            *gt_counts.get_mut(truth).expect("known ground-truth label") += 1;
            let dream_stale = prediction != "intact";
            let ground_truth_stale = truth != "intact";
            predictions.push(dream_stale);
            truths.push(ground_truth_stale);
            beliefs.push(BeliefRow {
                tag: tag.clone(),
                file: key.0.clone(),
                symbol: key.1.clone(),
                dream_stale,
                ground_truth_stale,
            });
        }
        let metrics = score_stale_predictions(&predictions, &truths)?;
        let mut value = metrics_json(&metrics);
        let object = value.as_object_mut().expect("metrics JSON is an object");
        object.insert("tag".into(), json!(tag));
        object.insert("at_oid".into(), json!(tag_to_oid[tag]));
        object.insert("gt_counts".into(), json!(gt_counts));
        object.insert("pred_counts".into(), json!(pred_counts));
        per_tag_metrics.push(value);
    }

    if beliefs.is_empty() {
        return Err(
            "aggregate scored-belief vector is empty: sampled intermediate tags contain no committed beliefs"
                .into(),
        );
    }

    let truths: Vec<bool> = beliefs.iter().map(|row| row.ground_truth_stale).collect();
    let mut arm_metrics = BTreeMap::new();
    arm_metrics.insert(
        "dream/CSR".to_string(),
        score_stale_predictions(
            &beliefs
                .iter()
                .map(|row| row.dream_stale)
                .collect::<Vec<_>>(),
            &truths,
        )?,
    );
    let mut tag_timestamps = BTreeMap::new();
    if !options.skip_baselines {
        for tag in &sampled {
            tag_timestamps.insert(tag.clone(), commit_timestamp(&repo, tag)?);
        }
        let final_timestamp = tag_timestamps[final_tag];
        let files: BTreeSet<String> = beliefs.iter().map(|row| row.file.clone()).collect();
        let final_contents: BTreeMap<String, Option<String>> = files
            .into_iter()
            .map(|file| {
                let content = final_file_content(&repo, final_tag, &file)?;
                Ok((file, content))
            })
            .collect::<AppResult<_>>()?;
        let grep_predictions: Vec<bool> = beliefs
            .iter()
            .map(|row| baseline_grep(&row.symbol, final_contents[&row.file].as_deref()))
            .collect();
        arm_metrics.insert(
            "grep".into(),
            score_stale_predictions(&grep_predictions, &truths)?,
        );
        for days in RECENCY_DAYS {
            let predictions: Vec<bool> = beliefs
                .iter()
                .map(|row| baseline_recency(tag_timestamps[&row.tag], final_timestamp, days))
                .collect();
            arm_metrics.insert(
                format!("recency-{days}"),
                score_stale_predictions(&predictions, &truths)?,
            );
        }
    }

    let run_stats: Vec<Value> = runs.iter().map(stamp_run_json).collect();
    let arm_json: BTreeMap<String, Value> = arm_metrics
        .iter()
        .map(|(name, metrics)| (name.clone(), metrics_json(metrics)))
        .collect();
    let results = json!({
        "provenance": {
            "repo_head_at_run": resolve_commit(&repo, "HEAD")?,
            "binary_sha256": sha256_file(&binary)?,
            "per_tag_stamping_stats": run_stats,
        },
        "repo": repo.to_string_lossy(),
        "first_tag": options.first_tag,
        "last_tag": options.last_tag,
        "tags_in_range": all_tags.len(),
        "sampled_tags": sampled,
        "tag_to_oid": tag_to_oid,
        "beliefs_scored": beliefs.len(),
        "arm_metrics": arm_json,
        "tag_commit_timestamps": tag_timestamps,
        "per_tag_metrics": per_tag_metrics,
        "survival_curve": survival_curve,
    });
    write_json(&results, options.out.as_deref())?;

    if !options.keep_db {
        remove_known_dir(&db_dir)?;
        remove_known_dir(&projects_dir)?;
        if owns_scratch && scratch.exists() {
            fs::remove_dir(&scratch)?;
        }
    }
    Ok(())
}

fn label_counts() -> BTreeMap<&'static str, usize> {
    BTreeMap::from([("intact", 0), ("obsolete", 0), ("superseded", 0)])
}

fn classify_belief<'a>(
    stamp: &str,
    key: &(String, String),
    later_maps: &[&BTreeMap<(String, String), String>],
    final_map: &BTreeMap<(String, String), String>,
) -> (&'a str, &'a str) {
    let superseded = later_maps
        .iter()
        .any(|map| map.get(key).is_some_and(|later| later != stamp));
    let prediction = if superseded {
        "superseded"
    } else if !final_map.contains_key(key) {
        "obsolete"
    } else {
        "intact"
    };
    let truth = match final_map.get(key) {
        None => "obsolete",
        Some(final_stamp) if final_stamp != stamp => "superseded",
        Some(_) => "intact",
    };
    (prediction, truth)
}

fn metrics_json(metrics: &Metrics) -> Value {
    json!({
        "n_beliefs": metrics.n_beliefs,
        "tp": metrics.tp,
        "fp": metrics.fp,
        "tn": metrics.tn,
        "fn": metrics.fn_,
        "precision_stale": metrics.precision_stale,
        "recall_stale": metrics.recall_stale,
        "f1_stale": metrics.f1_stale,
    })
}

fn stamp_run_json(run: &StampRun) -> Value {
    let mut object = Map::new();
    object.insert("tag".into(), json!(run.tag));
    object.insert("at_oid".into(), json!(run.at_oid));
    for (key, value) in &run.stats {
        object.insert(key.clone(), json!(value));
    }
    if !run.stderr.is_empty() {
        object.insert("stderr".into(), json!(run.stderr));
    }
    Value::Object(object)
}

fn list_tags_in_range(repo: &Path, first: &str, last: &str) -> AppResult<Vec<String>> {
    let tags = list_all_tags(repo)?;
    let first_index = tags
        .iter()
        .position(|tag| tag == first)
        .ok_or_else(|| format!("expected tag {first:?} not found in {}", repo.display()))?;
    let last_index = tags
        .iter()
        .position(|tag| tag == last)
        .ok_or_else(|| format!("expected tag {last:?} not found in {}", repo.display()))?;
    if first_index > last_index {
        return Err(format!("{first} sorts after {last} — range is empty").into());
    }
    Ok(tags[first_index..=last_index].to_vec())
}

fn evenly_sample(tags: &[String], count: usize) -> AppResult<Vec<String>> {
    if count >= tags.len() {
        return Ok(tags.to_vec());
    }
    if count < 2 {
        return Err("--tags-count must be >= 2 (need both endpoints)".into());
    }
    let mut indexes = BTreeSet::new();
    for index in 0..count {
        indexes.insert(round_ratio_ties_even(index * (tags.len() - 1), count - 1));
    }
    Ok(indexes
        .into_iter()
        .map(|index| tags[index].clone())
        .collect())
}

fn round_ratio_ties_even(numerator: usize, denominator: usize) -> usize {
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    match (remainder * 2).cmp(&denominator) {
        std::cmp::Ordering::Less => quotient,
        std::cmp::Ordering::Greater => quotient + 1,
        std::cmp::Ordering::Equal if quotient % 2 == 0 => quotient,
        std::cmp::Ordering::Equal => quotient + 1,
    }
}

fn resolve_commit(repo: &Path, rev: &str) -> AppResult<String> {
    Ok(run_git(repo, &["rev-parse", &format!("{rev}^{{commit}}")])?
        .trim()
        .to_string())
}

fn commit_timestamp(repo: &Path, rev: &str) -> AppResult<i64> {
    Ok(run_git(
        repo,
        &["show", "-s", "--format=%ct", &format!("{rev}^{{commit}}")],
    )?
    .trim()
    .parse()?)
}

fn baseline_recency(tag_timestamp: i64, final_timestamp: i64, days: i64) -> bool {
    tag_timestamp < final_timestamp - days * 86_400
}

fn stamp_at(
    binary: &Path,
    db_path: &Path,
    projects_dir: &Path,
    repo: &Path,
    tag: &str,
) -> AppResult<StampRun> {
    let output = Command::new(binary)
        .arg("--db-path")
        .arg(db_path)
        .arg("--projects-dir")
        .arg(projects_dir)
        .args(["codegraph", "stamp-spans", "--at", tag, "--repo"])
        .arg(repo)
        .output()?;
    ensure_success(binary, &output)?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    let at_oid = stdout
        .lines()
        .map(str::trim)
        .find_map(|line| {
            line.strip_prefix("at_commit")
                .and_then(|rest| rest.rsplit_once(':'))
                .map(|(_, oid)| oid.trim().to_string())
        })
        .ok_or_else(|| stamp_contract_error(tag, "omitted at_commit", &stderr))?;
    let stats =
        parse_stamp_stats(&stdout).map_err(|error| stamp_contract_error(tag, &error, &stderr))?;
    const ERROR_SKIPS: [&str; 8] = [
        "skipped_no_repo_root",
        "skipped_file_missing",
        "skipped_non_git",
        "skipped_outside_repo_root",
        "skipped_stamp_error",
        "skipped_span_out_of_range",
        "skipped_rev_unresolved",
        "skipped_non_utf8",
    ];
    let nonzero_skips: Vec<String> = ERROR_SKIPS
        .iter()
        .filter(|key| stats[**key] != 0)
        .map(|key| format!("{key}={}", stats[*key]))
        .collect();
    if !nonzero_skips.is_empty() {
        return Err(stamp_contract_error(
            tag,
            &format!("reported error-class skips: {}", nonzero_skips.join(", ")),
            &stderr,
        )
        .into());
    }
    Ok(StampRun {
        tag: tag.into(),
        at_oid,
        stats,
        stderr,
    })
}

fn stamp_contract_error(tag: &str, message: &str, stderr: &str) -> String {
    let stderr = stderr.trim();
    if stderr.is_empty() {
        format!("stamp-spans output for {tag} {message}")
    } else {
        format!("stamp-spans output for {tag} {message}; stderr: {stderr}")
    }
}

fn validate_stamp_counter_coherence(run: &StampRun) -> Result<(), String> {
    let checked = run.stats["files_checked"];
    let processed = run.stats["files_processed"];
    let spans = run.stats["spans_stamped"];
    let whole_files = run.stats["whole_file_witnesses"];
    let disambiguated = run.stats["disambiguated_symbols"];
    let reason = if processed != checked {
        Some(format!(
            "files_processed={processed} does not equal files_checked={checked} after zero error-class skips"
        ))
    } else if whole_files > processed {
        Some(format!(
            "whole_file_witnesses={whole_files} exceeds files_processed={processed}"
        ))
    } else if disambiguated > spans {
        Some(format!(
            "disambiguated_symbols={disambiguated} exceeds spans_stamped={spans}"
        ))
    } else {
        match spans.checked_add(whole_files) {
            Some(witnesses) if processed > witnesses => Some(format!(
                "files_processed={processed} exceeds spans_stamped + whole_file_witnesses={witnesses}"
            )),
            None => Some("spans_stamped + whole_file_witnesses overflows u64".into()),
            _ => None,
        }
    };
    match reason {
        Some(reason) => Err(format!(
            "incoherent stamp-spans counters for {} @ {}: {reason}",
            run.tag, run.at_oid
        )),
        None => Ok(()),
    }
}

fn parse_stamp_stats(stdout: &str) -> Result<BTreeMap<String, u64>, String> {
    let labels = BTreeMap::from([
        ("files checked", "files_checked"),
        ("files processed", "files_processed"),
        ("spans stamped", "spans_stamped"),
        ("whole-file witnesses", "whole_file_witnesses"),
        ("disambiguated symbols", "disambiguated_symbols"),
    ]);
    let mut stats = BTreeMap::new();
    for line in stdout.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("skipped:") {
            for field in rest.split_whitespace() {
                if let Some((key, value)) = field.split_once('=') {
                    if let Ok(value) = value.trim_end_matches(',').parse() {
                        stats.insert(format!("skipped_{key}"), value);
                    }
                }
            }
        } else if let Some((label, value)) = line.split_once(':') {
            if let (Some(key), Ok(value)) = (labels.get(label.trim()), value.trim().parse()) {
                stats.insert((*key).to_string(), value);
            }
        }
    }
    const REQUIRED: [&str; 13] = [
        "files_checked",
        "files_processed",
        "spans_stamped",
        "whole_file_witnesses",
        "disambiguated_symbols",
        "skipped_no_repo_root",
        "skipped_file_missing",
        "skipped_non_git",
        "skipped_outside_repo_root",
        "skipped_stamp_error",
        "skipped_span_out_of_range",
        "skipped_rev_unresolved",
        "skipped_non_utf8",
    ];
    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|key| !stats.contains_key(*key))
        .collect();
    if missing.is_empty() {
        Ok(stats)
    } else {
        Err(format!("omitted required counters: {}", missing.join(", ")))
    }
}

type Ledger = BTreeMap<String, BTreeMap<(String, String), String>>;

fn load_ledger_by_oid(db_path: &Path) -> AppResult<Ledger> {
    let connection = Connection::open(db_path)?;
    let mut statement = connection.prepare(
        "SELECT at_oid, file, symbol, stamp FROM witness_ledger \
         WHERE tier = 'committed' AND symbol IS NOT NULL \
         ORDER BY at_oid, file, symbol, stamp",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut ledger = Ledger::new();
    for row in rows {
        let (oid, file, symbol, stamp) = row?;
        ledger.entry(oid).or_default().insert((file, symbol), stamp);
    }
    Ok(ledger)
}

fn committed_symbol_counts_by_oid(db_path: &Path) -> AppResult<BTreeMap<String, i64>> {
    let connection = Connection::open(db_path)?;
    let mut statement = connection.prepare(
        "SELECT at_oid, COUNT(*) FROM witness_ledger \
         WHERE tier = 'committed' AND symbol IS NOT NULL \
         GROUP BY at_oid ORDER BY at_oid",
    )?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn final_file_content(repo: &Path, final_tag: &str, file: &str) -> AppResult<Option<String>> {
    let path = Path::new(file);
    let relative = if path.is_absolute() {
        match path.strip_prefix(repo) {
            Ok(relative) => relative,
            Err(_) => return Ok(None),
        }
    } else {
        path
    };
    if relative
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Ok(None);
    }
    let tree_path = relative
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["show", &format!("{final_tag}:{tree_path}")])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8(output.stdout)?))
}

fn sha256_file(path: &Path) -> AppResult<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn absolute(path: &Path) -> AppResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

fn run_git(repo: &Path, args: &[&str]) -> AppResult<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    ensure_success(Path::new("git"), &output)?;
    Ok(String::from_utf8(output.stdout)?)
}

fn ensure_success(program: &Path, output: &Output) -> AppResult<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} exited with {}: {}",
            program.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into())
    }
}

fn reset_dir(path: &Path) -> AppResult<()> {
    remove_known_dir(path)?;
    fs::create_dir_all(path)?;
    Ok(())
}

fn remove_known_dir(path: &Path) -> AppResult<()> {
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

fn write_json(value: &Value, out: Option<&Path>) -> AppResult<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    match out {
        Some(path) => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                fs::create_dir_all(parent)?;
            }
            File::create(path)?.write_all(&bytes)?;
        }
        None => std::io::stdout().write_all(&bytes)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_derivation_applies_revert_precedence_and_preserves_first_occurrence_order() {
        let commits = vec!["c3".into(), "c1".into(), "c3".into(), "c2".into()];
        let shipped = BTreeMap::from([
            ("c1".into(), "v1.0.0".into()),
            ("c2".into(), "v1.1.0".into()),
        ]);
        let reverted = BTreeSet::from(["c2".into(), "c3".into()]);
        let sessions = BTreeMap::from([("c1".into(), "session-a".into())]);

        let rows = derive_label_rows(&commits, &shipped, &reverted, &sessions);

        assert_eq!(
            rows,
            vec![
                LabelRow {
                    commit: "c3".into(),
                    release_tag: None,
                    label: "reverted",
                    session_id: None,
                },
                LabelRow {
                    commit: "c1".into(),
                    release_tag: Some("v1.0.0".into()),
                    label: "shipped",
                    session_id: Some("session-a".into()),
                },
                LabelRow {
                    commit: "c2".into(),
                    release_tag: Some("v1.1.0".into()),
                    label: "reverted",
                    session_id: None,
                },
            ]
        );
    }

    #[test]
    fn confusion_matrix_scores_all_four_quadrants() {
        let metrics = score_stale_predictions(
            &[true, true, false, false, true],
            &[true, false, true, false, true],
        )
        .unwrap();

        assert_eq!(metrics.n_beliefs, 5);
        assert_eq!(
            (metrics.tp, metrics.fp, metrics.tn, metrics.fn_),
            (2, 1, 1, 1)
        );
        assert!((metrics.precision_stale - 2.0 / 3.0).abs() < f64::EPSILON);
        assert!((metrics.recall_stale - 2.0 / 3.0).abs() < f64::EPSILON);
        assert!((metrics.f1_stale - 2.0 / 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn confusion_matrix_rejects_mismatched_vectors() {
        assert!(score_stale_predictions(&[true], &[]).is_err());
    }

    #[test]
    fn grep_normalizes_both_qualifiers_and_collision_suffixes() {
        let source = "fn method() {}\nfn other() {}\n";
        assert!(!baseline_grep("Container::method#2", Some(source)));
        assert!(!baseline_grep("Container.other#17", Some(source)));
        assert!(baseline_grep("Container:method#2", Some(source)));
        assert!(baseline_grep("Container::missing#3", Some(source)));
        assert!(baseline_grep("method", None));
    }

    #[test]
    fn percentage_uses_python_half_even_rounding() {
        assert_eq!(percentage(1, 4_000), Some(0.03));
        assert_eq!(percentage(1, 20_000), Some(0.01));
        assert_eq!(percentage(2_675, 100_000), Some(2.67));
        assert_eq!(percentage(1, 32), Some(3.12));
        assert_eq!(percentage(3, 8), Some(37.5));
        assert_eq!(percentage(0, 0), None);
    }

    #[test]
    fn revert_subject_matches_python_word_boundary_semantics() {
        assert!(is_revert_subject("Revert \"change\""));
        assert!(is_revert_subject("revert: change"));
        assert!(!is_revert_subject("Reverted change"));
        assert!(!is_revert_subject("Revert_change"));
    }
}
