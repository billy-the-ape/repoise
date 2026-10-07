//! Structural document chunking (master plan section 5).
//!
//! Docs/config files are parsed structurally so that, for example, a `#` line
//! inside a fenced code block is not a heading. Chunkers emit deterministic
//! [`Chunk`] values with exact 1-based line ranges; byte ranges and opaque
//! chunk ids are attached by the indexer (see [`crate::indexing`]).
//!
//! Size policy: aim at 300–700 estimated tokens, hard ceiling 1,000
//! (character count divided by 4 is an estimate until a tokenizer/profile is
//! configured). Oversized units split into labeled child chunks that repeat
//! the heading/context, keep exact ranges and reference their parent.

pub mod code;
pub mod config_fmt;
pub mod markdown;
pub mod text;
pub mod ts;

use serde::{Deserialize, Serialize};

/// Estimated token ceiling per chunk (characters / 4).
pub const MAX_CHUNK_TOKENS: u64 = 1000;
/// Target chunk size for packing (characters / 4).
pub const TARGET_CHUNK_TOKENS: u64 = 700;

/// Parser versions recorded on chunk/file records; bumping any of them
/// invalidates stored chunk identity for that corpus.
pub const PARSER_VERSION_MARKDOWN: &str = "markdown/1";
pub const PARSER_VERSION_TEXT: &str = "text/1";
pub const PARSER_VERSION_CONFIG: &str = "config/1";
/// TypeScript/TSX structural parser (pinned `tree-sitter-typescript`
/// grammar version + chunker rule version).
pub const PARSER_VERSION_TS: &str = "ts/tree-sitter-0.23.2/1";
/// JavaScript/JSX structural parser (pinned `tree-sitter-javascript`
/// grammar version + chunker rule version).
pub const PARSER_VERSION_JS: &str = "js/tree-sitter-0.25.0/1";
/// Line-window fallback for code without a shipped grammar (and for
/// parser-error ranges of grammar files).
pub const PARSER_VERSION_CODE_LINE: &str = "code-line/1";

/// Logical corpus a chunk belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Corpus {
    /// Prose documents (Markdown, text, RST, AsciiDoc, fallback).
    Docs,
    /// Readable configuration.
    Config,
    /// Source code (structural grammar chunks or line-window fallback).
    Code,
}

/// Kind of a structural unit that composes chunks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnitKind {
    /// Section heading line.
    Heading,
    /// Prose paragraph.
    Paragraph,
    /// Consecutive list items.
    List,
    /// Pipe table including its separator row.
    Table,
    /// Fenced code block.
    Code,
    /// Generic structural block (config sections/fields).
    Block,
}

/// One structural unit with exact source lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    /// Structural kind (drives splitting strategy).
    pub kind: UnitKind,
    /// The unit text lines, without surrounding blank lines and without
    /// trailing carriage returns.
    pub lines: Vec<String>,
    /// 1-based inclusive line range in the source file.
    pub line_start: u32,
    pub line_end: u32,
    /// Heading ancestors in effect for this unit (outermost first).
    pub heading_path: Vec<String>,
}

/// One chunk produced by a chunker.
///
/// `ordinal` is the chunk's index among its siblings, and `parent` is the
/// ordinal of the parent chunk (only set for labeled splits of an oversized
/// unit). Neither field alone identifies a chunk; the indexer combines
/// scope, structural position and text hash into an opaque chunk id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// Index among siblings.
    pub ordinal: u32,
    /// Parent chunk ordinal when this is a labeled split.
    pub parent: Option<u32>,
    /// Heading ancestry (outermost first); empty at the document root.
    pub heading_path: Vec<String>,
    /// Chunk text. Labeled splits begin with a `> context: ...` line that is
    /// not part of the source (ranges never cover it).
    pub text: String,
    /// 1-based inclusive source line range.
    pub line_start: u32,
    pub line_end: u32,
}

/// Structural chunker for one family of formats.
pub trait Chunker {
    /// Parses `path`/`text` into deterministic chunks. Empty input yields no
    /// chunks; `path` feeds the context label of labeled splits only.
    fn parse(&self, path: &str, text: &str) -> Vec<Chunk>;
}

/// Estimates token count from character count (labeled as an estimate).
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

/// Whether a unit alone exceeds the hard ceiling.
pub fn is_oversized(lines: &[String]) -> bool {
    estimate_tokens(&join_lines(lines)) > MAX_CHUNK_TOKENS
}

/// Joins lines with `\n`.
pub fn join_lines(lines: &[String]) -> String {
    lines.join("\n")
}

/// Splits `lines` into consecutive parts whose joined text stays within the
/// token cap. Parts preserve order; every line appears in exactly one part.
pub fn split_by_lines(lines: &[String], max_tokens: u64) -> Vec<Vec<String>> {
    let mut parts: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_tokens: u64 = 0;
    for line in lines {
        let line_tokens = estimate_tokens(line);
        if !current.is_empty() && current_tokens + line_tokens > max_tokens {
            parts.push(std::mem::take(&mut current));
            current_tokens = 0;
        }
        current.push(line.clone());
        current_tokens += line_tokens;
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

/// The context label line repeated in every labeled split (context only;
/// never source bytes, so source ranges stay exact).
pub fn context_label(heading_path: &[String], path: &str) -> String {
    if heading_path.is_empty() {
        format!("> context: {path}")
    } else {
        format!("> context: {} ({})", heading_path.join(" > "), path)
    }
}

/// One structural section: a heading (or the document root) plus the units
/// that follow it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Section {
    /// Heading ancestry for the section (outermost first).
    pub heading_path: Vec<String>,
    /// Structural units, in document order (the leading heading unit first).
    pub units: Vec<Unit>,
}

/// A fragment of one section chunk: either a whole unit or the first part of
/// an oversized unit (labeled continuation parts live in [`Group::children`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Fragment {
    /// Fragment text lines (no label for part 0; later parts carry one).
    lines: Vec<String>,
    /// 1-based inclusive source range (never covers synthetic label lines).
    line_start: u32,
    line_end: u32,
}

/// One packing atom: a unit (or the first part of an oversized unit) plus
/// its labeled continuation parts, which become child chunks of the section
/// chunk that contains the first part.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Group {
    /// The fragment carried in the section chunk text.
    fragment: Fragment,
    /// Labeled continuation parts (parent reference = the hosting chunk).
    children: Vec<Fragment>,
}

/// Splits one unit into a group: part 0 plus labeled continuation parts that
/// repeat the header/context. Tables repeat their header rows.
fn split_unit(unit: &Unit, section: &Section, path: &str) -> Group {
    if !is_oversized(&unit.lines) {
        return Group {
            fragment: Fragment {
                lines: unit.lines.clone(),
                line_start: unit.line_start,
                line_end: unit.line_end,
            },
            children: Vec::new(),
        };
    }
    let label = context_label(&section.heading_path, path);
    // Tables keep their first two rows (header + separator) as a repeated
    // header in every continuation part; other units rely on the label.
    let header_len = match unit.kind {
        UnitKind::Table if unit.lines.len() >= 3 => 2,
        _ => 0,
    };
    let header = &unit.lines[..header_len];
    let body = &unit.lines[header_len..];
    let body_parts = split_by_lines(body, MAX_CHUNK_TOKENS);
    let mut fragment = Fragment {
        lines: Vec::new(),
        line_start: unit.line_start,
        line_end: unit.line_end,
    };
    let mut children: Vec<Fragment> = Vec::new();
    let mut running = 0usize;
    for (index, part) in body_parts.iter().enumerate() {
        let line_start = unit.line_start + running as u32;
        let line_end = line_start + (part.len() - 1) as u32;
        running += part.len();
        if index == 0 {
            let mut lines = header.to_vec();
            lines.extend(part.iter().cloned());
            fragment = Fragment {
                lines,
                line_start: unit.line_start,
                line_end: unit.line_start + (header_len + part.len() - 1) as u32,
            };
        } else {
            let mut lines = vec![label.clone()];
            lines.extend(header.iter().cloned());
            lines.extend(part.iter().cloned());
            children.push(Fragment {
                lines,
                line_start,
                line_end,
            });
        }
    }
    Group { fragment, children }
}

/// Packs sections into chunks: sections pack consecutive units at the target
/// size; oversized units split into child chunks with a parent reference.
/// Chunks of one section after the first repeat the context label.
pub fn pack_sections(path: &str, sections: Vec<Section>) -> Vec<Chunk> {
    let mut chunks: Vec<Chunk> = Vec::new();
    for section in sections {
        let groups: Vec<Group> = section
            .units
            .iter()
            .map(|unit| split_unit(unit, &section, path))
            .collect();
        let label = context_label(&section.heading_path, path);
        let mut current: Vec<Group> = Vec::new();
        let mut current_tokens: u64 = 0;
        let mut first_in_section = true;
        for group in groups {
            let tokens = estimate_tokens(&join_lines(&group.fragment.lines));
            if !current.is_empty() && current_tokens + tokens > TARGET_CHUNK_TOKENS {
                emit_section_chunk(&mut chunks, &section, &current, &label, first_in_section);
                first_in_section = false;
                current = Vec::new();
                current_tokens = 0;
            }
            current_tokens += tokens;
            current.push(group);
        }
        if !current.is_empty() {
            emit_section_chunk(&mut chunks, &section, &current, &label, first_in_section);
        }
    }
    chunks
}

fn emit_section_chunk(
    chunks: &mut Vec<Chunk>,
    section: &Section,
    groups: &[Group],
    label: &str,
    first_in_section: bool,
) {
    let ordinal = chunks.len() as u32;
    let mut texts: Vec<String> = Vec::new();
    for group in groups {
        texts.push(join_lines(&group.fragment.lines));
    }
    let mut text = texts.join("\n\n");
    if !first_in_section {
        text = format!("{label}\n{text}");
    }
    let line_start = groups
        .first()
        .map(|group| group.fragment.line_start)
        .unwrap_or(0);
    let line_end = groups
        .last()
        .map(|group| group.fragment.line_end)
        .unwrap_or(0);
    chunks.push(Chunk {
        ordinal,
        parent: None,
        heading_path: section.heading_path.clone(),
        text,
        line_start,
        line_end,
    });
    for group in groups {
        for (index, child) in group.children.iter().enumerate() {
            chunks.push(Chunk {
                ordinal: index as u32,
                parent: Some(ordinal),
                heading_path: section.heading_path.clone(),
                text: join_lines(&child.lines),
                line_start: child.line_start,
                line_end: child.line_end,
            });
        }
    }
}
