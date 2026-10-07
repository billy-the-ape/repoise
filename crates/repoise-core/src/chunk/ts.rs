//! Structural TS/TSX/JS/JSX chunking over pinned tree-sitter grammars.
//!
//! Walks the concrete syntax tree (no query files): top-level declarations
//! become symbol chunks, classes extract methods, functions extract nested
//! named functions, and everything else (imports, bare statements, comments)
//! is grouped into module chunks. Oversized units split at statement/block
//! boundaries with a parent reference and a signature context. Parser-error
//! ranges are recorded and their top-level regions fall back to line windows.

use std::collections::HashSet;

use tree_sitter::{Node, Parser};
use tree_sitter_language::LanguageFn;

use super::code::{CodeImport, CodeParse, CodeRef, CodeSymbol, CodeUnit, RefKind, SymbolKind};
use super::{
    MAX_CHUNK_TOKENS, PARSER_VERSION_JS, PARSER_VERSION_TS, estimate_tokens, join_lines,
    split_by_lines,
};

/// Identifier-like node kinds that yield local reference hints.
const REF_NODE_KINDS: &[&str] = &[
    "identifier",
    "property_identifier",
    "shorthand_property_identifier",
    "private_property_identifier",
    "shorthand_property_identifier_pattern",
];

/// Names that must never surface as reference hints.
const STOP_WORDS: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "infer",
    "instanceof",
    "interface",
    "is",
    "keyof",
    "let",
    "new",
    "namespace",
    "never",
    "null",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "set",
    "static",
    "super",
    "switch",
    "throw",
    "this",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "yield",
    "any",
    "bigint",
    "boolean",
    "number",
    "object",
    "string",
    "symbol",
    "unknown",
];

/// Bounded reference-hint count per file.
const MAX_REFS_PER_FILE: usize = 512;

/// 1-based line table for one file.
struct Lines {
    /// Byte offset where each 1-based line starts.
    starts: Vec<u64>,
    /// Line contents without trailing `\r`.
    texts: Vec<String>,
}

impl Lines {
    fn new(text: &str) -> Self {
        let mut starts = vec![0u64];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                starts.push((offset + 1) as u64);
            }
        }
        let texts = text.lines().map(str::to_string).collect();
        Self { starts, texts }
    }

    /// 1-based line of a byte offset.
    fn line_of(&self, byte: u64) -> u32 {
        self.starts.partition_point(|start| *start <= byte) as u32
    }

    /// Exact text of 1-based inclusive lines `[start, end]`.
    fn span(&self, start: u32, end: u32) -> String {
        join_lines(&self.texts[(start - 1) as usize..end as usize])
    }

    fn count(&self) -> u32 {
        self.starts.len() as u32
    }
}

/// One parsed file in flight.
struct Builder<'t> {
    /// Source bytes for node text extraction.
    text: &'t str,
    lines: Lines,
    units: Vec<CodeUnit>,
    symbols: Vec<CodeSymbol>,
    imports: Vec<CodeImport>,
    refs: Vec<CodeRef>,
    seen_refs: HashSet<(u32, String, RefKind)>,
    error_ranges: Vec<(u32, u32)>,
    gaps: Vec<(u32, u32)>,
}

/// Parses TypeScript/TSX source structurally (fallback on parser failure).
pub fn parse(text: &str, tsx: bool) -> CodeParse {
    let language: LanguageFn = if tsx {
        tree_sitter_typescript::LANGUAGE_TSX
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT
    };
    structural(text, language, PARSER_VERSION_TS)
}

/// Parses JavaScript/JSX source structurally (JSX is native to the grammar).
pub fn parse_js(text: &str) -> CodeParse {
    structural(text, tree_sitter_javascript::LANGUAGE, PARSER_VERSION_JS)
}

/// Parses with one grammar; any parse failure falls back to line windows so
/// a file is never dropped.
fn structural(text: &str, language: LanguageFn, parser_version: &'static str) -> CodeParse {
    let mut parser = Parser::new();
    let tree = match parser.set_language(&language.into()) {
        Ok(()) => match parser.parse(text, None) {
            Some(tree) => tree,
            None => return super::code::line_window_fallback(text),
        },
        Err(_) => return super::code::line_window_fallback(text),
    };
    let root = tree.root_node();
    let mut builder = Builder {
        text,
        lines: Lines::new(text),
        units: Vec::new(),
        symbols: Vec::new(),
        imports: Vec::new(),
        refs: Vec::new(),
        seen_refs: HashSet::new(),
        error_ranges: Vec::new(),
        gaps: Vec::new(),
    };
    builder.collect_errors_from_children(root);
    builder.walk_program(root);
    builder.flush_gaps();
    builder.collect_refs(root);
    CodeParse {
        parser_version,
        units: builder.units,
        symbols: builder.symbols,
        imports: builder.imports,
        refs: builder.refs,
        error_ranges: builder.error_ranges,
    }
}

impl<'t> Builder<'t> {
    /// 1-based inclusive (start, end) line range of a node.
    fn node_range(&self, node: Node) -> (u32, u32) {
        (
            self.lines.line_of(node.start_byte() as u64),
            self.lines.line_of(node.end_byte() as u64),
        )
    }

    /// Exact source text of a node.
    fn text_of(&self, node: Node) -> String {
        self.text[node.byte_range()].to_string()
    }

    /// Emits one unit chunk and returns its ordinal.
    fn emit_unit(
        &mut self,
        parent: Option<u32>,
        heading: &[String],
        symbol: &str,
        context: Option<String>,
        start: u32,
        end: u32,
    ) -> u32 {
        if start > end || start == 0 || end > self.lines.count() {
            return u32::MAX;
        }
        let ordinal = self.units.len() as u32;
        self.units.push(CodeUnit {
            ordinal,
            parent,
            heading_path: heading.to_vec(),
            text: self.lines.span(start, end),
            line_start: start,
            line_end: end,
            symbol: symbol.to_string(),
            context,
        });
        ordinal
    }

    /// Records a defined symbol; returns its index in the file's symbol list.
    fn add_symbol(
        &mut self,
        name: String,
        kind: SymbolKind,
        start: u32,
        end: u32,
        parent: Option<usize>,
        exported: bool,
    ) -> usize {
        self.symbols.push(CodeSymbol {
            name,
            kind,
            line_start: start,
            line_end: end,
            parent,
            exported,
        });
        self.symbols.len() - 1
    }

    /// Records one bounded, deduplicated reference hint.
    fn add_ref(&mut self, line: u32, name: &str, kind: RefKind) {
        if STOP_WORDS.contains(&name) || name.is_empty() {
            return;
        }
        if self.refs.len() >= MAX_REFS_PER_FILE {
            return;
        }
        if self.seen_refs.insert((line, name.to_string(), kind)) {
            self.refs.push(CodeRef {
                line,
                name: name.to_string(),
                kind,
            });
        }
    }

    /// Adds a top-level gap range (import/bare-statement/comment lines).
    fn gap(&mut self, start: u32, end: u32) {
        if start <= end {
            self.gaps.push((start, end));
        }
    }

    /// Drops pending gap entries covered by an attached declaration range.
    fn claim_lines(&mut self, start: u32, end: u32) {
        self.gaps
            .retain(|(gap_start, gap_end)| *gap_end < start || *gap_start > end);
    }

    /// Emits pending gap ranges as module line-window chunks, in order.
    fn flush_gaps(&mut self) {
        if self.gaps.is_empty() {
            return;
        }
        let mut gaps = std::mem::take(&mut self.gaps);
        gaps.sort();
        for (start, end) in gaps {
            let lines = &self.lines.texts[(start - 1) as usize..end as usize];
            let mut running = 0usize;
            for part in split_by_lines(lines, MAX_CHUNK_TOKENS) {
                let part_start = running as u32 + 1;
                let part_end = part_start + (part.len() - 1) as u32;
                running += part.len();
                self.emit_unit(
                    None,
                    &[],
                    "",
                    None,
                    start + (part_start - 1),
                    start + (part_end - 1),
                );
            }
        }
    }

    /// Records maximal parser-error line ranges. The root is never an
    /// "error ancestor": its `has_error` only reflects descendants.
    fn collect_errors(&mut self, node: Node, root: Node) {
        let nested_error = node
            .parent()
            .is_some_and(|parent| parent != root && parent.has_error());
        if node.has_error() && !nested_error {
            let (start, end) = self.node_range(node);
            self.error_ranges.push((start, end));
        }
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                self.collect_errors(child, root);
            }
        }
    }

    /// Enters error collection at the program's children (never the root).
    fn collect_errors_from_children(&mut self, program: Node) {
        for i in 0..program.child_count() {
            if let Some(child) = program.child(i) {
                self.collect_errors(child, program);
            }
        }
    }

    /// Whether a node kind is a module-level declaration.
    fn is_declaration(kind: &str) -> bool {
        matches!(
            kind,
            "function_declaration"
                | "generator_function_declaration"
                | "class_declaration"
                | "abstract_class_declaration"
                | "interface_declaration"
                | "enum_declaration"
                | "type_alias_declaration"
                | "lexical_declaration"
                | "variable_declaration"
        )
    }

    /// Whether a name is a language keyword / type name that must not
    /// surface as a symbol or reference hint.
    fn is_stop_word(name: &str) -> bool {
        STOP_WORDS.contains(&name)
    }

    /// The declared name of a declaration node, if any.
    fn decl_name(&self, node: Node) -> Option<String> {
        let name = node
            .child_by_field_name("name")
            .or_else(|| {
                (0..node.child_count())
                    .filter_map(|i| node.child(i))
                    .find(|child| matches!(child.kind(), "identifier" | "type_identifier"))
            })
            .map(|node| self.text_of(node));
        name.filter(|name| !Self::is_stop_word(name))
    }

    /// Extends a declaration's start line back over contiguous leading
    /// comments and decorators (preceding siblings in the same parent).
    fn leading_start(&self, node: Node) -> u32 {
        let mut start = self.node_range(node).0;
        let Some(parent) = node.parent() else {
            return start;
        };
        let Some(index) = (0..parent.child_count()).find(|i| {
            parent
                .child(*i)
                .is_some_and(|child| child.byte_range() == node.byte_range())
        }) else {
            return start;
        };
        let mut i = index;
        while i > 0 {
            i -= 1;
            let Some(prev) = parent.child(i) else {
                break;
            };
            if prev.kind() == "comment" || prev.kind() == "decorator" {
                let (prev_start, prev_end) = self.node_range(prev);
                if prev_end + 1 == start {
                    start = prev_start;
                    continue;
                }
            }
            break;
        }
        start
    }

    /// Walks the program's top level, emitting units in document order.
    fn walk_program(&mut self, program: Node) {
        let count = program.child_count();
        for index in 0..count {
            let Some(node) = program.child(index) else {
                continue;
            };
            let (start, end) = self.node_range(node);
            match node.kind() {
                "import_statement" => {
                    self.collect_imports(node, start);
                    self.flush_gaps();
                    self.gap(start, end);
                }
                "export_statement" => {
                    self.handle_export(node, start, end);
                }
                "lexical_declaration" | "variable_declaration" => {
                    let attached = self.leading_start(node).min(start);
                    self.handle_variables(node, attached, end, false);
                    self.flush_gaps();
                }
                kind if Self::is_declaration(kind) => {
                    self.handle_declaration(node, kind, false, None);
                    self.flush_gaps();
                }
                "expression_statement" => {
                    // TS `namespace X { ... }` parses as an internal_module
                    // wrapped in an expression statement.
                    let module = (0..node.child_count())
                        .filter_map(|i| node.child(i))
                        .find(|child| child.kind() == "internal_module");
                    match module {
                        Some(module) => {
                            self.handle_declaration(module, "internal_module", false, None);
                            self.flush_gaps();
                        }
                        None => {
                            self.flush_gaps();
                            self.gap(start, end);
                        }
                    }
                }
                "comment" | "empty_statement" => {
                    self.flush_gaps();
                    self.gap(start, end);
                }
                "ERROR" => {
                    // Recorded above; the region is kept searchable as a
                    // line-window fallback instead of being dropped.
                    self.flush_gaps();
                    self.gap(start, end);
                }
                _ => {
                    self.flush_gaps();
                    self.gap(start, end);
                }
            }
        }
        self.flush_gaps();
    }

    /// Handles an `export` statement: exported declarations become symbol
    /// units; re-exports and `export default <expr>` contribute references
    /// and module lines.
    fn handle_export(&mut self, node: Node, start: u32, end: u32) {
        let declaration = (1..node.child_count())
            .filter_map(|i| node.child(i))
            .find(|child| Self::is_declaration(child.kind()) || child.kind() == "internal_module");
        match declaration {
            Some(decl) => {
                let decl_kind = decl.kind();
                let decl_end = self.node_range(decl).1;
                // Attach leading comments to the `export` statement itself.
                let outer_start = self.leading_start(node);
                self.claim_lines(outer_start, decl_end);
                self.flush_gaps();
                if decl_kind == "lexical_declaration" || decl_kind == "variable_declaration" {
                    self.handle_variables(decl, outer_start, decl_end, true);
                } else {
                    self.handle_declaration(decl, decl_kind, true, Some(outer_start));
                }
            }
            None => {
                self.collect_reexports(node, start);
                self.flush_gaps();
                self.gap(start, end);
            }
        }
    }

    /// Handles `const`/`let`/`var` statements: declarators bound to
    /// functions/classes become symbol units; everything else is module gap.
    fn handle_variables(&mut self, node: Node, start: u32, _end: u32, exported: bool) {
        let (stmt_start, stmt_end) = self.node_range(node);
        let unit_start_base = start.min(stmt_start);
        let mut extracted: Vec<(u32, u32)> = Vec::new();
        let mut first = true;
        for i in 0..node.child_count() {
            let Some(decl) = node.child(i) else {
                continue;
            };
            if decl.kind() != "variable_declarator" {
                continue;
            };
            let (decl_start, decl_end) = self.node_range(decl);
            let mut name = None;
            let mut value_kind = None;
            for j in 0..decl.child_count() {
                let Some(child) = decl.child(j) else {
                    continue;
                };
                match child.kind() {
                    "identifier" => name = Some(self.text_of(child)),
                    "arrow_function" | "function_expression" => {
                        value_kind = Some(SymbolKind::Function)
                    }
                    "class" => value_kind = Some(SymbolKind::Class),
                    _ => {}
                }
            }
            if let (Some(name), Some(kind)) = (name, value_kind) {
                let unit_start = if first { unit_start_base } else { decl_start };
                self.claim_lines(unit_start, decl_end);
                self.flush_gaps();
                let heading = vec![name.clone()];
                self.emit_unit(None, &heading, &name, None, unit_start, decl_end);
                self.add_symbol(name, kind, unit_start, decl_end, None, exported);
                extracted.push((unit_start, decl_end));
            }
            first = false;
        }
        // The statement's lines minus the extracted declarators stay in the
        // module gap (no source line is lost or double-covered).
        let mut cursor = stmt_start;
        for (span_start, span_end) in &extracted {
            if *span_start > cursor {
                self.gap(cursor, *span_start - 1);
            }
            cursor = (*span_end + 1).max(cursor);
        }
        if cursor <= stmt_end {
            self.gap(cursor, stmt_end);
        }
    }

    /// Handles a module-level declaration: a symbol chunk (or a split when
    /// oversized), plus method/nested-function child chunks when the parent
    /// fits in one chunk.
    fn handle_declaration(
        &mut self,
        node: Node,
        kind: &str,
        exported: bool,
        start_override: Option<u32>,
    ) {
        let Some(name) = self.decl_name(node) else {
            // Unnamed declaration (e.g. anonymous `export default class`):
            // keep it as a module chunk.
            let start = start_override.unwrap_or_else(|| self.leading_start(node));
            let end = self.node_range(node).1;
            self.emit_unit(None, &[], "", None, start, end);
            return;
        };
        let kind_enum = match kind {
            "function_declaration" | "generator_function_declaration" => SymbolKind::Function,
            "class_declaration" | "abstract_class_declaration" => SymbolKind::Class,
            "interface_declaration" => SymbolKind::Interface,
            "enum_declaration" => SymbolKind::Enum,
            "type_alias_declaration" => SymbolKind::TypeAlias,
            "internal_module" => SymbolKind::Namespace,
            _ => return,
        };
        let start = start_override.unwrap_or_else(|| self.leading_start(node));
        let end = self.node_range(node).1;
        self.claim_lines(start, end);
        self.add_symbol(name.clone(), kind_enum, start, end, None, exported);
        let heading = vec![name.clone()];
        if estimate_tokens(&self.lines.span(start, end)) <= MAX_CHUNK_TOKENS {
            self.emit_unit(None, &heading, &name, None, start, end);
            if kind_enum == SymbolKind::Class {
                self.emit_methods(node, &name, Some(self.symbols.len() - 1));
            } else if kind_enum == SymbolKind::Function {
                self.emit_nested_functions(node, &name, Some(self.symbols.len() - 1));
            }
        } else if matches!(
            kind,
            "function_declaration" | "generator_function_declaration"
        ) && node
            .child_by_field_name("body")
            .is_some_and(|body| body.kind() == "statement_block")
        {
            self.split_function(node, &name, &heading, start, end);
        } else if matches!(kind, "class_declaration" | "abstract_class_declaration") {
            self.split_class(node, &name, &heading, start, end);
        } else {
            self.split_generic(&name, &heading, start, end);
        }
    }

    /// Emits method chunks of a single-chunk class (overlapping the class
    /// chunk's range on purpose: methods are members of the class).
    fn emit_methods(&mut self, class_node: Node, class_name: &str, class_symbol: Option<usize>) {
        let Some(body) = class_node.child_by_field_name("body") else {
            return;
        };
        for i in 0..body.child_count() {
            let Some(item) = body.child(i) else {
                continue;
            };
            if item.kind() != "method_definition" {
                continue;
            };
            let Some(name_node) = item.child_by_field_name("name") else {
                continue;
            };
            let mname = self.text_of(name_node);
            let start = self.leading_start(item).min(self.node_range(item).0);
            let end = self.node_range(item).1;
            let heading = vec![class_name.to_string(), mname.clone()];
            self.emit_unit(None, &heading, &mname, None, start, end);
            self.add_symbol(mname, SymbolKind::Method, start, end, class_symbol, false);
        }
    }

    /// Emits nested named-function chunks inside a single-chunk function
    /// (recursive: each level extends the heading ancestry).
    fn emit_nested_functions(&mut self, fn_node: Node, fn_name: &str, fn_symbol: Option<usize>) {
        let Some(body) = fn_node.child_by_field_name("body") else {
            return;
        };
        for i in 0..body.child_count() {
            let Some(stmt) = body.child(i) else {
                continue;
            };
            if !matches!(
                stmt.kind(),
                "function_declaration" | "generator_function_declaration"
            ) {
                continue;
            }
            let Some(inner_name) = self.decl_name(stmt) else {
                continue;
            };
            let start = self.leading_start(stmt).min(self.node_range(stmt).0);
            let end = self.node_range(stmt).1;
            let heading = vec![fn_name.to_string(), inner_name.clone()];
            self.emit_unit(None, &heading, &inner_name, None, start, end);
            let symbol_index = self.add_symbol(
                inner_name.clone(),
                SymbolKind::Function,
                start,
                end,
                fn_symbol,
                false,
            );
            self.emit_nested_functions(stmt, &inner_name, Some(symbol_index));
        }
    }

    /// Groups consecutive statement/item ranges so each group's joined text
    /// stays within the hard token ceiling.
    fn group_ranges(&self, ranges: &[(u32, u32)]) -> Vec<Vec<(u32, u32)>> {
        let mut groups: Vec<Vec<(u32, u32)>> = Vec::new();
        let mut current: Vec<(u32, u32)> = Vec::new();
        let mut current_tokens = 0u64;
        for (start, end) in ranges {
            let tokens = estimate_tokens(&self.lines.span(*start, *end));
            if !current.is_empty() && current_tokens + tokens > MAX_CHUNK_TOKENS {
                groups.push(std::mem::take(&mut current));
                current_tokens = 0;
            }
            current_tokens += tokens;
            current.push((*start, *end));
        }
        if !current.is_empty() {
            groups.push(current);
        }
        groups
    }

    /// Collects the body item ranges of a block node (braces skipped,
    /// leading comments/decorators attached to the following item).
    fn block_item_ranges(&self, body: Node) -> Vec<(u32, u32)> {
        let mut ranges = Vec::new();
        let mut pending: Option<u32> = None;
        for i in 0..body.child_count() {
            let Some(child) = body.child(i) else {
                continue;
            };
            match child.kind() {
                "{" | "}" => {}
                "comment" | "decorator" => {
                    pending = Some(self.node_range(child).0.min(pending.unwrap_or(u32::MAX)));
                }
                _ => {
                    let (child_start, child_end) = self.node_range(child);
                    let start = pending
                        .take()
                        .filter(|start| *start < child_start)
                        .unwrap_or(child_start);
                    ranges.push((start, child_end));
                }
            }
        }
        ranges
    }

    /// The signature context label for split children (context only; never
    /// stored in chunk text or covered by source ranges).
    fn split_context(&self, heading: &[String], start: u32, end: u32) -> String {
        let signature = self.lines.texts[(start - 1) as usize..end as usize]
            .iter()
            .find(|line| !line.trim().is_empty())
            .cloned()
            .unwrap_or_default();
        format!("{} | {}", heading.join(" > "), signature)
    }

    /// Splits an oversized function: the hosting chunk carries the signature
    /// plus the first statement group; later statement groups are child
    /// chunks with a parent reference and signature context.
    fn split_function(&mut self, node: Node, name: &str, heading: &[String], start: u32, end: u32) {
        let body = node
            .child_by_field_name("body")
            .expect("caller verified statement_block body");
        let body_start = self.node_range(body).0;
        let stmt_ranges = self.block_item_ranges(body);
        let groups = self.group_ranges(&stmt_ranges);
        let hosting_end = groups
            .first()
            .map(|group| group.last().unwrap().1)
            .unwrap_or(body_start);
        if estimate_tokens(&self.lines.span(start, hosting_end)) > MAX_CHUNK_TOKENS {
            // Signature itself is oversized: plain line-window split.
            self.split_generic(name, heading, start, end);
            return;
        }
        let context = self.split_context(heading, start, end);
        let hosting_ordinal = self.emit_unit(None, heading, name, None, start, hosting_end);
        for group in groups.iter().skip(1) {
            let group_start = group.first().unwrap().0;
            let group_end = group.last().unwrap().1;
            self.emit_unit(
                Some(hosting_ordinal),
                heading,
                name,
                Some(context.clone()),
                group_start,
                group_end,
            );
        }
    }

    /// Splits an oversized class: the hosting chunk carries the class header
    /// plus the first body-item group; later item groups are child chunks.
    fn split_class(&mut self, node: Node, name: &str, heading: &[String], start: u32, end: u32) {
        let Some(body) = node.child_by_field_name("body") else {
            self.split_generic(name, heading, start, end);
            return;
        };
        let body_start = self.node_range(body).0;
        let item_ranges = self.block_item_ranges(body);
        let groups = self.group_ranges(&item_ranges);
        let hosting_end = groups
            .first()
            .map(|group| group.last().unwrap().1)
            .unwrap_or(body_start);
        if estimate_tokens(&self.lines.span(start, hosting_end)) > MAX_CHUNK_TOKENS {
            // Header itself is oversized: plain line-window split.
            self.split_generic(name, heading, start, end);
            return;
        }
        let context = self.split_context(heading, start, end);
        let hosting_ordinal = self.emit_unit(None, heading, name, None, start, hosting_end);
        for group in groups.iter().skip(1) {
            let group_start = group.first().unwrap().0;
            let group_end = group.last().unwrap().1;
            self.emit_unit(
                Some(hosting_ordinal),
                heading,
                name,
                Some(context.clone()),
                group_start,
                group_end,
            );
        }
    }

    /// Splits any other oversized declaration by line windows.
    fn split_generic(&mut self, name: &str, heading: &[String], start: u32, end: u32) {
        let lines = &self.lines.texts[(start - 1) as usize..end as usize];
        let parts = split_by_lines(lines, MAX_CHUNK_TOKENS);
        let first_len = parts.first().map(Vec::len).unwrap_or(0);
        if first_len == 0 {
            return;
        }
        let hosting_ordinal = self.emit_unit(
            None,
            heading,
            name,
            None,
            start,
            start + first_len as u32 - 1,
        );
        let context = self.split_context(heading, start, start + first_len as u32 - 1);
        let mut running = first_len;
        for part in parts.iter().skip(1) {
            let part_len = part.len() as u32;
            self.emit_unit(
                Some(hosting_ordinal),
                heading,
                name,
                Some(context.clone()),
                start + running as u32,
                start + running as u32 + part_len - 1,
            );
            running += part.len();
        }
    }

    /// Extracts the locally bound names of one import statement.
    fn collect_imports(&mut self, node: Node, line: u32) {
        let module = (0..node.child_count())
            .filter_map(|i| node.child(i))
            .find(|child| child.kind() == "string")
            .map(|node| unquote(&self.text_of(node)));
        let Some(module) = module else {
            return;
        };
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else {
                continue;
            };
            match child.kind() {
                "import_clause" => self.collect_clause_names(child, line, &module),
                "namespace_import" | "import_specifier" | "default_import" => {
                    if let Some(name) = specifier_name(child, self.text) {
                        self.imports.push(CodeImport {
                            line,
                            local_name: name,
                            module: module.clone(),
                        });
                    }
                }
                "identifier" => {
                    // Default import directly under the statement (TS).
                    let child_name = self.text_of(child);
                    if !Self::is_stop_word(&child_name) {
                        self.imports.push(CodeImport {
                            line,
                            local_name: child_name.clone(),
                            module: module.clone(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    /// Extracts import names nested one level deeper (import_clause /
    /// named_imports wrappers differ between grammars).
    fn collect_clause_names(&mut self, clause: Node, line: u32, module: &str) {
        for i in 0..clause.child_count() {
            let Some(child) = clause.child(i) else {
                continue;
            };
            match child.kind() {
                "named_imports" => self.collect_clause_names(child, line, module),
                "namespace_import" | "import_specifier" | "default_import" => {
                    if let Some(name) = specifier_name(child, self.text) {
                        self.imports.push(CodeImport {
                            line,
                            local_name: name,
                            module: module.to_string(),
                        });
                    }
                }
                "identifier" => {
                    let child_name = self.text_of(child);
                    if !Self::is_stop_word(&child_name) {
                        self.imports.push(CodeImport {
                            line,
                            local_name: child_name.clone(),
                            module: module.to_string(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    /// Extracts re-export edges and local `export default <name>` hints.
    fn collect_reexports(&mut self, node: Node, line: u32) {
        // Module specifier is scanned first (child order is not guaranteed to
        // put `from` before the clause in all grammars).
        let module = (0..node.child_count())
            .filter_map(|i| node.child(i))
            .find(|child| child.kind() == "string")
            .map(|child| unquote(&self.text_of(child)));
        for i in 0..node.child_count() {
            let Some(child) = node.child(i) else {
                continue;
            };
            if child.kind() == "export_clause" {
                for j in 0..child.child_count() {
                    let Some(spec) = child.child(j) else {
                        continue;
                    };
                    if spec.kind() != "export_specifier" {
                        continue;
                    }
                    // The source-local name is the first identifier (before
                    // any `as` alias).
                    let Some(name_node) = (0..spec.child_count())
                        .filter_map(|k| spec.child(k))
                        .find(|c| c.kind() == "identifier")
                    else {
                        continue;
                    };
                    let name = self.text_of(name_node);
                    if Self::is_stop_word(&name) {
                        continue;
                    }
                    if let Some(module) = module.clone() {
                        self.imports.push(CodeImport {
                            line,
                            local_name: name,
                            module,
                        });
                    } else {
                        self.add_ref(line, &name, RefKind::Reexport);
                    }
                }
            } else if child.kind() == "default" {
                // `export default <identifier>`: reference to the local name.
                for k in 0..node.child_count() {
                    if let Some(other) = node.child(k).filter(|child| child.kind() == "identifier")
                    {
                        self.add_ref(line, &self.text_of(other), RefKind::Reference);
                    }
                }
            }
        }
    }

    /// Collects bounded local reference hints in one iterative walk (deep
    /// nesting must not overflow the stack).
    fn collect_refs(&mut self, root: Node) {
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            let text = self.text_of(node);
            if REF_NODE_KINDS.contains(&node.kind()) && !Self::is_stop_word(&text) {
                self.add_ref(self.node_range(node).0, &text, RefKind::Reference);
            } else if node.kind() == "call_expression" {
                let callee = node
                    .child_by_field_name("function")
                    .or_else(|| node.child(0))
                    .filter(|child| child.kind() == "identifier");
                if let Some(callee) = callee {
                    self.add_ref(
                        self.node_range(callee).0,
                        &self.text_of(callee),
                        RefKind::Call,
                    );
                }
            }
            for i in (0..node.child_count()).rev() {
                if let Some(child) = node.child(i) {
                    stack.push(child);
                }
            }
        }
    }
}

/// The locally bound name of an import specifier node (the alias when
/// present, i.e. the last identifier).
fn specifier_name(node: Node, text: &str) -> Option<String> {
    (0..node.child_count())
        .rev()
        .filter_map(|i| node.child(i))
        .find(|child| child.kind() == "identifier")
        .map(|child| text[child.byte_range()].to_string())
        .filter(|name| !STOP_WORDS.contains(&name.as_str()))
}

/// Strips the surrounding quotes of a string literal.
fn unquote(text: &str) -> String {
    text.trim()
        .trim_matches(|c| c == '\'' || c == '"' || c == '`')
        .to_string()
}
