//! Native Repoise CLI.
//!
//! Thin adapter over shared engine services (repoise-core). The CLI never
//! prompts and is safe to run from agents. Exit codes: 0 success, 1
//! operational failure (with a message on stderr), 2 usage error, 3 the
//! index is stale or missing for search/read/status.

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use repoise_core::NAME;
use repoise_core::adapter::{SnapshotMode, SourceAdapter};
use repoise_core::cache::CachePaths;
use repoise_core::classify::Role;
use repoise_core::config::{CliOverrides, ConfigPaths, EffectiveConfig, Preset};
use repoise_core::embed::{CancellationToken, EmbeddingClient};
use repoise_core::error::Error;
use repoise_core::search::{QueryEmbedder, SearchMode};
use repoise_core::store::Store;
use repoise_core::{CACHE_ENV_VAR, CONFIG_FILENAME, LOCAL_CONFIG_FILENAME};

mod capabilities;
mod events;
mod mcp;
mod output;
mod progress;
use output::{json, print_doctor, print_explain, print_init};

#[cfg(feature = "remote-embedding")]
mod provider;

#[cfg(feature = "github-enrichment")]
mod github_enrichment;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Greet,
    Help,
    Version,
    Doctor,
    Explain,
    Index,
    Init,
    Status,
    Search,
    Read,
    Related,
    Check,
    Watch,
    Overlay,
    Mcp,
    Purge,
    Capabilities,
    Bench,
}

struct CliOptions {
    command: Command,
    root: PathBuf,
    json: bool,
    config: Option<PathBuf>,
    local_config: Option<PathBuf>,
    max_file_bytes: Option<u64>,
    preset: Option<String>,
    provider: Option<String>,
    adopt_managed_block: Option<PathBuf>,
    agents_snippet: bool,
    dry_run: bool,
    yes: bool,
    explain_path: Option<String>,
    committed: bool,
    query: Option<String>,
    path_filter: Option<String>,
    role_filter: Option<String>,
    max_results: Option<u32>,
    max_output_tokens: Option<u64>,
    cursor: Option<String>,
    source_id: Option<String>,
    search_mode: Option<String>,
    rrf_k: Option<u32>,
    lane: Option<String>,
    relations: Vec<String>,
    fresh: bool,
    watch_interval_ms: Option<u64>,
    watch_debounce_ms: Option<u64>,
    watch_max_pending: Option<u64>,
    overlay_action: Option<String>,
    purge_all: bool,
    repo_id: Option<String>,
    worktree_id: Option<String>,
    allow_refresh: bool,
    /// Pinned retrieval mode for `repoise mcp` (`--search-mode`).
    mcp_search_mode: Option<String>,
    events: bool,
    suite_path: Option<PathBuf>,
    bench_runner: Option<PathBuf>,
    bench_action: Option<String>,
    experiment_id: Option<String>,
    repeat: Option<u32>,
    seed: Option<u64>,
    output_dir: Option<PathBuf>,
}

const USAGE: &str = "\
Repoise - offline repository knowledge indexer

USAGE:
    repoise <COMMAND> [OPTIONS]

COMMANDS:
    greet                         Print the project greeting
    doctor [ROOT]                 Show effective settings, capabilities, policy
    explain --path <REL> [ROOT]   Explain the include/exclude decision for a path
    index [ROOT]                  Build and publish the offline index (incremental;
                                  live phase progress, or to stderr with --json)
    status [ROOT]                 Show scope, snapshot, index and freshness
                                  (freshness tracks the saved snapshot and config;
                                  Git dirtiness is reported separately)
    search --query <Q> [ROOT]     Offline lexical search over the current index
    read --source-id <ID> [ROOT]  Exact read-back of a search result
    related --source-id <ID> [ROOT]  Follow structural links from a source
    check [ROOT]                  Offline coverage check; --fresh also gates on staleness
    watch [ROOT]                  Incremental watch loop; embeds changed inputs when configured (Ctrl+C cancels safely)
    mcp [ROOT]                    stdio MCP server (JSON-RPC 2.0 on stdin/stdout)
    overlay uninstall [ROOT]      Remove only the managed overlay (dry-run capable)
    overlay update [ROOT]         3-way template migration of the overlay
    init [ROOT]                   Configure the repository (non-interactive)
    purge                         Remove generated cache data (--all or one scope)
    capabilities                  Report version, event-stream protocol, features,
                                  commands, benchmark status and exit codes
    bench [SUITE]                 Run a suite-driven agent benchmark (AP-04; docs/bench.md)
    bench report                    Re-grade recorded trials and rebuild report.json/report.md
                                  (requires --experiment <ID>; docs/bench.md)
    help                          Show this help
    version                       Print version information

GLOBAL OPTIONS:
    --json                      Emit machine-readable JSON (index: progress to stderr)
    --events                    NDJSON event stream on stdout for index and watch
                                (machine lifecycle events; see docs/automation.md)
    --config <PATH>             Override the committed config path
    --local-config <PATH>       Override the local override path
    --max-file-bytes <N>        Override the max accepted file size
    --committed                 Use the committed Git snapshot (not the working tree)
    REPOISE_EVENTS_HEARTBEAT_MS Heartbeat interval in ms for --event
                                streams (default 15000, minimum 10)

SEARCH OPTIONS:
    --path-filter <GLOB>        Filter results by repository-relative path
    --role <ROLE>               Filter results by classification role
    --max-results <N>           Page size (default 5, cap 20)
    --max-output-tokens <N>     Output token budget (estimated)
    --cursor <TOKEN>            Opaque pagination token from a previous page
    --mode <MODE>               Retrieval mode: lexical (default), hybrid, vectors-only
    --rrf-k <N>                 Reciprocal-rank-fusion k for hybrid mode (default from config)
    --lane <LANE>               Search lane: search (default, current docs/code) or history

RETRIEVAL WORKFLOW:
    Lexical matching is strict: every whitespace query term must match the
    same chunk's searchable fields, so broad natural-language queries may
    return zero results even when relevant evidence is indexed. For empty or
    irrelevant results, try a shorter identifier/symbol query with
    --path-filter/--role before concluding indexed evidence is insufficient.
    Hybrid mode (with a complete embedding profile) templates the query with
    the configured input profile, ranks corpus-scoped stored vectors and fuses
    them with the lexical rank (RRF); a missing profile, credential or failed
    embedding degrades to lexical and the response reports `lexical`.
    Results carry short excerpts that locate evidence, not the full source;
    use `read --source-id <ID>` to inspect the exact implementation. A short
    excerpt is not evidence the indexed source is unavailable. When results
    are truncated and more are useful, pass the response's next_cursor value
    as the next request's --cursor. An empty `related` result does not
    establish missing evidence. Fall back to targeted filesystem reads when
    indexed evidence remains insufficient. Keep retries bounded; do not repeat
    poor searches or call repoise when sufficient evidence is already available.

READ OPTIONS:
    --source-id <ID>            Opaque source id from a search result

INIT OPTIONS:
    --preset <PRESET>           docs-only | docs-code-lexical | hybrid
    --provider <NAME>           Embedding provider (required for hybrid)
    --adopt-managed-block <FILE>  Append the managed marker block to FILE
    --agents-snippet              Manage an agent-guidance block in AGENTS.md
    --dry-run                   Report planned changes without writing
    --yes                       Confirm (never prompts; documents intent)

RELATED OPTIONS:
    --source-id <ID>            Opaque source id from search/related results
    --relation <KIND>           references | referenced-by | children | parent (repeatable; all when omitted)

CHECK OPTIONS:
    --fresh                     Also fail when the index is stale; without it, check
                                classifies missing/coverage only (stable exit codes:
                                0 ok, 1 error, 2 usage, 3 missing/stale, 4 coverage not met).
                                Stale means the saved snapshot or effective config no
                                longer matches the index; Git dirtiness is not staleness.

WATCH OPTIONS:
    --interval <MS>             Poll interval (default 500)
    --debounce <MS>             Debounce before publication (default 200)
    --max-pending <N>           Bounded changed-path set; beyond it: full reparse (default 4096)
    Live config reload: edits to the resolved configuration apply as one
                        config-driven generation on the running watch; an
                        invalid configuration freezes publications (throttled
                        warning, last generation stays served), and cacheDir /
                        sharedCache / history changes require a restart
                        (see docs/agents.md)

MCP OPTIONS:
    --allow-refresh             Expose the opt-in refresh tool (read-only by default)
    --search-mode <MODE>        Pin the retrieval mode for this server:
                                lexical (default), hybrid, vectors-only. Tool
                                requests naming a different mode are refused.

OVERLAY OPTIONS:
    uninstall | update          Overlay action (uninstall removes; update migrates)
    --dry-run                   Report changes without writing

PURGE OPTIONS:
    --all                       Remove the entire cache root
    --repo-id <ID> --worktree-id <ID>   Remove one scope directory

BENCH OPTIONS:
    SUITE (positional or --suite)   Benchmark suite manifest (JSON, schema v1);
                                    relative to the current directory
    --runner <PATH>         Override the suite's adapter executable
                            (env REPOISE_BENCH_RUNNER is the second fallback)
    --repeat <N>            Override the suite's repetition count
    --seed <N>              Override the suite's seed (deterministic plan)
    --output <DIR>          Override the artifact root (default: repoise-bench)
    --experiment <ID>       Experiment id for `bench report` (under --output)
    --dry-run               Show the frozen plan and prerequisites only
                            (no worktrees, no agent processes)
                            Exit codes: 0 completed, 1 operational failure,
                            2 usage, 3 preflight/suite failure, 4 interrupted
";

fn parse_args(args: &[String]) -> Result<CliOptions, String> {
    let mut opts = CliOptions {
        command: Command::Greet,
        root: env::current_dir().map_err(|err| err.to_string())?,
        json: false,
        config: None,
        local_config: None,
        max_file_bytes: None,
        preset: None,
        provider: None,
        adopt_managed_block: None,
        dry_run: false,
        yes: true,
        explain_path: None,
        committed: false,
        query: None,
        path_filter: None,
        role_filter: None,
        max_results: None,
        max_output_tokens: None,
        cursor: None,
        source_id: None,
        search_mode: None,
        rrf_k: None,
        lane: None,
        relations: Vec::new(),
        fresh: false,
        agents_snippet: false,
        watch_interval_ms: None,
        watch_debounce_ms: None,
        watch_max_pending: None,
        overlay_action: None,
        purge_all: false,
        repo_id: None,
        worktree_id: None,
        allow_refresh: false,
        mcp_search_mode: None,
        events: false,
        suite_path: None,
        bench_runner: None,
        bench_action: None,
        experiment_id: None,
        repeat: None,
        seed: None,
        output_dir: None,
    };
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "greet" => opts.command = Command::Greet,
            "help" | "--help" | "-h" => opts.command = Command::Help,
            "version" | "--version" | "-V" => opts.command = Command::Version,
            "doctor" => opts.command = Command::Doctor,
            "explain" => opts.command = Command::Explain,
            "index" => opts.command = Command::Index,
            "init" => opts.command = Command::Init,
            "status" => opts.command = Command::Status,
            "search" => opts.command = Command::Search,
            "read" => opts.command = Command::Read,
            "related" => opts.command = Command::Related,
            "check" => opts.command = Command::Check,
            "watch" => opts.command = Command::Watch,
            "overlay" => opts.command = Command::Overlay,
            "mcp" => opts.command = Command::Mcp,
            "purge" => opts.command = Command::Purge,
            "capabilities" => opts.command = Command::Capabilities,
            "bench" => opts.command = Command::Bench,
            "--json" => opts.json = true,
            "--events" => opts.events = true,
            "--dry-run" => opts.dry_run = true,
            "--yes" | "-y" => opts.yes = true,
            "--committed" => opts.committed = true,
            "--config" => {
                i += 1;
                opts.config = Some(PathBuf::from(require_value(args, i, "--config")?));
            }
            "--local-config" => {
                i += 1;
                opts.local_config = Some(PathBuf::from(require_value(args, i, "--local-config")?));
            }
            "--max-file-bytes" => {
                i += 1;
                let value = require_value(args, i, "--max-file-bytes")?;
                opts.max_file_bytes = Some(value.parse::<u64>().map_err(|_| {
                    format!("--max-file-bytes must be a positive integer: {value}")
                })?);
            }
            "--preset" => {
                i += 1;
                opts.preset = Some(require_value(args, i, "--preset")?);
            }
            "--provider" => {
                i += 1;
                opts.provider = Some(require_value(args, i, "--provider")?);
            }
            "--adopt-managed-block" => {
                i += 1;
                opts.adopt_managed_block = Some(PathBuf::from(require_value(
                    args,
                    i,
                    "--adopt-managed-block",
                )?));
            }
            "--path" => {
                i += 1;
                opts.explain_path = Some(require_value(args, i, "--path")?);
            }
            "--query" => {
                i += 1;
                opts.query = Some(require_value(args, i, "--query")?);
            }
            "--path-filter" => {
                i += 1;
                opts.path_filter = Some(require_value(args, i, "--path-filter")?);
            }
            "--role" => {
                i += 1;
                opts.role_filter = Some(require_value(args, i, "--role")?);
            }
            "--max-results" => {
                i += 1;
                let value = require_value(args, i, "--max-results")?;
                opts.max_results =
                    Some(value.parse::<u32>().map_err(|_| {
                        format!("--max-results must be a positive integer: {value}")
                    })?);
            }
            "--max-output-tokens" => {
                i += 1;
                let value = require_value(args, i, "--max-output-tokens")?;
                opts.max_output_tokens = Some(value.parse::<u64>().map_err(|_| {
                    format!("--max-output-tokens must be a positive integer: {value}")
                })?);
            }
            "--cursor" => {
                i += 1;
                opts.cursor = Some(require_value(args, i, "--cursor")?);
            }
            "--source-id" => {
                i += 1;
                opts.source_id = Some(require_value(args, i, "--source-id")?);
            }
            "--mode" => {
                i += 1;
                opts.search_mode = Some(require_value(args, i, "--mode")?);
            }
            "--rrf-k" => {
                i += 1;
                let value = require_value(args, i, "--rrf-k")?;
                opts.rrf_k = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| format!("--rrf-k must be a positive integer: {value}"))?,
                );
            }
            "--lane" => {
                i += 1;
                opts.lane = Some(require_value(args, i, "--lane")?);
            }
            "--relation" => {
                i += 1;
                opts.relations.push(require_value(args, i, "--relation")?);
            }
            "--fresh" => opts.fresh = true,
            "--agents-snippet" => opts.agents_snippet = true,
            "--interval" => {
                i += 1;
                let value = require_value(args, i, "--interval")?;
                opts.watch_interval_ms = Some(parse_positive_u64("--interval", &value)?);
            }
            "--debounce" => {
                i += 1;
                let value = require_value(args, i, "--debounce")?;
                opts.watch_debounce_ms = Some(parse_positive_u64("--debounce", &value)?);
            }
            "--max-pending" => {
                i += 1;
                let value = require_value(args, i, "--max-pending")?;
                opts.watch_max_pending = Some(parse_positive_u64("--max-pending", &value)?);
            }
            "--all" => opts.purge_all = true,
            "--allow-refresh" => opts.allow_refresh = true,
            "--search-mode" => {
                i += 1;
                let value = require_value(args, i, "--search-mode")?;
                match value.as_str() {
                    "lexical" | "hybrid" | "vectors-only" => {
                        opts.mcp_search_mode = Some(value);
                    }
                    _ => {
                        return Err(format!(
                            "--search-mode must be one of: lexical, hybrid, vectors-only (got {value})"
                        ));
                    }
                }
            }
            "--repo-id" => {
                i += 1;
                opts.repo_id = Some(require_value(args, i, "--repo-id")?);
            }
            "--worktree-id" => {
                i += 1;
                opts.worktree_id = Some(require_value(args, i, "--worktree-id")?);
            }
            "--suite" => {
                i += 1;
                opts.suite_path = Some(PathBuf::from(require_value(args, i, "--suite")?));
            }
            "--runner" => {
                i += 1;
                opts.bench_runner = Some(PathBuf::from(require_value(args, i, "--runner")?));
            }
            "--experiment" => {
                i += 1;
                opts.experiment_id = Some(require_value(args, i, "--experiment")?);
            }
            "--repeat" => {
                i += 1;
                let value = require_value(args, i, "--repeat")?;
                opts.repeat = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| format!("--repeat must be a positive integer: {value}"))?,
                );
            }
            "--seed" => {
                i += 1;
                let value = require_value(args, i, "--seed")?;
                opts.seed = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| format!("--seed must be a non-negative integer: {value}"))?,
                );
            }
            "--output" => {
                i += 1;
                opts.output_dir = Some(PathBuf::from(require_value(args, i, "--output")?));
            }
            value if value.starts_with('-') && value.len() > 1 => {
                return Err(format!("unknown option: {value}"));
            }
            value => positional.push(value.to_string()),
        }
        i += 1;
    }
    if let Some(first) = positional.first() {
        match opts.command {
            Command::Doctor
            | Command::Explain
            | Command::Index
            | Command::Init
            | Command::Status
            | Command::Search
            | Command::Read
            | Command::Related
            | Command::Check
            | Command::Watch
            | Command::Mcp => opts.root = PathBuf::from(first),
            Command::Bench => {
                if first == "report" {
                    if opts.suite_path.is_some() {
                        return Err("bench report takes no suite manifest".into());
                    }
                    opts.bench_action = Some("report".into());
                } else {
                    if opts.suite_path.is_some() {
                        return Err(
                            "bench takes the suite positionally or via --suite (not both)".into(),
                        );
                    }
                    opts.suite_path = Some(PathBuf::from(first));
                }
            }
            Command::Overlay => match first.as_str() {
                "uninstall" | "update" => {
                    opts.overlay_action = Some(first.to_string());
                    if let Some(root) = positional.get(1) {
                        opts.root = PathBuf::from(root);
                    }
                }
                _ => {
                    return Err(format!(
                        "overlay requires an action (uninstall or update): {first}"
                    ));
                }
            },
            Command::Greet
            | Command::Help
            | Command::Version
            | Command::Purge
            | Command::Capabilities => {
                return Err(format!("unexpected argument: {first}"));
            }
        }
        let limit = match opts.command {
            Command::Overlay => 2,
            _ => 1,
        };
        if positional.len() > limit {
            return Err(format!("unexpected argument: {}", positional[limit]));
        }
    }
    if matches!(opts.command, Command::Overlay) && opts.overlay_action.is_none() {
        return Err("overlay requires an action (uninstall or update)".into());
    }
    if opts.json && opts.events {
        return Err("--events and --json are mutually exclusive".into());
    }
    if opts.events && !matches!(opts.command, Command::Index | Command::Watch) {
        return Err("--events is supported for the index and watch commands only".into());
    }
    Ok(opts)
}

fn require_value(args: &[String], index: usize, flag: &str) -> Result<String, String> {
    args.get(index)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_positive_u64(flag: &str, value: &str) -> Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("{flag} must be a positive integer: {value}"))?;
    if parsed == 0 {
        return Err(format!("{flag} must be a positive integer: {value}"));
    }
    Ok(parsed)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let opts = match parse_args(&args) {
        Ok(opts) => opts,
        Err(message) => {
            let _ = writeln!(
                io::stderr().lock(),
                "error: {message}\nTry 'repoise --help' for usage."
            );
            let _ = write!(io::stderr().lock(), "\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let code = match run(opts) {
        Ok(code) => code,
        Err(message) => {
            let _ = writeln!(io::stderr().lock(), "error: {message}");
            ExitCode::from(1)
        }
    };
    let _ = io::stdout().flush();
    code
}

fn run(opts: CliOptions) -> Result<ExitCode, String> {
    match opts.command {
        Command::Greet => {
            println!("{NAME} says hello");
            Ok(ExitCode::SUCCESS)
        }
        Command::Help => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Version => {
            println!("repoise {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor => run_doctor(&opts),
        Command::Explain => run_explain(&opts),
        Command::Index => run_index(&opts),
        Command::Init => run_init(&opts),
        Command::Status => run_status(&opts),
        Command::Search => run_search(&opts),
        Command::Read => run_read(&opts),
        Command::Related => run_related(&opts),
        Command::Check => run_check(&opts),
        Command::Watch => run_watch(&opts),
        Command::Mcp => mcp::run_mcp(&opts),
        Command::Overlay => run_overlay(&opts),
        Command::Purge => run_purge(&opts),
        Command::Capabilities => run_capabilities(&opts),
        Command::Bench => run_bench(&opts),
    }
}

fn run_capabilities(opts: &CliOptions) -> Result<ExitCode, String> {
    let document = capabilities::document(events::heartbeat_interval().as_millis() as u64);
    if opts.json {
        println!("{}", json(&document)?);
    } else {
        capabilities::print_human(&document);
    }
    Ok(ExitCode::SUCCESS)
}

fn run_bench(opts: &CliOptions) -> Result<ExitCode, String> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    // `bench report` (AP-05) re-grades recorded trials offline and rebuilds
    // the report from the frozen manifest and trial artifacts; it starts no
    // worktrees and no agent processes.
    if opts.bench_action.as_deref() == Some("report") {
        let experiment_id = opts.experiment_id.clone().ok_or_else(|| {
            "bench report requires --experiment <EXPERIMENT_ID> (the id printed by bench runs; under --output, default repoise-bench)".to_string()
        })?;
        let output = opts
            .output_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("repoise-bench"));
        let request = repoise_core::bench::RebuildRequest {
            output,
            experiment_id,
            json: opts.json,
        };
        let outcome = repoise_core::bench::rebuild(&request).map_err(|err| err.to_string())?;
        if opts.json {
            println!("{}", json(&outcome.payload)?);
        } else {
            println!("{}", outcome.summary);
        }
        return Ok(ExitCode::from(outcome.exit_code));
    }
    let suite_path = opts.suite_path.clone().ok_or_else(|| {
        "bench requires a suite manifest: bench <SUITE> or bench --suite <PATH>".to_string()
    })?;
    // Runner resolution: explicit --runner, then the REPOISE_BENCH_RUNNER env
    // override, then the suite's adapter.executable.
    let runner = opts.bench_runner.clone().or_else(|| {
        env::var("REPOISE_BENCH_RUNNER")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
    });
    let self_exe = env::current_exe().map_err(|err| err.to_string())?;
    let request = repoise_core::bench::BenchRequest {
        suite_path,
        dry_run: opts.dry_run,
        json: opts.json,
        runner,
        repeat: opts.repeat,
        seed: opts.seed,
        output: opts.output_dir.clone(),
        self_exe,
    };
    let interrupted = Arc::new(AtomicBool::new(false));
    let handler = interrupted.clone();
    if let Err(err) = ctrlc::set_handler(move || {
        handler.store(true, Ordering::SeqCst);
    }) {
        return Err(format!("could not install the signal handler: {err}"));
    }
    let outcome = repoise_core::bench::run(&request, interrupted).map_err(|err| err.to_string())?;
    if opts.json {
        println!("{}", json(&outcome.payload)?);
    } else {
        println!("{}", outcome.summary);
    }
    Ok(ExitCode::from(outcome.exit_code))
}

fn cli_overrides(opts: &CliOptions) -> Result<CliOverrides, String> {
    let preset = match &opts.preset {
        Some(name) => Some(Preset::parse(name).ok_or_else(|| {
            format!("unknown preset: {name} (expected docs-only, docs-code-lexical, or hybrid)")
        })?),
        None => None,
    };
    Ok(CliOverrides {
        preset,
        include: Vec::new(),
        exclude: Vec::new(),
        max_file_bytes: opts.max_file_bytes,
    })
}

fn config_paths(opts: &CliOptions) -> ConfigPaths {
    ConfigPaths {
        committed: opts.config.clone(),
        local: opts.local_config.clone(),
    }
}

fn run_doctor(opts: &CliOptions) -> Result<ExitCode, String> {
    let cli = cli_overrides(opts)?;
    let paths = config_paths(opts);
    let report = repoise_core::doctor::doctor(&opts.root, &cli, &paths, opts.committed)
        .map_err(|err| err.to_string())?;
    if opts.json {
        println!("{}", json(&report)?);
    } else {
        print_doctor(&report);
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    if !report.diagnostics.is_empty() {
        for diagnostic in &report.diagnostics {
            eprintln!("diagnostic: {diagnostic}");
        }
        return Err("doctor reported diagnostics".to_string());
    }
    Ok(ExitCode::SUCCESS)
}

fn run_explain(opts: &CliOptions) -> Result<ExitCode, String> {
    let relative = opts
        .explain_path
        .as_deref()
        .ok_or("--path <REL> is required for explain")?;
    let cli = cli_overrides(opts)?;
    let paths = config_paths(opts);
    let effective = repoise_core::config::EffectiveConfig::resolve_with(&opts.root, &paths, &cli)
        .map_err(|err| err.to_string())?;
    let adapter = default_adapter(&opts.root).map_err(|err| err.to_string())?;
    let decision = repoise_core::discovery::explain_path(
        adapter.as_ref(),
        adapter_mode(&opts.root, opts.committed),
        &effective,
        std::path::Path::new(relative),
    )
    .map_err(|err| err.to_string())?;
    if opts.json {
        let value = serde_json::json!({ "path": relative, "decision": decision });
        println!("{}", json(&value)?);
    } else {
        print_explain(relative, &decision);
    }
    Ok(ExitCode::SUCCESS)
}

fn run_index(opts: &CliOptions) -> Result<ExitCode, String> {
    let ctx = build_context(opts)?;
    let db_path = ctx.cache.db_path(&ctx.repo_id, &ctx.worktree_id);
    let scope_dir = db_path
        .parent()
        .ok_or_else(|| "internal error: scope directory has no parent".to_string())?;
    // Authoritative one-writer-per-scope lease: another live writer owns the
    // scope (its OS lock is held) and this run is refused before any provider
    // work or publication.
    let lock_result = repoise_core::ownership::ScopeLock::acquire(scope_dir, "index");
    if opts.events {
        return run_index_events(&ctx, lock_result);
    }
    let owner_guard = match lock_result {
        Err(err) => return Err(err.to_string()),
        Ok(guard) => {
            for warning in &guard.warnings {
                eprintln!("warning: {warning}");
            }
            guard
        }
    };
    let store = Store::new(ctx.cache.db_path(&ctx.repo_id, &ctx.worktree_id));
    let request = repoise_core::indexing::IndexRequest::default();
    let client = embedding_client(&ctx.effective, CancellationToken::new())?;
    let session = client.as_ref();
    let enrichment = enrichment_session(&ctx)?;
    let enrichment_session = enrichment.as_ref();
    // Progress renders to stderr in JSON mode so stdout stays a single JSON
    // document; to stdout otherwise.
    let reporter = progress::ProgressReporter::new(opts.json);
    // Immediate start feedback so a long discovery never looks stuck.
    let started = format!(
        "indexing {}/{} ({:?}): discovery started",
        ctx.repo_id, ctx.worktree_id, ctx.mode
    );
    if opts.json {
        eprintln!("{started}");
    } else {
        println!("{started}");
    }
    let wrapped = OwnerRefreshingReporter {
        inner: reporter,
        guard: &owner_guard,
    };
    let progress: Option<&dyn repoise_core::progress::IndexProgressReporter> = Some(&wrapped);
    let result = repoise_core::indexing::index(
        ctx.adapter.as_ref(),
        ctx.mode,
        &ctx.effective,
        &store,
        &ctx.cache,
        &request,
        session,
        enrichment_session,
        progress,
    );
    // Terminate any unterminated in-place progress line so the final summary
    // or an error starts on a fresh line (no-op when piped).
    wrapped.inner.finish();
    let outcome = result.map_err(|err| err.to_string())?;
    if opts.json {
        println!("{}", json(&outcome)?);
    } else {
        println!("mode: {}", outcome.snapshot_mode);
        println!("scope: {}/{}", outcome.repo_id, outcome.worktree_id);
        println!(
            "revision: {}",
            outcome
                .snapshot
                .revision_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "(none)".into())
        );
        println!("generation: {}", outcome.generation_id);
        println!("manifest: {}", outcome.manifest_hash);
        println!(
            "files: {} (reused {}, reparsed {}, skipped {})",
            outcome.files_indexed,
            outcome.files_reused,
            outcome.files_reparsed,
            outcome.files_skipped
        );
        println!(
            "chunks: {} (reused {}, added {}, removed {})",
            outcome.chunks_total,
            outcome.chunks_reused,
            outcome.chunks_added,
            outcome.chunks_removed
        );
        match (&outcome.family_id, &outcome.sharing_fallback) {
            (Some(family), None) => {
                println!(
                    "family: {family} ({} files, {} chunks reused from shared parse artifacts)",
                    outcome.files_family_reused, outcome.chunks_family_reused
                );
            }
            (Some(_), Some(reason)) | (None, Some(reason)) => {
                println!("sharing: {reason}");
            }
            (None, None) => {}
        }
        if outcome.symbols_total > 0 || outcome.references_total > 0 {
            println!(
                "code: {} symbols, {} reference edges",
                outcome.symbols_total, outcome.references_total
            );
        }
        if let Some(stats) = &outcome.embedding {
            println!(
                "vectors: {} ({} cached, {} embedded, {} failed, {} pending, {} requests)",
                outcome.vectors_total,
                stats.cache_hits,
                stats.embedded,
                stats.failed,
                stats.pending,
                stats.requests
            );
        } else {
            println!("vectors: 0 (lexical-only build)");
        }
        if let Some(history) = &outcome.history {
            println!(
                "history: {} items ({} verified, {} no-association, {} failed)",
                history.count, history.enriched, history.no_association, history.failed
            );
            for gap in &history.gaps {
                println!("  gap: {gap}");
            }
            if history.rate_limited {
                println!("  note: host rate limit stopped enrichment");
            }
            if history.permission_denied {
                println!("  note: permission denied; cached remote content invalidated");
            }
            if history.budget_exhausted {
                println!("  note: request budget stopped enrichment");
            }
            if history.unattempted > 0 {
                println!(
                    "  note: {} hint item(s) left unattempted (budget or PR cap)",
                    history.unattempted
                );
            }
        }
        println!(
            "elapsed: {}",
            progress::format_duration(wrapped.inner.elapsed())
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Wraps a progress reporter so the owner heartbeat marker is refreshed
/// while a long build runs (the marker is the descriptive health signal for
/// concurrent `status` callers; the OS lock is the authoritative lease).
struct OwnerRefreshingReporter<'a, R> {
    inner: R,
    guard: &'a repoise_core::ownership::ScopeLock,
}

impl<R: repoise_core::progress::IndexProgressReporter> repoise_core::progress::IndexProgressReporter
    for OwnerRefreshingReporter<'_, R>
{
    fn phase_started(&self, phase: repoise_core::progress::IndexPhase, total: Option<u64>) {
        self.guard.refresh();
        self.inner.phase_started(phase, total);
    }

    fn progress(&self, phase: repoise_core::progress::IndexPhase, done: u64, total: u64) {
        self.guard.refresh();
        self.inner.progress(phase, done, total);
    }

    fn phase_finished(&self, phase: repoise_core::progress::IndexPhase) {
        self.guard.refresh();
        self.inner.phase_finished(phase);
    }
}

/// Runs `index` in `--events` mode: the machine stdout carries the NDJSON
/// event stream (started, progress, warning, heartbeat, completed/failed);
/// diagnostics go to stderr.
fn run_index_events(
    ctx: &Context,
    lock_result: Result<
        repoise_core::ownership::ScopeLock,
        repoise_core::ownership::OwnershipError,
    >,
) -> Result<ExitCode, String> {
    use serde_json::json;

    let root = ctx
        .adapter
        .canonical_root()
        .map_err(|err| err.to_string())?;
    let scope = events::Scope {
        root: root.to_string_lossy().to_string(),
        repo_id: ctx.repo_id.clone(),
        worktree_id: ctx.worktree_id.clone(),
        snapshot_mode: events::scope_mode_name(ctx.mode).to_string(),
    };
    let stream = events::EventStream::stdout("index");
    stream.start(&scope);
    stream.start_heartbeat(events::heartbeat_interval());
    let scope_value = serde_json::to_value(&scope).unwrap_or_default();
    // A refused writer stops before any provider work or publication: the
    // terminal event carries the stable machine reason and retryability.
    let owner_guard = match lock_result {
        Err(err) => {
            let (reason, retryable) = repoise_core::ownership::failure_reason(&err);
            stream.terminal(
                "failed",
                json!({
                    "operation": "index",
                    "scope": scope_value.clone(),
                    "code": "operational",
                    "reason": reason,
                    "message": err.to_string(),
                    "retryable": retryable,
                }),
            );
            return Err(err.to_string());
        }
        Ok(guard) => {
            for warning in &guard.warnings {
                stream.emit(
                    "warning",
                    json!({ "operation": "index", "message": warning }),
                );
            }
            guard
        }
    };
    let report_setup_failure = |err: String| {
        stream.terminal(
            "failed",
            json!({
                "operation": "index",
                "scope": scope_value.clone(),
                "code": "operational",
                "message": err,
                "retryable": serde_json::Value::Null,
            }),
        );
        err
    };
    let store = Store::new(ctx.cache.db_path(&ctx.repo_id, &ctx.worktree_id));
    let request = repoise_core::indexing::IndexRequest::default();
    let client =
        embedding_client(&ctx.effective, CancellationToken::new()).map_err(report_setup_failure)?;
    let session = client.as_ref();
    let enrichment = enrichment_session(ctx).map_err(report_setup_failure)?;
    let enrichment_session = enrichment.as_ref();
    // Explicit degradation notes as machine-readable warning events.
    if ctx.effective.embedding.is_some() && session.is_none() {
        stream.emit(
            "warning",
            json!({
                "operation": "index",
                "message": "embedding is configured but unavailable (missing profile, credential, or transport); this build is lexical-only"
            }),
        );
    }
    let reporter = events::EventProgressReporter::new(stream.clone());
    let wrapped = OwnerRefreshingReporter {
        inner: reporter,
        guard: &owner_guard,
    };
    let result = repoise_core::indexing::index(
        ctx.adapter.as_ref(),
        ctx.mode,
        &ctx.effective,
        &store,
        &ctx.cache,
        &request,
        session,
        enrichment_session,
        Some(&wrapped),
    );
    match result {
        Ok(outcome) => {
            stream.terminal(
                "completed",
                json!({
                    "operation": "index",
                    "scope": scope_value,
                    "result": serde_json::to_value(&outcome).unwrap_or_default(),
                }),
            );
            eprintln!("index run {} completed", stream.run_id());
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => {
            stream.terminal(
                "failed",
                json!({
                    "operation": "index",
                    "scope": scope_value,
                    "code": "operational",
                    "message": err.to_string(),
                    "retryable": serde_json::Value::Null,
                }),
            );
            Err(err.to_string())
        }
    }
}

/// Builds the optional history enrichment session from the effective config.
/// A missing credential or a missing host transport keeps the build
/// local-only (hints stay unverified); local indexing never fails for it.
fn enrichment_session(
    ctx: &Context,
) -> Result<Option<repoise_core::history::EnrichmentSession>, String> {
    let Some(settings) = &ctx.effective.history.enrichment else {
        return Ok(None);
    };
    if settings.host != "github" {
        eprintln!(
            "note: no built-in enrichment transport for host '{}'; hints stay unverified",
            settings.host
        );
        return Ok(None);
    }
    #[cfg(feature = "github-enrichment")]
    {
        let token = match env::var(&settings.token_env) {
            Ok(token) if !token.trim().is_empty() => token,
            _ => {
                // No credential means no enrichment at all: never send
                // unauthenticated requests (they would leak private-repo
                // owner/name/SHAs and burn the anonymous quota).
                eprintln!(
                    "note: token variable {} is not set; enrichment is skipped and hints stay unverified",
                    settings.token_env
                );
                return Ok(None);
            }
        };
        let provider = github_enrichment::GitHubEnrichment::new(
            "https://api.github.com",
            Some(token),
            settings.max_body_chars,
        );
        let cache = repoise_core::history::HostCache::open(
            ctx.cache.host_cache_path(&ctx.repo_id, "github"),
            "github",
        );
        let budgets = repoise_core::history::EnrichmentBudgets {
            max_prs_per_build: settings.max_prs_per_build,
            max_requests_per_build: settings.max_requests_per_build,
            max_body_chars: settings.max_body_chars,
        };
        Ok(Some(repoise_core::history::EnrichmentSession::new(
            Box::new(provider),
            cache,
            budgets,
        )))
    }
    #[cfg(not(feature = "github-enrichment"))]
    {
        eprintln!("note: this binary has no GitHub enrichment transport; hints stay unverified");
        Ok(None)
    }
}

fn run_status(opts: &CliOptions) -> Result<ExitCode, String> {
    let ctx = build_context(opts)?;
    let store = Store::new(ctx.cache.db_path(&ctx.repo_id, &ctx.worktree_id));
    let view = repoise_core::status::status(
        ctx.adapter.as_ref(),
        ctx.mode,
        &ctx.effective,
        ctx.config_file.as_deref(),
        &store,
        &ctx.cache,
    )
    .map_err(|err| err.to_string())?;
    if opts.json {
        println!("{}", json(&view)?);
    } else {
        println!("root: {}", view.scope.root.display());
        println!("adapter: {} mode: {}", view.scope.adapter, view.scope.mode);
        output::print_sharing(&view.sharing);
        println!("scope: {}/{}", view.scope.repo_id, view.scope.worktree_id);
        println!(
            "remote: {}",
            view.scope.remote_identity.as_deref().unwrap_or("(none)")
        );
        println!("snapshot: {}", view.snapshot.snapshot_id);
        println!(
            "revision: {}",
            view.snapshot.revision.as_deref().unwrap_or("(none)")
        );
        println!(
            "branch: {}",
            view.snapshot
                .branch
                .as_deref()
                .unwrap_or("(detached or none)")
        );
        println!("dirty: {}", view.snapshot.dirty_count);
        match &view.index {
            Some(index) => {
                println!(
                    "index: generation {} ({} files, {} chunks, built {})",
                    index.generation_id, index.files, index.chunks, index.built_at_ms
                );
                let profile = index
                    .vector_profile
                    .as_deref()
                    .map(|p| format!(" profile {p}"))
                    .unwrap_or_default();
                let pending = if index.vectors_pending > 0 {
                    format!(" ({} eligible pending)", index.vectors_pending)
                } else {
                    String::new()
                };
                if index.embedding_coverage == "disabled" {
                    println!("vectors: 0 (lexical-only build){profile}");
                } else {
                    println!(
                        "vectors: {} / {} eligible (scope {}; {} lexical-only){profile}{pending}",
                        index.vectors,
                        index.eligible_chunks,
                        index.embedding_scope,
                        index.ineligible_chunks
                    );
                }
            }
            None => println!("index: (none)"),
        }
        match &view.history_lane {
            Some(lane) if lane.enabled => {
                println!(
                    "history lane: on ({} items, head {})",
                    lane.items,
                    lane.head_revision.as_deref().unwrap_or("(none)")
                );
                for gap in &lane.gaps {
                    println!("  gap: {gap}");
                }
                if let Some(summary) = &lane.enrichment {
                    println!(
                        "  enrichment: {} enriched, {} no-association, {} failed, {} unattempted",
                        summary.enriched,
                        summary.no_association,
                        summary.failed,
                        summary.unattempted
                    );
                    if summary.rate_limited {
                        println!("  rate_limited: true");
                    }
                    if summary.permission_denied {
                        println!("  permission_denied: true");
                    }
                    if summary.budget_exhausted {
                        println!("  budget_exhausted: true");
                    }
                }
            }
            Some(_) => println!("history lane: off"),
            None => {}
        }
        println!("freshness: {}", view.freshness.status);
        for reason in &view.freshness.reasons {
            println!("  - {reason}");
        }
        println!(
            "cache: {} ({} bytes)",
            view.cache.db_path.display(),
            view.cache.db_bytes
        );
        if view.writer.lock_held {
            if let Some(owner) = &view.owner {
                println!("writer: owned (pid {}, {})", owner.pid, owner.operation);
            } else {
                println!("writer: busy (lock held; owner metadata unavailable)");
            }
        } else if let Some(owner) = &view.owner {
            println!(
                "writer: stale metadata (pid {}, {}; no writer holds the scope)",
                owner.pid, owner.operation
            );
        } else {
            println!("writer: free");
        }
        if let Some(err) = &view.last_error {
            println!("last error: {err}");
        }
        for problem in &view.config.problems {
            eprintln!("config problem: {problem}");
        }
    }
    if view.index.is_none() || view.freshness.status == "stale" {
        return Ok(ExitCode::from(3));
    }
    Ok(ExitCode::SUCCESS)
}

fn run_init(opts: &CliOptions) -> Result<ExitCode, String> {
    let preset = match &opts.preset {
        Some(name) => Preset::parse(name).ok_or_else(|| {
            format!("unknown preset: {name} (expected docs-only, docs-code-lexical, or hybrid)")
        })?,
        None => Preset::DEFAULT,
    };
    let options = repoise_core::init::InitOptions {
        preset,
        dry_run: opts.dry_run,
        yes: opts.yes,
        provider: opts.provider.clone(),
        adopt_managed_block: opts.adopt_managed_block.clone(),
        agents_snippet: opts.agents_snippet,
    };
    let plan = repoise_core::init::plan(&opts.root, &options).map_err(|err| err.to_string())?;
    let outcome = if opts.dry_run {
        None
    } else {
        Some(repoise_core::init::apply(&plan).map_err(|err| err.to_string())?)
    };
    if opts.json {
        let files: Vec<serde_json::Value> = plan
            .files
            .iter()
            .map(|file| {
                serde_json::json!({
                    "path": file.relative.to_string_lossy(),
                    "action": format!("{:?}", file.action).to_lowercase(),
                })
            })
            .collect();
        let value = serde_json::json!({
            "root": plan.root.to_string_lossy(),
            "dry_run": opts.dry_run,
            "files": files,
            "manifest": plan.manifest,
        });
        println!("{}", json(&value)?);
        return Ok(ExitCode::SUCCESS);
    }
    print_init(&plan, outcome.as_ref(), opts.dry_run);
    if let Some(outcome) = &outcome
        && !outcome.conflicts.is_empty()
    {
        return Err("init left conflicts; review and re-run".to_string());
    }
    if !opts.dry_run {
        println!("next: run `repoise doctor` to verify effective settings");
    }
    Ok(ExitCode::SUCCESS)
}

fn run_related(opts: &CliOptions) -> Result<ExitCode, String> {
    let source_id = opts
        .source_id
        .as_deref()
        .ok_or("--source-id <ID> is required for related")?
        .to_string();
    let kinds: Vec<repoise_core::related::RelationKind> = opts
        .relations
        .iter()
        .map(|kind| {
            repoise_core::related::RelationKind::parse(kind).ok_or_else(|| {
                format!(
                    "unknown relation kind: {kind} (expected references, referenced-by, children, or parent)"
                )
            })
        })
        .collect::<Result<_, String>>()?;
    let ctx = build_context(opts)?;
    let store = Store::new(ctx.cache.db_path(&ctx.repo_id, &ctx.worktree_id));
    let request = repoise_core::related::RelatedRequest {
        source_id,
        kinds,
        limit: opts.max_results,
        max_output_tokens: opts.max_output_tokens,
    };
    let response =
        match repoise_core::related::related(ctx.adapter.as_ref(), ctx.mode, &store, &request) {
            Ok(response) => response,
            Err(Error::IndexState(message)) if message.starts_with("no published index") => {
                return Ok(ExitCode::from(3));
            }
            Err(err) => return Err(err.to_string()),
        };
    if opts.json {
        println!("{}", json(&response)?);
    } else {
        println!(
            "{} related source(s), generation {}",
            response.results.len(),
            response.generation_id
        );
        for related in &response.results {
            println!(
                "{}: {} ({}; L{}-L{}{})",
                related.path,
                related.title,
                related.relation,
                related.line_start,
                related.line_end,
                related
                    .edge_kind
                    .as_deref()
                    .map(|kind| {
                        let name = related
                            .name
                            .as_deref()
                            .map(|name| format!(" {name}"))
                            .unwrap_or_default();
                        format!("; {kind}{name}")
                    })
                    .unwrap_or_default(),
            );
            println!("  source-id: {}", related.source_id);
            if !related.excerpt.is_empty() {
                println!("  {}", related.excerpt);
            }
            if let Some(url) = &related.url {
                println!("  url: {url}");
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_check(opts: &CliOptions) -> Result<ExitCode, String> {
    let ctx = build_context(opts)?;
    let store = Store::new(ctx.cache.db_path(&ctx.repo_id, &ctx.worktree_id));
    let options = repoise_core::check::CheckOptions {
        require_fresh: opts.fresh,
    };
    let result = repoise_core::check::check(
        ctx.adapter.as_ref(),
        ctx.mode,
        &ctx.effective,
        ctx.config_file.as_deref(),
        &store,
        &ctx.cache,
        &options,
    )
    .map_err(|err| err.to_string())?;
    if opts.json {
        println!("{}", json(&result)?);
    } else {
        println!("check: {:?}", result.category);
        println!("fresh: {}", result.fresh);
        println!("coverage met: {}", result.coverage_met);
        for reason in &result.reasons {
            println!("  - {reason}");
        }
    }
    let code = match result.category {
        repoise_core::check::CheckCategory::Ok => ExitCode::SUCCESS,
        repoise_core::check::CheckCategory::Missing | repoise_core::check::CheckCategory::Stale => {
            ExitCode::from(3)
        }
        repoise_core::check::CheckCategory::CoverageNotMet => ExitCode::from(4),
    };
    Ok(code)
}

fn run_watch(opts: &CliOptions) -> Result<ExitCode, String> {
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let ctx = build_context(opts)?;
    let db_path = ctx.cache.db_path(&ctx.repo_id, &ctx.worktree_id);
    let scope_dir = db_path
        .parent()
        .ok_or_else(|| "internal error: scope directory has no parent".to_string())?;
    let root = ctx
        .adapter
        .canonical_root()
        .map_err(|err| err.to_string())?;
    let scope = events::Scope {
        root: root.to_string_lossy().to_string(),
        repo_id: ctx.repo_id.clone(),
        worktree_id: ctx.worktree_id.clone(),
        snapshot_mode: events::scope_mode_name(ctx.mode).to_string(),
    };
    // Authoritative one-writer-per-scope lease for the watch scope, held for
    // the watch process lifetime. Resolved before any store access or
    // provider work: a refused writer must not open or migrate the owner's
    // database. The descriptive owner marker is refreshed every loop
    // iteration so an idle watch does not look stale.
    let lock_result = repoise_core::ownership::ScopeLock::acquire(scope_dir, "watch");
    // The cooperative SIGINT/SIGTERM handling must be installed before the
    // first `started` event is emitted, so a signal sent as soon as the
    // stream starts cancels safely instead of killing the process.
    let embed_cancel = CancellationToken::new();
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let cancel_handler = cancel.clone();
    let cancel_token = embed_cancel.clone();
    if let Err(err) = ctrlc::set_handler(move || {
        cancel_handler.store(true, Ordering::SeqCst);
        // Stop embedding work inside an in-flight publication as well.
        cancel_token.cancel();
    }) {
        // Ctrl+C everywhere; with the ctrlc `termination` feature also
        // SIGTERM/SIGHUP on Unix and CTRL_CLOSE_EVENT on Windows.
        return Err(format!("could not install the signal handler: {err}"));
    }
    // The event stream (events mode only) is started before the refusal so
    // the stream carries `started` plus the terminal `failed` event.
    let stream: Option<events::EventStream<std::io::Stdout>> = if opts.events {
        let stream = events::EventStream::stdout("watch");
        stream.start(&scope);
        stream.start_heartbeat(events::heartbeat_interval());
        Some(stream)
    } else {
        None
    };
    let owner_guard = match lock_result {
        Err(err) => {
            // A refused writer stops before any store access or provider
            // work: the terminal event carries the stable machine reason and
            // retryability.
            let (reason, retryable) = repoise_core::ownership::failure_reason(&err);
            if let Some(stream) = &stream {
                stream.terminal(
                    "failed",
                    json!({
                        "operation": "watch",
                        "scope": serde_json::to_value(&scope).unwrap_or_default(),
                        "code": "operational",
                        "reason": reason,
                        "message": err.to_string(),
                        "retryable": retryable,
                    }),
                );
            }
            return Err(err.to_string());
        }
        Ok(guard) => {
            for warning in &guard.warnings {
                if let Some(stream) = &stream {
                    stream.emit(
                        "warning",
                        json!({ "operation": "watch", "message": warning }),
                    );
                } else {
                    eprintln!("warning: {warning}");
                }
            }
            guard
        }
    };
    let store = Store::new(ctx.cache.db_path(&ctx.repo_id, &ctx.worktree_id));
    let options = repoise_core::watch::WatchOptions {
        interval: Duration::from_millis(
            opts.watch_interval_ms
                .unwrap_or(repoise_core::watch::DEFAULT_INTERVAL.as_millis() as u64),
        ),
        debounce: Duration::from_millis(
            opts.watch_debounce_ms
                .unwrap_or(repoise_core::watch::DEFAULT_DEBOUNCE.as_millis() as u64),
        ),
        max_pending_paths: opts
            .watch_max_pending
            .unwrap_or(repoise_core::watch::DEFAULT_MAX_PENDING_PATHS as u64)
            as usize,
        config_warning_interval: repoise_core::watch::DEFAULT_CONFIG_WARNING_INTERVAL,
    };
    // Build the configured embedding session once for the watch process;
    // every debounced publication runs it through the shared index path.
    // Missing configuration (or this build lacking the transport) keeps
    // watch publications lexical-only.
    let client = embedding_client(&ctx.effective, embed_cancel.clone())?;
    let embed_configured = client.is_some();
    // The resolved committed/local config sources for live reload: the
    // explicit CLI paths when given, otherwise the repository defaults.
    // These control files are monitored even when the policy excludes them.
    let cli_overrides = cli_overrides(opts)?;
    let config_sources = repoise_core::watch::ConfigSources {
        committed: opts
            .config
            .clone()
            .unwrap_or_else(|| root.join(CONFIG_FILENAME)),
        local: opts
            .local_config
            .clone()
            .unwrap_or_else(|| root.join(LOCAL_CONFIG_FILENAME)),
        cli: cli_overrides,
    };
    // When the effective embedding settings move, the watcher rebuilds the
    // session through the shared helper; a failure is reported sanitized
    // (no endpoint/credential bytes in the warning).
    let factory_cancel = embed_cancel.clone();
    let client_factory: Box<repoise_core::watch::EmbeddingClientFactory> = Box::new(move |eff| {
        embedding_client(eff, factory_cancel.clone()).map_err(|_| {
            repoise_core::error::Error::Config(
                "the embedding client could not be built".to_string(),
            )
        })
    });
    let mut watcher = repoise_core::watch::Watcher::new(
        ctx.adapter,
        ctx.mode,
        ctx.effective,
        store,
        ctx.cache,
        options,
        client,
    )
    .map_err(|err| err.to_string())?
    .with_config_sources(config_sources, Some(client_factory));
    let summary = format!(
        "watching {}/{} (interval {} ms, debounce {} ms; embeddings {})",
        ctx.repo_id,
        ctx.worktree_id,
        options.interval.as_millis(),
        options.debounce.as_millis(),
        if embed_configured {
            "configured"
        } else {
            "lexical-only"
        }
    );
    if opts.events {
        let stream = stream.expect("the event stream was created in events mode");
        eprintln!("{summary}; SIGINT/SIGTERM cancels");
        let mut last_generation: Option<i64> = None;
        loop {
            match watcher.step(&cancel) {
                Ok(repoise_core::watch::WatchStep::Idle) => {}
                Ok(repoise_core::watch::WatchStep::Published(outcome)) => {
                    last_generation = Some(outcome.generation_id);
                    owner_guard.refresh();
                    stream.emit(
                        "progress",
                        json!({
                            "operation": "watch",
                            "phase": "publication",
                            "generationId": outcome.generation_id,
                            "changedPaths": outcome.changed_paths,
                            "filesReparsed": outcome.files_reparsed,
                            "forceFull": outcome.force_full,
                            "branchSwitch": outcome.branch_switch,
                            "configChange": outcome.config_change,
                            "vectorsTotal": outcome.vectors_total,
                            "embedding": serde_json::to_value(&outcome.embedding)
                                .unwrap_or_default(),
                            "durationMs": outcome.duration_ms,
                        }),
                    );
                }
                Ok(repoise_core::watch::WatchStep::ConfigIssue(issue)) => {
                    // Rejected configuration revision: throttled, sanitized;
                    // the last published generation stays served.
                    eprintln!("warning: {}", issue.message);
                    stream.emit(
                        "warning",
                        json!({
                            "operation": "watch",
                            "kind": issue.kind,
                            "message": issue.message,
                        }),
                    );
                }
                Ok(repoise_core::watch::WatchStep::Cancelled) => {}
                Err(err) => {
                    eprintln!("watch step failed: {err}");
                    stream.emit(
                        "warning",
                        json!({
                            "operation": "watch",
                            "message": format!("watch step failed: {err}"),
                        }),
                    );
                }
            }
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            owner_guard.refresh();
            std::thread::sleep(options.interval);
        }
        stream.terminal(
            "cancelled",
            json!({
                "operation": "watch",
                "scope": serde_json::to_value(&scope).unwrap_or_default(),
                "reason": "signal",
                "lastGeneration": last_generation,
            }),
        );
        eprintln!(
            "watch run {} stopped (safe cancel; last complete generation retained)",
            stream.run_id()
        );
        return Ok(ExitCode::SUCCESS);
    }
    println!("{summary}; Ctrl+C cancels");
    loop {
        match watcher.step(&cancel) {
            Ok(repoise_core::watch::WatchStep::Idle) => {}
            Ok(repoise_core::watch::WatchStep::Published(outcome)) => {
                if opts.json {
                    println!("{}", json(&outcome)?);
                } else {
                    println!(
                        "published generation {} ({} file(s) reparsed{}{}; {} ms)",
                        outcome.generation_id,
                        outcome.files_reparsed,
                        if outcome.force_full {
                            ", full reparse"
                        } else {
                            ""
                        },
                        if outcome.branch_switch {
                            ", branch switch"
                        } else {
                            ""
                        },
                        outcome.duration_ms
                    );
                    for path in &outcome.changed_paths {
                        println!("  changed: {path}");
                    }
                    if let Some(stats) = &outcome.embedding {
                        println!(
                            "vectors: {} ({} cached, {} embedded, {} failed, {} pending, {} requests)",
                            outcome.vectors_total,
                            stats.cache_hits,
                            stats.embedded,
                            stats.failed,
                            stats.pending,
                            stats.requests
                        );
                    } else {
                        println!(
                            "vectors: {} (lexical-only publication)",
                            outcome.vectors_total
                        );
                    }
                }
            }
            Ok(repoise_core::watch::WatchStep::ConfigIssue(issue)) => {
                // Rejected configuration revision: throttled, sanitized; the
                // last published generation stays served.
                eprintln!("warning: {}", issue.message);
            }
            Ok(repoise_core::watch::WatchStep::Cancelled) => break,
            Err(err) => {
                eprintln!("watch step failed: {err}");
            }
        }
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        owner_guard.refresh();
        std::thread::sleep(options.interval);
    }
    println!("watch stopped (safe cancel; last complete generation retained)");
    Ok(ExitCode::SUCCESS)
}

fn run_overlay(opts: &CliOptions) -> Result<ExitCode, String> {
    let root = std::fs::canonicalize(&opts.root).map_err(|err| err.to_string())?;
    match opts.overlay_action.as_deref() {
        Some("uninstall") => {
            let outcome = repoise_core::overlay::uninstall(&root, opts.dry_run)
                .map_err(|err| err.to_string())?;
            if opts.json {
                println!("{}", json(&outcome)?);
            } else {
                for path in &outcome.removed_files {
                    println!("removed file: {path}");
                }
                for path in &outcome.removed_blocks {
                    println!("removed block: {path}");
                }
                for conflict in &outcome.conflicts {
                    eprintln!("conflict: {}: {}", conflict.path, conflict.reason);
                }
                if outcome.manifest_removed {
                    println!("overlay manifest removed");
                } else if !outcome.dry_run {
                    println!("overlay manifest retained (conflicts or already removed)");
                }
                if outcome.dry_run {
                    println!("dry run: nothing written");
                }
            }
            if !outcome.conflicts.is_empty() {
                return Err("overlay uninstall left conflicts; review and re-run".to_string());
            }
            Ok(ExitCode::SUCCESS)
        }
        Some("update") => {
            let outcome = repoise_core::overlay::update(&root, opts.dry_run)
                .map_err(|err| err.to_string())?;
            if opts.json {
                println!("{}", json(&outcome)?);
            } else {
                for path in &outcome.updated {
                    println!("updated: {path}");
                }
                for path in &outcome.unchanged {
                    println!("unchanged: {path}");
                }
                for conflict in &outcome.conflicts {
                    eprintln!("conflict: {}: {}", conflict.path, conflict.reason);
                }
                if outcome.manifest_updated {
                    println!("overlay manifest re-pinned");
                }
                if outcome.dry_run {
                    println!("dry run: nothing written");
                }
            }
            if !outcome.conflicts.is_empty() {
                return Err("overlay update left conflicts; review and re-run".to_string());
            }
            Ok(ExitCode::SUCCESS)
        }
        _ => Err("overlay requires an action (uninstall or update)".to_string()),
    }
}

fn default_adapter(
    root: &std::path::Path,
) -> Result<Box<dyn repoise_core::adapter::SourceAdapter>, String> {
    let root = root.canonicalize().map_err(|err| err.to_string())?;
    if root.join(".git").exists() {
        Ok(Box::new(
            repoise_core::adapter::git::GitAdapter::new(&root).map_err(|err| err.to_string())?,
        ))
    } else {
        Ok(Box::new(
            repoise_core::adapter::filesystem::FilesystemAdapter::new(&root)
                .map_err(|err| err.to_string())?,
        ))
    }
}

fn adapter_mode(root: &std::path::Path, committed: bool) -> SnapshotMode {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if root.join(".git").exists() {
        if committed {
            SnapshotMode::Committed
        } else {
            SnapshotMode::WorkingTree
        }
    } else {
        SnapshotMode::PlainDirectory
    }
}

/// The declared scope plus the context shared by index/status/search/read.
struct Context {
    effective: EffectiveConfig,
    adapter: Box<dyn SourceAdapter>,
    mode: SnapshotMode,
    cache: CachePaths,
    config_file: Option<PathBuf>,
    repo_id: String,
    worktree_id: String,
}

fn resolve_cache_root(opts: &CliOptions) -> Result<CachePaths, String> {
    let root = std::fs::canonicalize(&opts.root).map_err(|err| err.to_string())?;
    let cli = cli_overrides(opts)?;
    let paths = config_paths(opts);
    let effective =
        EffectiveConfig::resolve_with(&root, &paths, &cli).map_err(|err| err.to_string())?;
    let env_override = env::var(CACHE_ENV_VAR).ok();
    CachePaths::resolve(&root, &effective.cache_dir, env_override.as_deref())
        .map_err(|err| err.to_string())
}

fn build_context(opts: &CliOptions) -> Result<Context, String> {
    let root = std::fs::canonicalize(&opts.root).map_err(|err| err.to_string())?;
    let cli = cli_overrides(opts)?;
    let paths = config_paths(opts);
    let effective =
        EffectiveConfig::resolve_with(&root, &paths, &cli).map_err(|err| err.to_string())?;
    let adapter = default_adapter(&root)?;
    let mode = adapter_mode(&root, opts.committed);
    let cache = resolve_cache_root(opts)?;
    let (repo_id, worktree_id, _) = repoise_core::search::scope_for_search(adapter.as_ref(), mode)
        .map_err(|err| err.to_string())?;
    let config_file = opts.config.clone().or_else(|| {
        let default = root.join(CONFIG_FILENAME);
        if default.exists() {
            Some(default)
        } else {
            None
        }
    });
    Ok(Context {
        effective,
        adapter,
        mode,
        cache,
        config_file,
        repo_id,
        worktree_id,
    })
}

fn run_search(opts: &CliOptions) -> Result<ExitCode, String> {
    let query = opts
        .query
        .as_deref()
        .ok_or("--query <Q> is required for search")?
        .to_string();
    let role_filter = match &opts.role_filter {
        Some(name) => Some(Role::parse(name).ok_or_else(|| format!("unknown role: {name}"))?),
        None => None,
    };
    let ctx = build_context(opts)?;
    let store = Store::new(ctx.cache.db_path(&ctx.repo_id, &ctx.worktree_id));
    if opts.lane.as_deref() == Some("history") {
        let request = repoise_core::history::HistorySearchRequest {
            query,
            path_filter: opts.path_filter.clone(),
            max_results: opts.max_results,
            cursor: opts.cursor.clone(),
        };
        let response = match repoise_core::history::search_history(
            ctx.adapter.as_ref(),
            ctx.mode,
            &store,
            &request,
        ) {
            Ok(response) => response,
            Err(Error::IndexState(message)) if message.starts_with("no published index") => {
                return Ok(ExitCode::from(3));
            }
            Err(err) => return Err(err.to_string()),
        };
        if opts.json {
            println!("{}", json(&response)?);
        } else {
            println!(
                "{} result(s), generation {} ({})",
                response.results.len(),
                response.generation_id,
                response.retrieval_mode
            );
            for hit in &response.results {
                let author = hit
                    .author
                    .as_deref()
                    .map(|author| format!(" (by {author})"))
                    .unwrap_or_default();
                println!("{}: {}{}", hit.revision_id, hit.message, author);
                if !hit.affected_paths.is_empty() {
                    println!("  paths: {}", hit.affected_paths.join(", "));
                }
                if let Some(association) = &hit.association {
                    println!(
                        "  pr #{} (verified, {}): {}",
                        association.number,
                        association.state.as_deref().unwrap_or("state unknown"),
                        association.title.as_deref().unwrap_or("(no title)")
                    );
                }
                if let Some(url) = &hit.url {
                    println!("  url: {url}");
                }
            }
            if response.truncated {
                println!(
                    "truncated; next: --cursor {}",
                    response.next_cursor.as_deref().unwrap_or("(no cursor)")
                );
            }
            // Lane coverage: agents must see gaps (horizon, shallow) and
            // enrichment stops even when the page is empty.
            if !response.lane.enabled {
                println!("lane: history off for this index");
            } else {
                println!(
                    "lane: history on ({} items, head {})",
                    response.lane.items,
                    response.lane.head_revision.as_deref().unwrap_or("(none)")
                );
                for gap in &response.lane.gaps {
                    println!("  gap: {gap}");
                }
                if let Some(summary) = &response.lane.enrichment {
                    println!(
                        "  enrichment: {} enriched, {} no-association, {} failed, {} unattempted",
                        summary.enriched,
                        summary.no_association,
                        summary.failed,
                        summary.unattempted
                    );
                    if summary.rate_limited {
                        println!("  rate_limited: true");
                    }
                    if summary.permission_denied {
                        println!("  permission_denied: true");
                    }
                    if summary.budget_exhausted {
                        println!("  budget_exhausted: true");
                    }
                }
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(other) = &opts.lane {
        return Err(format!(
            "unknown search lane: {other} (expected search or history)"
        ));
    }
    let mode = match opts.search_mode.as_deref() {
        None | Some("lexical") => SearchMode::Lexical,
        Some("hybrid") => SearchMode::Hybrid,
        Some("vectors-only") => SearchMode::VectorsOnly,
        Some(other) => {
            return Err(format!(
                "unknown search mode: {other} (expected lexical, hybrid, or vectors-only)"
            ));
        }
    };
    let request = repoise_core::search::SearchRequest {
        query,
        path_filter: opts.path_filter.clone(),
        role_filter,
        max_results: opts.max_results,
        max_output_tokens: opts.max_output_tokens,
        cursor: opts.cursor.clone(),
        mode,
        rrf_k: opts.rrf_k.or(Some(ctx.effective.rrf_k)),
    };
    let query_embedder = if mode == SearchMode::Lexical {
        None
    } else {
        query_embedder_from_config(&ctx.effective)?
    };
    let response = match repoise_core::search::search(
        ctx.adapter.as_ref(),
        ctx.mode,
        &store,
        &request,
        query_embedder.as_deref(),
    ) {
        Ok(response) => response,
        Err(Error::IndexState(message)) if message.starts_with("no published index") => {
            return Ok(ExitCode::from(3));
        }
        Err(err) => return Err(err.to_string()),
    };
    if opts.json {
        println!("{}", json(&response)?);
    } else {
        println!(
            "{} result(s), generation {} ({}; {})",
            response.results.len(),
            response.generation_id,
            response.retrieval_mode,
            response.coverage.vector
        );
        for hit in &response.results {
            println!(
                "{}: {} (L{}-L{}) [{}]",
                hit.path, hit.title, hit.line_start, hit.line_end, hit.role
            );
            println!("  {}", hit.excerpt);
            if let Some(url) = &hit.url {
                println!("  url: {url}");
            }
        }
        if let Some(paths) = &response.fallback {
            println!("no matches; nearby files:");
            for path in paths {
                println!("  {path}");
            }
        }
        if response.truncated {
            println!(
                "truncated; next: --cursor {}",
                response.next_cursor.as_deref().unwrap_or("(no cursor)")
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn run_read(opts: &CliOptions) -> Result<ExitCode, String> {
    if opts.lane.as_deref() == Some("history") {
        return Err(
            "read targets current chunks only; history items have no exact-read lane".into(),
        );
    }
    let source_id = opts
        .source_id
        .as_deref()
        .ok_or("--source-id <ID> is required for read")?
        .to_string();
    let ctx = build_context(opts)?;
    let store = Store::new(ctx.cache.db_path(&ctx.repo_id, &ctx.worktree_id));
    let request = repoise_core::read::ReadRequest {
        source_id,
        max_output_tokens: opts.max_output_tokens,
    };
    let result = match repoise_core::read::read(ctx.adapter.as_ref(), ctx.mode, &store, &request) {
        Ok(result) => result,
        Err(Error::IndexState(message)) if message.starts_with("no published index") => {
            return Ok(ExitCode::from(3));
        }
        Err(Error::Stale { path, reason, .. }) => {
            return Err(format!("stale source ({path}): {reason}"));
        }
        Err(err) => return Err(err.to_string()),
    };
    if opts.json {
        println!("{}", json(&result)?);
    } else {
        println!(
            "{} (L{}-L{})",
            result.path, result.line_start, result.line_end
        );
        println!("revision: {}", result.revision_hash);
        println!(
            "scope: {}/{}",
            result.scope.repo_id, result.scope.worktree_id
        );
        if let Some(url) = &result.url {
            println!("url: {url}");
        }
        println!("{}", result.text);
    }
    Ok(ExitCode::SUCCESS)
}

fn run_purge(opts: &CliOptions) -> Result<ExitCode, String> {
    let cache = resolve_cache_root(opts)?;
    let request = repoise_core::purge::PurgeRequest {
        all: opts.purge_all,
        repo_id: opts.repo_id.clone(),
        worktree_id: opts.worktree_id.clone(),
    };
    if !request.all && (request.repo_id.is_none() || request.worktree_id.is_none()) {
        return Err("purge requires --all or both --repo-id and --worktree-id".into());
    }
    let report = repoise_core::purge::purge(&cache, &request).map_err(|err| err.to_string())?;
    if !request.all {
        // Unregister the purged scope from the family member registry so
        // shared garbage collection keeps working for sibling worktrees.
        // The probe creates nothing; a failure here only warns (the scope
        // itself was purged) and never touches sibling data.
        let unregistration: Result<(), String> = (|| {
            let root = std::fs::canonicalize(&opts.root).map_err(|err| err.to_string())?;
            let cli = cli_overrides(opts)?;
            let paths = config_paths(opts);
            let effective =
                repoise_core::config::EffectiveConfig::resolve_with(&root, &paths, &cli)
                    .map_err(|err| err.to_string())?;
            let adapter = default_adapter(&root)?;
            let state = repoise_core::shared::resolve_sharing(adapter.as_ref(), &effective, &cache)
                .map_err(|err| err.to_string())?;
            if let repoise_core::shared::SharingState::Shared(store) = state {
                repoise_core::shared::remove_member(
                    &store.root,
                    request.repo_id.as_deref().ok_or("missing repo id")?,
                    request
                        .worktree_id
                        .as_deref()
                        .ok_or("missing worktree id")?,
                )
                .map_err(|err| err.to_string())?;
            }
            Ok(())
        })();
        if let Err(err) = unregistration {
            eprintln!("warning: could not remove family registration: {err}");
        }
    }
    if opts.json {
        println!("{}", json(&report)?);
    } else {
        for path in &report.removed {
            println!("removed: {}", path.display());
        }
        if let Some(skipped) = &report.skipped_owned {
            for scope in &skipped.owned {
                let detail = scope
                    .owner
                    .as_ref()
                    .map(|owner| {
                        format!(" (owner pid {}, operation {})", owner.pid, owner.operation)
                    })
                    .unwrap_or_else(|| " (owner metadata unavailable)".to_string());
                println!("skipped (owned): {}{detail}", scope.path.display());
            }
        }
        if report.removed.is_empty() && report.skipped_owned.is_none() {
            println!("nothing to remove");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Resolves the endpoint (an `env:` reference) and the API key (the named
/// environment variable) from embedding settings. Missing values degrade the
/// operation to lexical-only (with a note) rather than failing it.
#[cfg(feature = "remote-embedding")]
fn resolve_provider_credentials(
    settings: &repoise_core::config::EmbeddingConfig,
) -> Result<Option<(String, String)>, String> {
    let Some(endpoint_ref) = settings.endpoint.as_deref() else {
        return Ok(None);
    };
    let Some(var_name) = endpoint_ref.strip_prefix("env:") else {
        return Err("embedding.endpoint must be an env: reference".into());
    };
    let Some(endpoint) = env::var(var_name).ok() else {
        eprintln!("note: endpoint variable {var_name} is not set; continuing lexical-only");
        return Ok(None);
    };
    let Some(key_env) = settings.api_key_env.as_deref() else {
        eprintln!("note: no API key variable configured; continuing lexical-only");
        return Ok(None);
    };
    let Some(api_key) = env::var(key_env).ok() else {
        eprintln!("note: API key variable {key_env} is not set; continuing lexical-only");
        return Ok(None);
    };
    Ok(Some((endpoint, api_key)))
}

/// Builds the optional embedding session for a build, from effective config.
/// A missing profile, endpoint, or credential keeps the build lexical-only.
/// The caller supplies the cancellation token (watch cancels it on SIGINT so
/// an in-flight publication stops embedding; one-shot builds never cancel).
fn embedding_client(
    eff: &EffectiveConfig,
    cancellation: CancellationToken,
) -> Result<Option<EmbeddingClient>, String> {
    let Some(settings) = &eff.embedding else {
        return Ok(None);
    };
    let Some(profile) = repoise_core::config::profile_from_config(settings) else {
        return Ok(None);
    };
    #[cfg(feature = "remote-embedding")]
    {
        let Some((endpoint, api_key)) = resolve_provider_credentials(settings)? else {
            return Ok(None);
        };
        let budgets = repoise_core::config::budgets_from_config(settings);
        let provider = provider::OpenAiCompatibleProvider::new(
            &endpoint,
            &api_key,
            &profile.model,
            profile.dimension,
            profile.input_profile,
            budgets.timeout,
        )
        .map_err(|err| err.to_string())?;
        Ok(Some(EmbeddingClient::new(
            Box::new(provider),
            budgets,
            cancellation,
        )))
    }
    #[cfg(not(feature = "remote-embedding"))]
    {
        let _ = profile;
        let _ = cancellation;
        eprintln!("note: this binary has no remote embedding transport; continuing lexical-only");
        Ok(None)
    }
}

/// Wraps a provider as a query-time embedder (one provider batch per call).
#[cfg(feature = "remote-embedding")]
struct ProviderQueryEmbedder {
    provider: std::sync::Arc<dyn repoise_core::embed::EmbeddingProvider>,
    profile: repoise_core::embed::EmbeddingProfile,
}

#[cfg(feature = "remote-embedding")]
impl QueryEmbedder for ProviderQueryEmbedder {
    fn embed_queries(&self, inputs: &[String]) -> repoise_core::Result<Vec<Option<Vec<f32>>>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let batch = self.provider.embed(inputs)?;
        let dimension = self.profile.dimension;
        Ok(batch
            .vectors
            .into_iter()
            .map(|vector| {
                if repoise_core::embed::validate_vector(&vector, dimension) {
                    Some(vector)
                } else {
                    None
                }
            })
            .collect())
    }

    fn profile_fingerprint(&self) -> String {
        self.profile.fingerprint()
    }

    fn input_profile(&self) -> repoise_core::embed::InputProfile {
        self.profile.input_profile
    }
}

/// Builds the optional query-time embedder for hybrid retrieval. Missing
/// profile, endpoint, or credential degrades hybrid search to lexical.
fn query_embedder_from_config(
    eff: &EffectiveConfig,
) -> Result<Option<Box<dyn QueryEmbedder>>, String> {
    let Some(settings) = &eff.embedding else {
        return Ok(None);
    };
    let Some(profile) = repoise_core::config::profile_from_config(settings) else {
        return Ok(None);
    };
    #[cfg(feature = "remote-embedding")]
    {
        let Some((endpoint, api_key)) = resolve_provider_credentials(settings)? else {
            return Ok(None);
        };
        let budgets = repoise_core::config::budgets_from_config(settings);
        let provider = provider::OpenAiCompatibleProvider::new(
            &endpoint,
            &api_key,
            &profile.model,
            profile.dimension,
            profile.input_profile,
            budgets.timeout,
        )
        .map_err(|err| err.to_string())?;
        Ok(Some(Box::new(ProviderQueryEmbedder {
            provider: std::sync::Arc::new(provider),
            profile: profile.clone(),
        })))
    }
    #[cfg(not(feature = "remote-embedding"))]
    {
        let _ = profile;
        Ok(None)
    }
}
