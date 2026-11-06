// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! A small Markdown preview. Task text is data, so the preview shows links as text with no action.

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontFamily, FontId, Stroke, Ui};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::theme::Palette;

/// A part of a line with one style.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Span {
    pub text: String,
    pub strong: bool,
    pub emphasis: bool,
    pub code: bool,
    pub strike: bool,
}

/// The mark in front of a list item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Marker {
    Bullet,
    Number(u64),
    Task(bool),
    /// A later paragraph of the same item.
    None,
}

/// A block of a Markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Block {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph(Vec<Span>),
    Item {
        depth: usize,
        marker: Marker,
        spans: Vec<Span>,
    },
    Quote(Vec<Span>),
    Code(String),
    Rule,
}

fn level_number(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    spans: Vec<Span>,
    lists: Vec<Option<u64>>,
    item_marker: Option<Marker>,
    in_item: usize,
    quote: usize,
    strong: usize,
    emphasis: usize,
    strike: usize,
    code: Option<String>,
    link: Option<String>,
}

impl Builder {
    fn push(&mut self, text: &str, code: bool) {
        self.spans.push(Span {
            text: text.to_owned(),
            strong: self.strong > 0,
            emphasis: self.emphasis > 0,
            code,
            strike: self.strike > 0,
        });
    }

    fn flush_paragraph(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        if self.in_item > 0 {
            let marker = self.item_marker.take().unwrap_or(Marker::None);
            self.blocks.push(Block::Item {
                depth: self.lists.len().saturating_sub(1),
                marker,
                spans,
            });
        } else if self.quote > 0 {
            self.blocks.push(Block::Quote(spans));
        } else {
            self.blocks.push(Block::Paragraph(spans));
        }
    }
}

/// Parses Markdown into blocks.
pub(crate) fn parse(text: &str) -> Vec<Block> {
    let options = Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let mut b = Builder::default();
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => b.flush_paragraph(),
                Tag::Heading { .. } => b.flush_paragraph(),
                Tag::BlockQuote(_) => {
                    b.flush_paragraph();
                    b.quote += 1;
                }
                Tag::CodeBlock(_) => {
                    b.flush_paragraph();
                    b.code = Some(String::new());
                }
                Tag::List(start) => {
                    b.flush_paragraph();
                    b.lists.push(start);
                }
                Tag::Item => {
                    b.flush_paragraph();
                    b.in_item += 1;
                    let marker = match b.lists.last_mut() {
                        Some(Some(number)) => {
                            let current = *number;
                            *number += 1;
                            Marker::Number(current)
                        }
                        _ => Marker::Bullet,
                    };
                    b.item_marker = Some(marker);
                }
                Tag::Emphasis => b.emphasis += 1,
                Tag::Strong => b.strong += 1,
                Tag::Strikethrough => b.strike += 1,
                Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                    b.link = Some(dest_url.to_string());
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => b.flush_paragraph(),
                TagEnd::Heading(level) => {
                    let spans = std::mem::take(&mut b.spans);
                    b.blocks.push(Block::Heading {
                        level: level_number(level),
                        spans,
                    });
                }
                TagEnd::BlockQuote(_) => {
                    b.flush_paragraph();
                    b.quote = b.quote.saturating_sub(1);
                }
                TagEnd::CodeBlock => {
                    if let Some(code) = b.code.take() {
                        b.blocks
                            .push(Block::Code(code.trim_end_matches('\n').to_owned()));
                    }
                }
                TagEnd::List(_) => {
                    b.flush_paragraph();
                    b.lists.pop();
                }
                TagEnd::Item => {
                    b.flush_paragraph();
                    b.in_item = b.in_item.saturating_sub(1);
                    b.item_marker = None;
                }
                TagEnd::Emphasis => b.emphasis = b.emphasis.saturating_sub(1),
                TagEnd::Strong => b.strong = b.strong.saturating_sub(1),
                TagEnd::Strikethrough => b.strike = b.strike.saturating_sub(1),
                TagEnd::Link | TagEnd::Image => {
                    if let Some(url) = b.link.take() {
                        b.push(&format!(" ({url})"), false);
                    }
                }
                _ => {}
            },
            Event::Text(text) => {
                if let Some(code) = &mut b.code {
                    code.push_str(&text);
                } else {
                    b.push(&text, false);
                }
            }
            Event::Code(text) => b.push(&text, true),
            Event::Html(text) | Event::InlineHtml(text) => {
                if let Some(code) = &mut b.code {
                    code.push_str(&text);
                } else {
                    b.push(&text, true);
                }
            }
            Event::SoftBreak => b.push(" ", false),
            Event::HardBreak => b.push("\n", false),
            Event::Rule => {
                b.flush_paragraph();
                b.blocks.push(Block::Rule);
            }
            Event::TaskListMarker(done) => b.item_marker = Some(Marker::Task(done)),
            _ => {}
        }
    }
    b.flush_paragraph();
    b.blocks
}

fn job_of(spans: &[Span], size: f32, base: Color32, palette: &Palette) -> LayoutJob {
    let mut job = LayoutJob::default();
    for span in spans {
        let color = if span.strong { palette.text } else { base };
        let mut format = TextFormat {
            font_id: FontId::new(size, FontFamily::Proportional),
            color,
            italics: span.emphasis,
            ..TextFormat::default()
        };
        if span.code {
            format.font_id = FontId::new(size - 1.0, FontFamily::Monospace);
            format.background = palette.surface_alt;
        }
        if span.strike {
            format.strikethrough = Stroke::new(1.0, base);
        }
        if span.strong {
            // The default font has no bold face, so a thin underlay adds weight.
            format.underline = Stroke::NONE;
            format.extra_letter_spacing = 0.2;
        }
        job.append(&span.text, 0.0, format);
    }
    job
}

/// Draws blocks.
pub(crate) fn show(ui: &mut Ui, blocks: &[Block]) {
    let palette = Palette::of(ui);
    let body = 13.5;
    if blocks.is_empty() {
        ui.label(
            egui::RichText::new("Empty")
                .italics()
                .color(palette.text_muted),
        );
        return;
    }
    for block in blocks {
        match block {
            Block::Heading { level, spans } => {
                ui.add_space(if *level <= 2 { 6.0 } else { 3.0 });
                let size = match level {
                    1 => 20.0,
                    2 => 17.0,
                    3 => 15.0,
                    _ => 14.0,
                };
                ui.label(job_of(spans, size, palette.text, &palette));
                ui.add_space(2.0);
            }
            Block::Paragraph(spans) => {
                ui.label(job_of(spans, body, palette.text_soft, &palette));
                ui.add_space(4.0);
            }
            Block::Item {
                depth,
                marker,
                spans,
            } => {
                ui.horizontal_top(|ui| {
                    #[expect(clippy::cast_precision_loss, reason = "list depth is small")]
                    ui.add_space(*depth as f32 * 16.0);
                    let (mark, color) = match marker {
                        Marker::Bullet => {
                            // A shape needs no glyph, so each font can show this bullet.
                            let (rect, _) = ui.allocate_exact_size(
                                egui::Vec2::new(8.0, body),
                                egui::Sense::hover(),
                            );
                            ui.painter()
                                .circle_filled(rect.center(), 2.0, palette.text_muted);
                            (String::new(), palette.text_muted)
                        }
                        Marker::Number(number) => (format!("{number}."), palette.text_muted),
                        Marker::Task(true) => ("[x]".to_owned(), palette.success),
                        Marker::Task(false) => ("[ ]".to_owned(), palette.text_muted),
                        Marker::None => ("  ".to_owned(), palette.text_muted),
                    };
                    if !mark.is_empty() {
                        ui.label(egui::RichText::new(mark).color(color).monospace());
                    }
                    ui.label(job_of(spans, body, palette.text_soft, &palette));
                });
            }
            Block::Quote(spans) => {
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: 10,
                        right: 0,
                        top: 2,
                        bottom: 2,
                    })
                    .show(ui, |ui| {
                        let rect = ui.available_rect_before_wrap();
                        ui.label(job_of(spans, body, palette.text_muted, &palette));
                        let y = rect.top()..=ui.min_rect().bottom();
                        ui.painter()
                            .vline(rect.left() - 8.0, y, Stroke::new(2.0, palette.border));
                    });
                ui.add_space(4.0);
            }
            Block::Code(code) => {
                egui::Frame::new()
                    .fill(palette.surface_alt)
                    .corner_radius(6)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                            egui::RichText::new(code)
                                .monospace()
                                .color(palette.text_soft),
                        );
                    });
                ui.add_space(4.0);
            }
            Block::Rule => {
                ui.separator();
            }
        }
    }
}

/// A preview that parses the text only when the text changes.
#[derive(Debug, Default)]
pub(crate) struct Preview {
    source: String,
    blocks: Vec<Block>,
    parsed: bool,
}

impl Preview {
    /// Draws the preview of a text.
    pub(crate) fn show(&mut self, ui: &mut Ui, text: &str) {
        if !self.parsed || self.source != text {
            text.clone_into(&mut self.source);
            self.blocks = parse(text);
            self.parsed = true;
        }
        show(ui, &self.blocks);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(spans: &[Span]) -> String {
        spans.iter().map(|span| span.text.as_str()).collect()
    }

    #[test]
    fn headings_paragraphs_and_styles() {
        let blocks = parse("# Title\n\nSome **bold** and *soft* and `code` text.\n");
        let Block::Heading { level, spans } = &blocks[0] else {
            panic!("the first block must be a heading: {blocks:?}");
        };
        assert_eq!(*level, 1);
        assert_eq!(plain(spans), "Title");
        let Block::Paragraph(spans) = &blocks[1] else {
            panic!("the second block must be a paragraph: {blocks:?}");
        };
        assert_eq!(plain(spans), "Some bold and soft and code text.");
        assert!(spans.iter().any(|span| span.strong && span.text == "bold"));
        assert!(
            spans
                .iter()
                .any(|span| span.emphasis && span.text == "soft")
        );
        assert!(spans.iter().any(|span| span.code && span.text == "code"));
    }

    #[test]
    fn lists_and_task_lists() {
        let blocks = parse("- one\n  - two\n- [x] done\n- [ ] open\n\n3. three\n4. four\n");
        let items: Vec<(usize, Marker, String)> = blocks
            .iter()
            .filter_map(|block| match block {
                Block::Item {
                    depth,
                    marker,
                    spans,
                } => Some((*depth, marker.clone(), plain(spans))),
                _ => None,
            })
            .collect();
        assert_eq!(
            items,
            vec![
                (0, Marker::Bullet, "one".to_owned()),
                (1, Marker::Bullet, "two".to_owned()),
                (0, Marker::Task(true), "done".to_owned()),
                (0, Marker::Task(false), "open".to_owned()),
                (0, Marker::Number(3), "three".to_owned()),
                (0, Marker::Number(4), "four".to_owned()),
            ]
        );
    }

    #[test]
    fn code_blocks_keep_their_text() {
        let blocks = parse("```rust\nfn main() {}\n\nlet a = 1;\n```\n");
        assert_eq!(
            blocks,
            vec![Block::Code("fn main() {}\n\nlet a = 1;".to_owned())]
        );
    }

    #[test]
    fn links_and_html_stay_text() {
        let blocks =
            parse("[click](https://example.invalid) <b>x</b>\n\n<script>alert(1)</script>\n");
        let all = format!("{blocks:?}");
        assert!(all.contains("click"));
        assert!(all.contains("(https://example.invalid)"));
        assert!(all.contains("script"), "HTML must show as text: {all}");
    }

    #[test]
    fn quotes_and_rules() {
        let blocks = parse("> note\n\n---\n");
        assert!(matches!(&blocks[0], Block::Quote(spans) if plain(spans) == "note"));
        assert_eq!(blocks[1], Block::Rule);
    }

    #[test]
    fn empty_text_gives_no_blocks() {
        assert!(parse("  \n").is_empty());
    }
}
