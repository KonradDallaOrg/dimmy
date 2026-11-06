//! The licence and the open source notices as a list of blocks the hosts
//! draw natively: headings, paragraphs, tables, licence texts.
//!
//! Both texts are compiled in (`LICENSE_TEXT`, `THIRD_PARTY_NOTICES`). Shown
//! raw they read badly: the notices are ~1 MB of markdown (a 600-row table,
//! ~300 licence texts in code fences) and a monospace text box wrapped the
//! table into noise. Parsing happens once, here, with pulldown-cmark (the
//! CommonMark parser rustdoc uses), so Windows and macOS draw the same
//! structure and neither carries a markdown parser of its own.

use std::sync::OnceLock;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::Serialize;

#[derive(Serialize, Debug, PartialEq)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Block {
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        text: String,
    },
    Table {
        header: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    /// A licence text or other preformatted block: shown as is, monospace.
    Code {
        text: String,
    },
}

fn level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Markdown (headings, paragraphs, lists, pipe tables, code) to blocks.
/// Inline formatting is flattened to its text; list items become
/// paragraphs starting with a bullet.
pub fn from_markdown(md: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut text = String::new();
    let mut header: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut item_depth = 0usize;

    for event in Parser::new_ext(md, Options::ENABLE_TABLES) {
        match event {
            Event::Start(Tag::Heading { .. })
            | Event::Start(Tag::CodeBlock(_))
            | Event::Start(Tag::TableCell) => text.clear(),
            Event::Start(Tag::Paragraph) if item_depth == 0 => text.clear(),
            Event::Start(Tag::Item) => {
                if item_depth == 0 {
                    text.clear();
                }
                item_depth += 1;
                text.push_str("• ");
            }
            Event::End(TagEnd::Item) => {
                item_depth -= 1;
                if item_depth == 0 {
                    blocks.push(Block::Paragraph {
                        text: text.trim().to_string(),
                    });
                    text.clear();
                } else {
                    text.push(' ');
                }
            }
            Event::End(TagEnd::Heading(l)) => blocks.push(Block::Heading {
                level: level(l),
                text: text.trim().to_string(),
            }),
            Event::End(TagEnd::Paragraph) if item_depth == 0 => {
                let t = text.trim();
                if !t.is_empty() {
                    blocks.push(Block::Paragraph {
                        text: t.to_string(),
                    });
                }
            }
            Event::End(TagEnd::Paragraph) => text.push(' '),
            Event::End(TagEnd::CodeBlock) => blocks.push(Block::Code {
                text: text.trim_end_matches('\n').to_string(),
            }),
            Event::Start(Tag::Table(_)) => {
                header.clear();
                rows.clear();
                row.clear();
            }
            Event::End(TagEnd::TableCell) => row.push(text.trim().to_string()),
            Event::End(TagEnd::TableHead) => header = std::mem::take(&mut row),
            Event::End(TagEnd::TableRow) => rows.push(std::mem::take(&mut row)),
            Event::End(TagEnd::Table) => blocks.push(Block::Table {
                header: std::mem::take(&mut header),
                rows: std::mem::take(&mut rows),
            }),
            Event::Text(t) | Event::Code(t) => text.push_str(&t),
            Event::SoftBreak => text.push(' '),
            Event::HardBreak => text.push('\n'),
            _ => {}
        }
    }
    blocks
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

fn is_numbered_title(t: &str) -> bool {
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0 && t[digits..].starts_with(". ") && t.ends_with('.') && t.len() < 90
}

fn is_caps_title(t: &str) -> bool {
    t.len() < 90 && t.chars().any(|c| c.is_alphabetic()) && !t.chars().any(|c| c.is_lowercase())
}

/// Plain text (the AGPL) to blocks. The licence file is one paragraph per
/// line, blank lines between them; section titles sit on their own line,
/// sometimes with the body right under them; one title is centred; the
/// sample notice at the end is indented. So: the first line and ALL-CAPS
/// lines are headings, `N. Title.` lines are section headings, a centred
/// line or a lone short line without end punctuation (`Preamble`) is a
/// heading, indented paragraphs are preformatted, everything else is a
/// paragraph (hard-wrapped lines joined).
pub fn from_plain(text: &str) -> Vec<Block> {
    let mut chunks: Vec<Vec<&str>> = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    // lines() also drops the \r of CRLF files.
    for line in text.lines() {
        if line.trim().is_empty() {
            if !para.is_empty() {
                chunks.push(std::mem::take(&mut para));
            }
        } else {
            para.push(line);
        }
    }
    if !para.is_empty() {
        chunks.push(para);
    }

    let mut blocks: Vec<Block> = Vec::new();
    let mut first = true;
    for chunk in chunks {
        let lone = chunk.len() == 1;
        let t0 = chunk[0].trim();
        if lone && indent(chunk[0]) >= 10 {
            blocks.push(Block::Heading {
                level: 2,
                text: t0.to_string(),
            });
            first = false;
            continue;
        }
        if chunk.iter().all(|l| indent(l) >= 4) {
            let cut = chunk.iter().map(|l| indent(l)).min().unwrap_or(0);
            let code = chunk
                .iter()
                .map(|l| &l[cut..])
                .collect::<Vec<_>>()
                .join("\n");
            match blocks.last_mut() {
                Some(Block::Code { text }) => {
                    text.push_str("\n\n");
                    text.push_str(&code);
                }
                _ => blocks.push(Block::Code { text: code }),
            }
            continue;
        }
        let title = if first {
            Some(1)
        } else if is_numbered_title(t0) {
            Some(3)
        } else if is_caps_title(t0)
            || (lone
                && t0.split_whitespace().count() <= 4
                && !t0.ends_with(['.', ':', ';', ',', ')', '>']))
        {
            Some(2)
        } else {
            None
        };
        first = false;
        let rest: &[&str] = match title {
            Some(level) => {
                blocks.push(Block::Heading {
                    level,
                    text: t0.to_string(),
                });
                &chunk[1..]
            }
            None => &chunk,
        };
        if !rest.is_empty() {
            blocks.push(Block::Paragraph {
                text: rest.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" "),
            });
        }
    }
    blocks
}

static LICENSE_JSON: OnceLock<String> = OnceLock::new();
static NOTICES_JSON: OnceLock<String> = OnceLock::new();

/// The licence (`notices == false`) or the notices as JSON blocks, built
/// once per process.
pub fn json(notices: bool) -> &'static str {
    let (cell, build): (&OnceLock<String>, fn() -> Vec<Block>) = if notices {
        (&NOTICES_JSON, || from_markdown(crate::THIRD_PARTY_NOTICES))
    } else {
        (&LICENSE_JSON, || from_plain(crate::LICENSE_TEXT))
    };
    cell.get_or_init(|| {
        let blocks = build();
        assert!(
            !blocks.is_empty(),
            "a compiled-in legal text parses to blocks"
        );
        serde_json::to_string(&blocks).expect("legal blocks serialise")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(level: u8, t: &str) -> Block {
        Block::Heading {
            level,
            text: t.into(),
        }
    }

    fn p(t: &str) -> Block {
        Block::Paragraph { text: t.into() }
    }

    #[test]
    fn markdown_keeps_headings_paragraphs_tables_and_code() {
        let md = [
            "# Open source notices",
            "",
            "Dimmy is *free* software.",
            "It includes:",
            "",
            "## Rust crates (2)",
            "",
            "| Crate | Version |",
            "|---|---|",
            "| a | 1.0 |",
            "| b | 2.0 |",
            "",
            "```",
            "MIT License",
            "",
            "Copyright (c) X",
            "```",
            "",
            "- one",
            "- two",
        ]
        .join("\n");
        assert_eq!(
            from_markdown(&md),
            vec![
                h(1, "Open source notices"),
                p("Dimmy is free software. It includes:"),
                h(2, "Rust crates (2)"),
                Block::Table {
                    header: vec!["Crate".into(), "Version".into()],
                    rows: vec![
                        vec!["a".into(), "1.0".into()],
                        vec!["b".into(), "2.0".into()]
                    ],
                },
                Block::Code {
                    text: "MIT License\n\nCopyright (c) X".into()
                },
                p("• one"),
                p("• two"),
            ]
        );
    }

    #[test]
    fn the_real_notices_lose_nothing() {
        let blocks = from_markdown(crate::THIRD_PARTY_NOTICES);
        // Every licence text survives as its own block.
        let fences = crate::THIRD_PARTY_NOTICES
            .lines()
            .filter(|l| l.trim_start().starts_with("```"))
            .count();
        let code = blocks
            .iter()
            .filter(|b| matches!(b, Block::Code { .. }))
            .count();
        assert_eq!(code, fences / 2, "one code block per fenced licence text");
        // The crate table keeps every row and its four columns.
        let table_lines = crate::THIRD_PARTY_NOTICES
            .lines()
            .skip_while(|l| !l.starts_with("## Rust crates"))
            .skip(1)
            .skip_while(|l| !l.starts_with('|'))
            .take_while(|l| l.starts_with('|'))
            .count();
        match blocks.iter().find(|b| matches!(b, Block::Table { .. })) {
            Some(Block::Table { header, rows }) => {
                assert_eq!(header.len(), 4);
                assert_eq!(
                    rows.len(),
                    table_lines - 2,
                    "header and delimiter rows are not data"
                );
                assert!(rows.iter().all(|r| r.len() == 4));
            }
            _ => panic!("the notices start with the crate table"),
        }
    }

    #[test]
    fn the_licence_gets_its_titles_sections_and_sample_notice() {
        let agpl = [
            "GNU AFFERO GENERAL PUBLIC LICENSE",
            "Version 3, 19 November 2007",
            "",
            "Copyright (C) 2007 Free Software Foundation, Inc. <http://fsf.org/>",
            "",
            "Preamble",
            "",
            "The GNU Affero General Public License is a free,",
            "copyleft license.",
            "",
            "TERMS AND CONDITIONS",
            "",
            "0. Definitions.",
            "",
            "1. Source Code.",
            "The source code is the preferred form.",
            "",
            "            How to Apply These Terms to Your New Programs",
            "",
            "     <one line to give the program's name>",
            "     Copyright (C) <year>",
            "",
            "     This program is free software.",
            "",
            "Copyright (c) 2026 Konrad Dalla",
        ]
        .join("\r\n");
        assert_eq!(
            from_plain(&agpl),
            vec![
                h(1, "GNU AFFERO GENERAL PUBLIC LICENSE"),
                p("Version 3, 19 November 2007"),
                p("Copyright (C) 2007 Free Software Foundation, Inc. <http://fsf.org/>"),
                h(2, "Preamble"),
                p("The GNU Affero General Public License is a free, copyleft license."),
                h(2, "TERMS AND CONDITIONS"),
                h(3, "0. Definitions."),
                h(3, "1. Source Code."),
                p("The source code is the preferred form."),
                h(2, "How to Apply These Terms to Your New Programs"),
                Block::Code {
                    text: "<one line to give the program's name>\nCopyright (C) <year>\n\nThis program is free software.".into()
                },
                p("Copyright (c) 2026 Konrad Dalla"),
            ]
        );
    }

    #[test]
    fn the_real_licence_has_all_its_sections() {
        let blocks = from_plain(crate::LICENSE_TEXT);
        assert!(matches!(&blocks[0], Block::Heading { level: 1, .. }));
        let sections = blocks
            .iter()
            .filter(|b| matches!(b, Block::Heading { level: 3, .. }))
            .count();
        assert_eq!(sections, 18, "sections 0 to 17");
    }

    #[test]
    fn json_is_tagged_and_cached() {
        let a = json(true);
        assert!(
            a.starts_with(r#"[{"t":"heading""#),
            "{}",
            &a[..60.min(a.len())]
        );
        assert!(std::ptr::eq(a, json(true)), "built once");
        let lic: serde_json::Value = serde_json::from_str(json(false)).unwrap();
        assert!(lic.as_array().unwrap().len() > 50);
    }
}
