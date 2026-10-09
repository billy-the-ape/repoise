//! Human-readable and JSON output for CLI commands.

use repoise_core::doctor::DoctorReport;
use repoise_core::ignore::{Decision, MatchedRule};
use repoise_core::init::{FileAction, InitOutcome, InitPlan};

/// Pretty-serializes any serializable value.
pub fn json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string_pretty(value).map_err(|err| err.to_string())
}

/// Prints the doctor report for humans.
pub fn print_doctor(report: &DoctorReport) {
    let eff = &report.effective;
    println!("root: {}", report.root.display());
    println!("adapter: {:?}", report.adapter);
    println!(
        "capabilities: {}",
        report
            .capabilities
            .iter()
            .map(|cap| format!("{cap:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("mode: {:?}", report.mode);
    println!(
        "remote: {}",
        report.remote_identity.as_deref().unwrap_or("(none)")
    );
    println!(
        "config: {}",
        if report.config_file.is_some() {
            "repoise.config.json (present)"
        } else {
            "repoise.config.json (absent)"
        }
    );
    println!(
        "local: {}",
        if report.local_config_file.is_some() {
            "repoise.local.json (present)"
        } else {
            "repoise.local.json (absent)"
        }
    );
    println!("preset: {} ({:?})", eff.preset.name(), eff.preset_origin);
    println!(
        "git ignores: {}",
        if eff.inherit_git_ignores {
            "inherited"
        } else {
            "not inherited"
        }
    );
    println!("max file bytes: {}", eff.max_file_bytes);
    println!(
        "rules: package-default={} secret={} config={} native={}",
        report.policy.package_default_rules,
        report.policy.secret_rules,
        report.policy.config_rules,
        report.policy.native_rules
    );
    for source in &report.policy.native_ignore_sources {
        println!("  native: {}", source.display());
    }
    println!("policy fingerprint: {}", report.policy.fingerprint);
    println!("config fingerprint: {}", report.config_fingerprint);
}

/// Prints one explain decision for humans.
pub fn print_explain(path: &str, decision: &Decision) {
    println!("path: {path}");
    println!(
        "decision: {}",
        if decision.included {
            "included"
        } else {
            "excluded"
        }
    );
    match &decision.winning_rule {
        Some(rule) => println!("winning rule: {}", describe_rule(rule)),
        None => println!("winning rule: (default include)"),
    }
    if !decision.matched.is_empty() {
        println!("matched:");
        for rule in &decision.matched {
            println!("  - {}", describe_rule(rule));
        }
    }
}

fn describe_rule(rule: &MatchedRule) -> String {
    format!("{} {} {}", rule.source, rule.kind, rule.pattern)
}

/// Prints the init plan for humans (conflicts are shown per file).
pub fn print_init(plan: &InitPlan, _outcome: Option<&InitOutcome>, dry_run: bool) {
    if dry_run {
        println!("dry run: no changes written");
    }
    for file in &plan.files {
        let verb = match file.action {
            FileAction::Create if dry_run => "would create",
            FileAction::Create => "created",
            FileAction::Unchanged => "unchanged",
            FileAction::Upgrade if dry_run => "would upgrade (not modified)",
            FileAction::Upgrade => "upgraded",
            FileAction::Conflict if dry_run => "would conflict (not modified)",
            FileAction::Conflict => "conflict (not modified)",
        };
        println!("{verb}: {}", file.relative.display());
    }
}
