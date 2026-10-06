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
use repoise_core::error::Error;
use repoise_core::store::Store;
use repoise_core::{CACHE_ENV_VAR, CONFIG_FILENAME};

mod output;
use output::{json, print_doctor, print_explain, print_init};

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
    Purge,
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
    purge_all: bool,
    repo_id: Option<String>,
    worktree_id: Option<String>,
}

const USAGE: &str = "\
Repoise - offline repository knowledge indexer

USAGE:
    repoise <COMMAND> [OPTIONS]

COMMANDS:
    greet                         Print the project greeting
    doctor [ROOT]                 Show effective settings, capabilities, policy
    explain --path <REL> [ROOT]   Explain the include/exclude decision for a path
    index [ROOT]                  Build and publish the offline index (incremental)
    status [ROOT]                 Show scope, snapshot, index and freshness
    search --query <Q> [ROOT]     Offline lexical search over the current index
    read --source-id <ID> [ROOT]  Exact read-back of a search result
    init [ROOT]                   Configure the repository (non-interactive)
    purge                         Remove generated cache data (--all or one scope)
    help                          Show this help
    version                       Print version information

GLOBAL OPTIONS:
    --json                      Emit machine-readable JSON
    --config <PATH>             Override the committed config path
    --local-config <PATH>       Override the local override path
    --max-file-bytes <N>        Override the max accepted file size
    --committed                 Use the committed Git snapshot (not the working tree)

SEARCH OPTIONS:
    --path-filter <GLOB>        Filter results by repository-relative path
    --role <ROLE>               Filter results by classification role
    --max-results <N>           Page size (default 5, cap 20)
    --max-output-tokens <N>     Output token budget (estimated)
    --cursor <TOKEN>            Opaque pagination token from a previous page

READ OPTIONS:
    --source-id <ID>            Opaque source id from a search result

INIT OPTIONS:
    --preset <PRESET>           docs-only | docs-code-lexical | hybrid
    --provider <NAME>           Embedding provider (required for hybrid)
    --adopt-managed-block <FILE>  Append the managed marker block to FILE
    --dry-run                   Report planned changes without writing
    --yes                       Confirm (never prompts; documents intent)

PURGE OPTIONS:
    --all                       Remove the entire cache root
    --repo-id <ID> --worktree-id <ID>   Remove one scope directory
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
        purge_all: false,
        repo_id: None,
        worktree_id: None,
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
            "purge" => opts.command = Command::Purge,
            "--json" => opts.json = true,
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
            "--all" => opts.purge_all = true,
            "--repo-id" => {
                i += 1;
                opts.repo_id = Some(require_value(args, i, "--repo-id")?);
            }
            "--worktree-id" => {
                i += 1;
                opts.worktree_id = Some(require_value(args, i, "--worktree-id")?);
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
            | Command::Read => opts.root = PathBuf::from(first),
            Command::Greet | Command::Help | Command::Version | Command::Purge => {
                return Err(format!("unexpected argument: {first}"));
            }
        }
        if positional.len() > 1 {
            return Err(format!("unexpected argument: {}", positional[1]));
        }
    }
    Ok(opts)
}

fn require_value(args: &[String], index: usize, flag: &str) -> Result<String, String> {
    args.get(index)
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
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
        Command::Purge => run_purge(&opts),
    }
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
    let report =
        repoise_core::doctor::doctor(&opts.root, &cli, &paths).map_err(|err| err.to_string())?;
    if opts.json {
        println!("{}", json(&report)?);
    } else {
        print_doctor(&report);
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
    let store = Store::new(ctx.cache.db_path(&ctx.repo_id, &ctx.worktree_id));
    let request = repoise_core::indexing::IndexRequest::default();
    let outcome = repoise_core::indexing::index(
        ctx.adapter.as_ref(),
        ctx.mode,
        &ctx.effective,
        &store,
        &ctx.cache,
        &request,
    )
    .map_err(|err| err.to_string())?;
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
    }
    Ok(ExitCode::SUCCESS)
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
            Some(index) => println!(
                "index: generation {} ({} files, {} chunks, built {})",
                index.generation_id, index.files, index.chunks, index.built_at_ms
            ),
            None => println!("index: (none)"),
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
    let request = repoise_core::search::SearchRequest {
        query,
        path_filter: opts.path_filter.clone(),
        role_filter,
        max_results: opts.max_results,
        max_output_tokens: opts.max_output_tokens,
        cursor: opts.cursor.clone(),
    };
    let response =
        match repoise_core::search::search(ctx.adapter.as_ref(), ctx.mode, &store, &request) {
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
            "{} result(s), generation {}",
            response.results.len(),
            response.generation_id
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
    let source_id = opts
        .source_id
        .as_deref()
        .ok_or("--source-id <ID> is required for read")?
        .to_string();
    let ctx = build_context(opts)?;
    let store = Store::new(ctx.cache.db_path(&ctx.repo_id, &ctx.worktree_id));
    let request = repoise_core::read::ReadRequest { source_id };
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
    if opts.json {
        println!("{}", json(&report)?);
    } else {
        for path in &report.removed {
            println!("removed: {}", path.display());
        }
        if report.removed.is_empty() {
            println!("nothing to remove");
        }
    }
    Ok(ExitCode::SUCCESS)
}
