//! Code corpus chunking (master plan section 5, card K4).
//!
//! TS/TSX and JS/JSX files are parsed structurally with pinned
//! `tree-sitter` grammars ([`super::ts`]); every other code language uses a
//! deterministic line-window fallback (`code-line`). Grammar parser errors
//! are recorded and the affected top-level ranges fall back to line windows
//! instead of dropping the file. All units carry exact 1-based source line
//! ranges: chunk text is always the exact source lines for that range (no
//! synthetic lines), so exact read-back validation holds for code chunks.
//!
//! Symbol and reference extraction is syntactic: symbols are defined
//! declarations with enclosing-scope parent links, and references are
//! unqualified local hints (imports, calls, identifiers). The indexer
//! resolves them against the generation's symbol table and labels edges
//! that are not syntactically certain as uncertain.

use serde::{Deserialize, Serialize};

use super::{MAX_CHUNK_TOKENS, PARSER_VERSION_CODE_LINE, join_lines, split_by_lines};

/// The kind of symbol a code symbol record describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymbolKind {
    /// Function declaration, generator, arrow/function expression binding.
    Function,
    /// Class method (including constructor, getters and setters).
    Method,
    /// Class or class expression binding.
    Class,
    /// Interface declaration.
    Interface,
    /// Enum declaration (including `const enum`).
    Enum,
    /// Type alias declaration.
    TypeAlias,
    /// Namespace declaration.
    Namespace,
}

/// The kind of a syntactic reference edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefKind {
    /// Binding imported from a module.
    Import,
    /// Name re-exported (with or without a module source).
    Reexport,
    /// Identifier used as a call callee.
    Call,
    /// Any other unqualified identifier reference.
    Reference,
}

/// Confidence of a reference edge (syntax never proves resolution).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefConfidence {
    /// Syntactically certain (a resolved import edge to a known file).
    Certain,
    /// A syntax-only hint (unresolved import, or a name match).
    Uncertain,
}

/// One code chunk with an exact source range.
///
/// `ordinal`/`parent` follow the same sibling/address scheme as doc
/// chunks; `context` carries the parent/signature context of a split of an
/// oversized unit (metadata only — never part of the chunk text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeUnit {
    /// Index among siblings.
    pub ordinal: u32,
    /// Parent chunk ordinal when this is a split of an oversized unit.
    pub parent: Option<u32>,
    /// Heading ancestry (outermost first); empty at module root.
    pub heading_path: Vec<String>,
    /// Exact source lines for the range (no synthetic lines).
    pub text: String,
    /// 1-based inclusive source line range.
    pub line_start: u32,
    pub line_end: u32,
    /// Primary defined symbol for lexical matching (empty for module/fallback).
    pub symbol: String,
    /// Parent/signature context for a split (e.g. `Widget > run (signature)`).
    pub context: Option<String>,
}

/// One defined symbol in a code file (opaque ids are assigned by the indexer).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeSymbol {
    /// Symbol name.
    pub name: String,
    /// Declaration kind.
    pub kind: SymbolKind,
    /// 1-based inclusive line range (including attached doc comments).
    pub line_start: u32,
    pub line_end: u32,
    /// Index into the file's symbol list of the enclosing symbol, if any.
    pub parent: Option<usize>,
    /// Module-level export.
    pub exported: bool,
}

/// One import binding: a locally bound name plus the module specifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeImport {
    /// Line of the import.
    pub line: u32,
    /// Locally bound name (alias, default or namespace name).
    pub local_name: String,
    /// Module specifier (quotes stripped).
    pub module: String,
}

/// One syntactic local reference hint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeRef {
    /// Line of the reference.
    pub line: u32,
    /// Unqualified referenced name.
    pub name: String,
    /// Edge kind.
    pub kind: RefKind,
}

/// The result of code parsing for one file (deterministic).
#[derive(Clone, Debug)]
pub struct CodeParse {
    /// Parser version recorded on file/chunk records.
    pub parser_version: &'static str,
    /// Chunks in document order.
    pub units: Vec<CodeUnit>,
    /// Defined symbols (in document order).
    pub symbols: Vec<CodeSymbol>,
    /// Import bindings.
    pub imports: Vec<CodeImport>,
    /// Local reference hints (deduplicated, bounded).
    pub refs: Vec<CodeRef>,
    /// Recorded parser-error line ranges (1-based inclusive).
    pub error_ranges: Vec<(u32, u32)>,
}

/// Parses one code file. TS/TSX and JS/JSX use their structural grammars;
/// every other language uses the line-window fallback.
pub fn parse_code(language: &str, text: &str) -> CodeParse {
    match language {
        "ts" | "tsx" => super::ts::parse(text, language == "tsx"),
        "js" | "jsx" | "mjs" | "cjs" => super::ts::parse_js(text),
        _ => line_window_fallback(text),
    }
}

/// Deterministic line-window fallback: windows of source lines capped at the
/// hard token ceiling. Chunks have no heading, symbol or context.
pub fn line_window_fallback(text: &str) -> CodeParse {
    let lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();
    let mut units = Vec::new();
    let mut running = 0usize;
    for part in split_by_lines(&lines, MAX_CHUNK_TOKENS) {
        let line_start = running as u32 + 1;
        let line_end = line_start + (part.len() - 1) as u32;
        running += part.len();
        units.push(CodeUnit {
            ordinal: units.len() as u32,
            parent: None,
            heading_path: Vec::new(),
            text: join_lines(&part),
            line_start,
            line_end,
            symbol: String::new(),
            context: None,
        });
    }
    CodeParse {
        parser_version: PARSER_VERSION_CODE_LINE,
        units,
        symbols: Vec::new(),
        imports: Vec::new(),
        refs: Vec::new(),
        error_ranges: Vec::new(),
    }
}

/// Reports the code-grammar capabilities for doctor/operator reports.
pub fn grammar_capabilities() -> Vec<String> {
    vec![
        "typescript 0.23.2 (.ts/.tsx structural, tree-sitter)".to_string(),
        "javascript 0.25.0 (.js/.jsx/.mjs/.cjs structural, tree-sitter)".to_string(),
        "line-window fallback (code-line) for all other code languages".to_string(),
    ]
}
