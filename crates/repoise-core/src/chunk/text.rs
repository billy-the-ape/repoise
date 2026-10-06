//! Structural chunker for .txt/.rst/.adoc and generic text fallbacks.
//!
//! RestructuredText underline headings and AsciiDoc `= Title` headings drive
//! sections; everything else is split into paragraphs on blank lines. Plain
//! text has a single implicit section. All units carry exact line ranges.

use super::{Chunk, Chunker, Section, Unit, UnitKind, pack_sections};

/// Text/RST/AsciiDoc structural chunker.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextChunker {
    /// Format family to interpret (drives heading detection).
    pub flavor: TextFlavor,
}

/// Heading flavor for the text chunker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextFlavor {
    /// Plain paragraphs only (generic fallback and .txt).
    #[default]
    Plain,
    /// RestructuredText underline headings.
    Rst,
    /// AsciiDoc `= Title` headings.
    Adoc,
}

impl Chunker for TextChunker {
    fn parse(&self, path: &str, text: &str) -> Vec<Chunk> {
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        if lines.is_empty() {
            return Vec::new();
        }
        let mut sections: Vec<Section> = Vec::new();
        let mut current = Section::default();
        let mut i = 0;
        while i < lines.len() {
            let line = &lines[i];
            if line.trim().is_empty() {
                i += 1;
                continue;
            }
            let heading = match self.flavor {
                TextFlavor::Plain => None,
                TextFlavor::Adoc => adoc_heading(line),
                TextFlavor::Rst => rst_heading(&lines, i),
            };
            if let Some(text) = heading {
                if !current.units.is_empty() {
                    sections.push(std::mem::take(&mut current));
                }
                current = Section {
                    heading_path: vec![text],
                    units: Vec::new(),
                };
                let end = match self.flavor {
                    TextFlavor::Rst => i + 2, // text line + underline
                    _ => i + 1,
                };
                current.units.push(Unit {
                    kind: UnitKind::Heading,
                    lines: lines[i..end.min(lines.len())].to_vec(),
                    line_start: i as u32 + 1,
                    line_end: end.min(lines.len()) as u32,
                    heading_path: current.heading_path.clone(),
                });
                i = end;
                continue;
            }
            // Paragraph: consecutive non-blank lines.
            let mut j = i + 1;
            while j < lines.len()
                && !lines[j].trim().is_empty()
                && match self.flavor {
                    TextFlavor::Plain => true,
                    TextFlavor::Adoc => adoc_heading(&lines[j]).is_none(),
                    TextFlavor::Rst => rst_heading(&lines, j).is_none(),
                }
            {
                j += 1;
            }
            current.units.push(Unit {
                kind: UnitKind::Paragraph,
                lines: lines[i..j].to_vec(),
                line_start: i as u32 + 1,
                line_end: j as u32,
                heading_path: current.heading_path.clone(),
            });
            i = j;
        }
        if !current.units.is_empty() {
            sections.push(current);
        }
        pack_sections(path, sections)
    }
}

/// AsciiDoc heading: `= Title` .. `===== Title` (1–5 equals, then a space).
fn adoc_heading(line: &str) -> Option<String> {
    let count = line.chars().take_while(|c| *c == '=').count();
    if !(1..=5).contains(&count) {
        return None;
    }
    let rest = &line[count..];
    if !rest.starts_with(' ') {
        return None;
    }
    let text = rest.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// RST heading: a line whose next line is a single repeated underline char
/// at least as long as the text.
fn rst_heading(lines: &[String], index: usize) -> Option<String> {
    let text = lines[index].trim().to_string();
    if text.is_empty() || index + 1 >= lines.len() {
        return None;
    }
    let underline = lines[index + 1].trim_end();
    if underline.trim().is_empty() || text.chars().count() > underline.chars().count() {
        return None;
    }
    let chars: Vec<char> = underline.chars().collect();
    if !chars.windows(2).all(|window| window[0] == window[1]) {
        return None;
    }
    if !matches!(
        chars[0],
        '=' | ':' | '-' | '~' | '^' | '#' | '*' | '"' | '\'' | '+' | '_'
    ) {
        return None;
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rst_underline_heading_creates_a_section() {
        let doc = "Intro paragraph here.\n\nTitle\n=====\n\nBody text.\n";
        let chunks = TextChunker {
            flavor: TextFlavor::Rst,
        }
        .parse("a.rst", doc);
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.heading_path == vec!["Title".to_string()])
        );
        assert!(
            chunks
                .iter()
                .filter(|chunk| chunk.heading_path.is_empty())
                .any(|chunk| chunk.text.contains("Intro paragraph"))
        );
    }

    #[test]
    fn adoc_heading_creates_a_section() {
        let doc = "= Root\n\nFirst.\n\n== Child\n\nSecond.\n";
        let chunks = TextChunker {
            flavor: TextFlavor::Adoc,
        }
        .parse("a.adoc", doc);
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.heading_path == vec!["Child".to_string()])
        );
    }

    #[test]
    fn plain_text_is_one_section_of_paragraphs() {
        let doc = "para one\n\npara two\n";
        let chunks = TextChunker {
            flavor: TextFlavor::Plain,
        }
        .parse("a.txt", doc);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].line_start, 1);
        assert_eq!(chunks[0].line_end, 3);
    }
}
