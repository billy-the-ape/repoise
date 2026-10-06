//! Native Repoise CLI.
//!
//! Thin adapter over shared engine services (repoise-core). The CLI never
//! prompts and is safe to run from agents. Exit codes: 0 success, 1
//! operational failure (with a message on stderr), 2 usage error.

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use repoise_core::NAME;
use repoise_core::config::{CliOverrides, ConfigPaths, Preset};

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
}

const USAGE: &str = "\
Repoise - offline repository knowledge indexer

USAGE:
    repoise <COMMAND> [OPTIONS]

COMMANDS:
    greet                       Print the project greeting
    doctor [ROOT]               Show effective settings, capabilities, policy
    explain --path <REL> [ROOT] Explain the include/exclude decision for a path
    index [ROOT]                Build the snapshot manifest and file inventory
    init [ROOT]                 Configure the repository (non-interactive)
    help                        Show this help
    version                     Print version information

GLOBAL OPTIONS:
    --json                      Emit machine-readable JSON
    --config <PATH>             Override the committed config path
    --local-config <PATH>       Override the local override path
    --max-file-bytes <N>        Override the max accepted file size

INIT OPTIONS:
    --preset <PRESET>           docs-only | docs-code-lexical | hybrid
    --provider <NAME>           Embedding provider (required for hybrid)
    --adopt-managed-block <FILE>  Append the managed marker block to FILE
    --dry-run                   Report planned changes without writing
    --yes                       Confirm (never prompts; documents intent)
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
            "--json" => opts.json = true,
            "--dry-run" => opts.dry_run = true,
            "--yes" | "-y" => opts.yes = true,
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
            value if value.starts_with('-') && value.len() > 1 => {
                return Err(format!("unknown option: {value}"));
            }
            value => positional.push(value.to_string()),
        }
        i += 1;
    }
    if let Some(first) = positional.first() {
        match opts.command {
            Command::Doctor | Command::Explain | Command::Index | Command::Init => {
                opts.root = PathBuf::from(first)
            }
            Command::Greet | Command::Help | Command::Version => {
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
            let _ = writeln!(io::stderr().lock(), "error: {message}");
            let _ = write!(io::stderr().lock(), "\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let code = match run(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            let _ = writeln!(io::stderr().lock(), "error: {message}");
            ExitCode::from(1)
        }
    };
    let _ = io::stdout().flush();
    code
}

fn run(opts: CliOptions) -> Result<(), String> {
    match opts.command {
        Command::Greet => {
            println!("{NAME} says hello");
            Ok(())
        }
        Command::Help => {
            print!("{USAGE}");
            Ok(())
        }
        Command::Version => {
            println!("{NAME} {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Doctor => run_doctor(&opts),
        Command::Explain => run_explain(&opts),
        Command::Index => run_index(&opts),
        Command::Init => run_init(&opts),
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

fn run_doctor(opts: &CliOptions) -> Result<(), String> {
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
    Ok(())
}

fn run_explain(opts: &CliOptions) -> Result<(), String> {
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
        adapter_mode(&opts.root),
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
    Ok(())
}

fn run_index(opts: &CliOptions) -> Result<(), String> {
    let cli = cli_overrides(opts)?;
    let paths = config_paths(opts);
    let effective = repoise_core::config::EffectiveConfig::resolve_with(&opts.root, &paths, &cli)
        .map_err(|err| err.to_string())?;
    let adapter = default_adapter(&opts.root).map_err(|err| err.to_string())?;
    let mode = adapter_mode(&opts.root);
    let inventory = repoise_core::discovery::inventory(adapter.as_ref(), mode, &effective)
        .map_err(|err| err.to_string())?;
    if opts.json {
        println!("{}", json(&inventory)?);
    } else {
        println!("mode: {:?}", mode);
        println!(
            "revision: {}",
            inventory
                .snapshot
                .revision_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "(none)".into())
        );
        println!("manifest: {}", inventory.manifest.manifest_hash);
        println!("files: {}", inventory.files.len());
        println!("skips: {}", inventory.skips.len());
        for skip in &inventory.skips {
            println!("  skip: {} ({:?})", skip.path.display(), skip.reason);
        }
        println!(
            "dirty overlay: {}",
            inventory
                .snapshot
                .dirty_overlay_digest
                .as_deref()
                .unwrap_or("none (committed snapshot)")
        );
    }
    Ok(())
}

fn run_init(opts: &CliOptions) -> Result<(), String> {
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
        return Ok(());
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
    Ok(())
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

fn adapter_mode(root: &std::path::Path) -> repoise_core::adapter::SnapshotMode {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if root.join(".git").exists() {
        repoise_core::adapter::SnapshotMode::WorkingTree
    } else {
        repoise_core::adapter::SnapshotMode::PlainDirectory
    }
}
