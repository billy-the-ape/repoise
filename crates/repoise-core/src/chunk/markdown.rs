//! Structural Markdown/MDX chunker.
//!
//! Line-based structural scan: fenced code blocks (```` ``` ````/`~~~`) are
//! opaque, so `#` lines inside them are not headings; ATX headings drive
//! section boundaries and heading ancestry; paragraphs, lists, tables and
//! fenced blocks become units with exact line ranges. Front matter is
//! metadata and is not chunked.

use super::{Chunk, Chunker, Section, Unit, UnitKind, pack_sections};

/// Markdown/MDX structural chunker.
#[derive(Clone, Copy, Debug, Default)]
pub struct MarkdownChunker;

impl Chunker for MarkdownChunker {
    fn parse(&self, path: &str, text: &str) -> Vec<Chunk> {
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        if lines.is_empty() {
            return Vec::new();
        }
        let start = skip_front_matter(&lines);
        let mut sections: Vec<Section> = Vec::new();
        let mut current = Section::default();
        let mut stack: Vec<(u8, String)> = Vec::new(); // (level, text)
        let mut i = start;
        while i < lines.len() {
            let line = &lines[i];
            if line.trim().is_empty() {
                i += 1;
                continue;
            }
            // Fenced code block (fence char run of 3+ at the line start).
            let fence = fence_open(line);
            if let Some((char, len)) = fence {
                let mut j = i + 1;
                while j < lines.len() && !fence_close(&lines[j], char, len) {
                    j += 1;
                }
                if j < lines.len() {
                    j += 1; // closing fence
                }
                current
                    .units
                    .push(unit(&lines, i, j, UnitKind::Code, &stack));
                i = j;
                continue;
            }
            // ATX heading: closes the current section, opens a new one.
            if let Some((level, heading_text)) = heading(line) {
                if !current.units.is_empty() {
                    sections.push(std::mem::take(&mut current));
                }
                while stack.last().is_some_and(|(l, _)| *l >= level) {
                    stack.pop();
                }
                stack.push((level, heading_text));
                current = Section {
                    heading_path: stack_texts(&stack),
                    units: Vec::new(),
                };
                current.units.push(Unit {
                    kind: UnitKind::Heading,
                    lines: vec![line.clone()],
                    line_start: (i + 1) as u32,
                    line_end: (i + 1) as u32,
                    heading_path: stack_texts(&stack),
                });
                i += 1;
                continue;
            }
            // Pipe table (row plus separator row, then pipe rows).
            if is_table_start(&lines, i) {
                let mut j = i + 2;
                while j < lines.len() && is_table_row(&lines[j]) && !lines[j].trim().is_empty() {
                    j += 1;
                }
                current
                    .units
                    .push(unit(&lines, i, j, UnitKind::Table, &stack));
                i = j;
                continue;
            }
            // List (marker lines plus indented continuation lines).
            if is_list_marker(line) {
                let mut j = i + 1;
                while j < lines.len() {
                    let next = &lines[j];
                    if next.trim().is_empty() {
                        break;
                    }
                    if is_list_marker(next) || next.starts_with([' ', '\t']) {
                        j += 1;
                    } else {
                        break;
                    }
                }
                current
                    .units
                    .push(unit(&lines, i, j, UnitKind::List, &stack));
                i = j;
                continue;
            }
            // Paragraph: accumulate plain lines until structure.
            let mut j = i + 1;
            while j < lines.len()
                && !lines[j].trim().is_empty()
                && fence_open(&lines[j]).is_none()
                && heading(&lines[j]).is_none()
                && !is_list_marker(&lines[j])
                && !is_table_start(&lines, j)
            {
                j += 1;
            }
            current
                .units
                .push(unit(&lines, i, j, UnitKind::Paragraph, &stack));
            i = j;
        }
        if !current.units.is_empty() {
            sections.push(current);
        }
        pack_sections(path, sections)
    }
}

/// Builds a unit covering 0-based half-open line range `[start, end)`.
fn unit(
    lines: &[String],
    start: usize,
    end: usize,
    kind: UnitKind,
    stack: &[(u8, String)],
) -> Unit {
    Unit {
        kind,
        lines: lines[start..end].to_vec(),
        line_start: (start + 1) as u32,
        line_end: end as u32,
        heading_path: stack_texts(stack),
    }
}

fn stack_texts(stack: &[(u8, String)]) -> Vec<String> {
    stack.iter().map(|(_, text)| text.clone()).collect()
}

/// Index of the first content line after a leading front-matter block.
fn skip_front_matter(lines: &[String]) -> usize {
    if lines.first().is_some_and(|line| line.trim() == "---") {
        for (index, line) in lines.iter().enumerate().skip(1) {
            if line.trim() == "---" || line.trim() == "..." {
                return index + 1;
            }
        }
    }
    0
}

/// Fence open: a run of 3+ backticks or tildes at the (trimmed) line start.
fn fence_open(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let count = 1 + chars.take_while(|c| *c == first).count();
    (count >= 3).then_some((first, count))
}

/// Fence close: same char, a run of at least the opening length, nothing else.
fn fence_close(line: &str, char: char, len: usize) -> bool {
    let trimmed = line.trim_start();
    let count = trimmed.chars().take_while(|c| *c == char).count();
    // `char` is an ASCII fence character, so `count` bytes are consumed.
    count >= len && trimmed[count..].chars().all(|c| c.is_whitespace())
}

/// ATX heading: up to 3 leading spaces, 1–6 hashes, then text.
fn heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    if indent > 3 {
        return None;
    }
    let count = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&count) {
        return None;
    }
    // `#` is ASCII, so `count` bytes are consumed.
    let rest = &trimmed[count..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim().to_string();
    Some((count as u8, text))
}

/// A table start: a pipe row followed by a pipe delimiter row.
fn is_table_start(lines: &[String], index: usize) -> bool {
    index + 1 < lines.len()
        && is_table_row(&lines[index])
        && is_table_row(&lines[index + 1])
        && is_table_separator(&lines[index + 1])
}

/// A row that can participate in a pipe table.
fn is_table_row(line: &str) -> bool {
    line.contains('|')
}

/// A table delimiter row: only pipes, colons, dashes and spaces, with a dash.
fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains('-') && trimmed.chars().all(|c| matches!(c, '|' | ':' | '-' | ' '))
}

/// A list item marker: `-`, `*`, `+` or `1.`/`1)`.
fn is_list_marker(line: &str) -> bool {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed
        .strip_prefix('-')
        .or_else(|| trimmed.strip_prefix('*'))
        .or_else(|| trimmed.strip_prefix('+'))
    {
        return rest.starts_with([' ', '\t']);
    }
    let digits: String = trimmed.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 9 {
        return false;
    }
    let rest = &trimmed[digits.len()..];
    matches!(rest.as_bytes().first(), Some(b'.') | Some(b')'))
        && rest
            .get(1..2)
            .is_some_and(|two| two.starts_with([' ', '\t']))
}
