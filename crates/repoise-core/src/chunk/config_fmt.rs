//! Structural chunker for readable configuration (JSON/YAML/TOML/INI/conf).
//!
//! INI-style formats split on `[section]` headers; JSON/YAML and other
//! formats split on blank-line blocks. Oversized blocks split by lines with
//! the section context repeated. Ranges are exact; no config value is ever
//! re-interpreted as code.

use super::{Chunk, Chunker, Section, Unit, UnitKind, pack_sections};

/// Config structural chunker.
#[derive(Clone, Copy, Debug, Default)]
pub struct ConfigChunker {
    /// Whether the format uses `[section]` headers (INI/conf/TOML).
    pub sectioned: bool,
}

impl Chunker for ConfigChunker {
    fn parse(&self, path: &str, text: &str) -> Vec<Chunk> {
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        if lines.is_empty() {
            return Vec::new();
        }
        let mut sections: Vec<Section> = Vec::new();
        let mut current = Section::default();
        let mut block: Vec<(usize, String)> = Vec::new(); // (0-based line, text)
        let push_block = |block: &mut Vec<(usize, String)>, current: &mut Section| {
            if block.is_empty() {
                return;
            };
            let start = block.first().expect("non-empty").0;
            let end = block.last().expect("non-empty").0 + 1;
            current.units.push(Unit {
                kind: UnitKind::Block,
                lines: block.iter().map(|(_, line)| line.clone()).collect(),
                line_start: start as u32 + 1,
                line_end: end as u32,
                heading_path: current.heading_path.clone(),
            });
            block.clear();
        };
        let mut i = 0;
        while i < lines.len() {
            let line = &lines[i];
            let section = self.sectioned.then(|| line.trim()).and_then(|trimmed| {
                let inner = trimmed.strip_prefix('[')?.strip_suffix(']')?;
                (!inner.trim().is_empty()).then_some(inner.trim().to_string())
            });
            if let Some(name) = section {
                push_block(&mut block, &mut current);
                if !current.units.is_empty() {
                    sections.push(std::mem::take(&mut current));
                }
                current = Section {
                    heading_path: vec![name],
                    units: Vec::new(),
                };
                i += 1;
                continue;
            }
            if line.trim().is_empty() {
                push_block(&mut block, &mut current);
                i += 1;
                continue;
            }
            block.push((i, line.clone()));
            i += 1;
        }
        push_block(&mut block, &mut current);
        if !current.units.is_empty() {
            sections.push(current);
        }
        pack_sections(path, sections)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_sections_become_chunk_sections() {
        let doc = "[server]\nhost = example.com\n\n[cache]\nttl = 60\n";
        let chunks = ConfigChunker { sectioned: true }.parse("a.toml", doc);
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.heading_path == vec!["cache".to_string()])
        );
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.text.contains("host = example.com"))
        );
    }

    #[test]
    fn blank_line_blocks_are_units() {
        let doc = "a = 1\nb = 2\n\nc = 3\n";
        let chunks = ConfigChunker { sectioned: false }.parse("a.json", doc);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].line_start, 1);
        assert_eq!(chunks[0].line_end, 4);
    }
}
