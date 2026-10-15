//! Rendering a transcript as text for `less`: Markdown is turned into ANSI
//! styles (bold, italic, colors), paragraphs are wrapped, lists keep their
//! hanging indent and tables are drawn with box characters.

use crate::transcript::{local_time, Transcript};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthStr;

const BREAK: &str = "\n";

#[derive(Clone, Copy, Default, PartialEq)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
    underline: bool,
    dim: bool,
    color: Option<u8>,
}

const YELLOW: u8 = 33;
const BLUE: u8 = 34;
const MAGENTA: u8 = 35;
const CYAN: u8 = 36;
const GREEN: u8 = 32;

impl Style {
    fn color(c: u8) -> Style {
        Style { color: Some(c), ..Style::default() }
    }
    fn dim() -> Style {
        Style { dim: true, ..Style::default() }
    }
    fn bold(self) -> Style {
        Style { bold: true, ..self }
    }
    fn sgr(&self) -> String {
        let mut codes = Vec::new();
        if self.bold {
            codes.push("1".to_string());
        }
        if self.dim {
            codes.push("2".to_string());
        }
        if self.italic {
            codes.push("3".to_string());
        }
        if self.underline {
            codes.push("4".to_string());
        }
        if self.strike {
            codes.push("9".to_string());
        }
        if let Some(c) = self.color {
            codes.push(c.to_string());
        }
        codes.join(";")
    }
}

type Piece = (String, Style);

fn width_of(pieces: &[Piece]) -> usize {
    pieces.iter().map(|(t, _)| t.width()).sum()
}

fn paint(pieces: &[Piece], color: bool) -> String {
    let mut s = String::new();
    for (text, style) in pieces {
        let sgr = style.sgr();
        if color && !sgr.is_empty() {
            s.push_str(&format!("\x1b[{sgr}m{text}\x1b[0m"));
        } else {
            s.push_str(text);
        }
    }
    s
}

/// Split styled text into lines of at most `width` columns, breaking at spaces.
/// A word longer than the width gets a line of its own.
fn wrap(pieces: &[Piece], width: usize) -> Vec<Vec<Piece>> {
    enum Token {
        Word(Vec<Piece>),
        Break,
    }
    let mut tokens = Vec::new();
    let mut word: Vec<Piece> = Vec::new();
    let end_word = |word: &mut Vec<Piece>, tokens: &mut Vec<Token>| {
        if !word.is_empty() {
            tokens.push(Token::Word(std::mem::take(word)));
        }
    };
    for (text, style) in pieces {
        if text == BREAK {
            end_word(&mut word, &mut tokens);
            tokens.push(Token::Break);
            continue;
        }
        for ch in text.chars() {
            if ch.is_whitespace() {
                end_word(&mut word, &mut tokens);
            } else {
                match word.last_mut() {
                    Some((t, s)) if s == style => t.push(ch),
                    _ => word.push((ch.to_string(), *style)),
                }
            }
        }
    }
    end_word(&mut word, &mut tokens);

    let mut lines = Vec::new();
    let mut line: Vec<Piece> = Vec::new();
    let mut used = 0;
    for token in tokens {
        match token {
            Token::Break => {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            Token::Word(w) => {
                let w_width = width_of(&w);
                if used > 0 && used + 1 + w_width > width {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
                if used > 0 {
                    line.push((" ".to_string(), Style::default()));
                    used += 1;
                }
                used += w_width;
                line.extend(w);
            }
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

enum Frame {
    Quote,
    Item { indent: usize, marker: Option<String> },
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<Vec<Piece>>>,
    header_rows: usize,
}

struct Markdown {
    out: String,
    width: usize,
    color: bool,
    bold: u32,
    italic: u32,
    strike: u32,
    link: u32,
    heading: Option<HeadingLevel>,
    inline: Vec<Piece>,
    frames: Vec<Frame>,
    lists: Vec<Option<u64>>,
    need_blank: bool,
    code_block: Option<String>,
    table: Option<Table>,
    links: Vec<(String, usize)>,
}

impl Markdown {
    fn new(width: usize, color: bool) -> Markdown {
        Markdown {
            out: String::new(),
            width,
            color,
            bold: 0,
            italic: 0,
            strike: 0,
            link: 0,
            heading: None,
            inline: Vec::new(),
            frames: Vec::new(),
            lists: Vec::new(),
            need_blank: false,
            code_block: None,
            table: None,
            links: Vec::new(),
        }
    }

    fn style(&self) -> Style {
        let mut s = Style {
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            ..Style::default()
        };
        if self.link > 0 {
            s.color = Some(BLUE);
            s.underline = true;
        }
        if self.heading.is_some() {
            s.bold = true;
            s.color = Some(MAGENTA);
        }
        s
    }

    fn push(&mut self, text: &str, style: Style) {
        if let Some(table) = self.table.as_mut() {
            if let Some(cell) = table.rows.last_mut().and_then(|r| r.last_mut()) {
                cell.push((text.to_string(), style));
            }
        } else {
            self.inline.push((text.to_string(), style));
        }
    }

    fn prefix_width(&self) -> usize {
        self.frames
            .iter()
            .map(|f| match f {
                Frame::Quote => 2,
                Frame::Item { indent, .. } => *indent,
            })
            .sum()
    }

    fn available(&self) -> usize {
        self.width.saturating_sub(self.prefix_width()).max(20)
    }

    /// The prefix of the next line. The first line of a list item shows its marker.
    fn line_prefix(&mut self) -> Vec<Piece> {
        let mut prefix = Vec::new();
        for f in self.frames.iter_mut() {
            match f {
                Frame::Quote => prefix.push(("│ ".to_string(), Style::dim())),
                Frame::Item { indent, marker } => match marker.take() {
                    Some(m) => prefix.push((m, Style::default())),
                    None => prefix.push((" ".repeat(*indent), Style::default())),
                },
            }
        }
        prefix
    }

    fn emit(&mut self, pieces: &[Piece]) {
        let mut line = self.line_prefix();
        line.extend_from_slice(pieces);
        let text = paint(&line, self.color);
        self.out.push_str(text.trim_end());
        self.out.push('\n');
    }

    fn blank_line(&mut self) {
        let bars = self.frames.iter().filter(|f| matches!(f, Frame::Quote)).count();
        let line = paint(&vec![("│".to_string(), Style::dim()); bars], self.color);
        self.out.push_str(&line);
        self.out.push('\n');
    }

    fn flush(&mut self) {
        if self.inline.is_empty() {
            return;
        }
        let pieces = std::mem::take(&mut self.inline);
        for line in wrap(&pieces, self.available()) {
            self.emit(&line);
        }
    }

    fn start_block(&mut self) {
        self.flush();
        if self.need_blank && !self.out.is_empty() {
            self.blank_line();
        }
        self.need_blank = false;
    }

    fn render(mut self, markdown: &str) -> String {
        let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        for event in Parser::new_ext(markdown, options) {
            self.event(event);
        }
        self.flush();
        self.out
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => {
                if let Some(code) = self.code_block.as_mut() {
                    code.push_str(&t);
                } else {
                    let style = self.style();
                    self.push(&t, style);
                }
            }
            Event::Code(t) => {
                let style = if self.color {
                    Style { color: Some(GREEN), ..self.style() }
                } else {
                    self.style()
                };
                let text = if self.color { t.to_string() } else { format!("`{t}`") };
                self.push(&text, style);
            }
            Event::SoftBreak => self.push(" ", Style::default()),
            Event::HardBreak => self.push(BREAK, Style::default()),
            Event::Rule => {
                self.start_block();
                let line = "─".repeat(self.available());
                self.emit(&[(line, Style::dim())]);
                self.need_blank = true;
            }
            Event::TaskListMarker(done) => {
                self.push(if done { "☑ " } else { "☐ " }, Style::default());
            }
            Event::FootnoteReference(name) => self.push(&format!("[{name}]"), Style::default()),
            Event::InlineMath(t) | Event::DisplayMath(t) => self.push(&t, self.style()),
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => self.start_block(),
            Tag::Heading { level, .. } => {
                self.start_block();
                self.heading = Some(level);
            }
            Tag::BlockQuote(_) => {
                self.start_block();
                self.frames.push(Frame::Quote);
            }
            Tag::CodeBlock(_) => {
                self.start_block();
                self.code_block = Some(String::new());
            }
            Tag::List(start) => {
                if self.frames.iter().any(|f| matches!(f, Frame::Item { .. })) {
                    self.flush();
                } else {
                    self.start_block();
                }
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let depth = self.lists.len();
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}. ", *n - 1)
                    }
                    _ => if depth % 2 == 1 { "• " } else { "◦ " }.to_string(),
                };
                let indent = marker.width();
                self.frames.push(Frame::Item { indent, marker: Some(marker) });
            }
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                self.link += 1;
                self.links.push((dest_url.to_string(), self.inline.len()));
            }
            Tag::Table(_) => {
                self.start_block();
                self.table = Some(Table::default());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                if let Some(row) = self.table.as_mut().and_then(|t| t.rows.last_mut()) {
                    row.push(Vec::new());
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush();
                self.need_blank = true;
            }
            TagEnd::Heading(level) => {
                let text_width = width_of(&self.inline).min(self.available());
                self.flush();
                self.heading = None;
                let rule = match level {
                    HeadingLevel::H1 => Some("═"),
                    HeadingLevel::H2 => Some("─"),
                    _ => None,
                };
                if let Some(r) = rule {
                    self.emit(&[(r.repeat(text_width), Style::color(MAGENTA))]);
                }
                self.need_blank = true;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.frames.pop();
                self.need_blank = true;
            }
            TagEnd::CodeBlock => {
                let code = self.code_block.take().unwrap_or_default();
                let style = if self.color { Style::color(GREEN) } else { Style::default() };
                for line in code.trim_end_matches('\n').lines() {
                    let line = line.replace('\t', "    ");
                    self.emit(&[("  ".to_string(), Style::default()), (line, style)]);
                }
                self.need_blank = true;
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                self.need_blank = true;
            }
            TagEnd::Item => {
                self.flush();
                self.frames.pop();
            }
            TagEnd::Emphasis => self.italic -= 1,
            TagEnd::Strong => self.bold -= 1,
            TagEnd::Strikethrough => self.strike -= 1,
            TagEnd::Link | TagEnd::Image => {
                self.link -= 1;
                if let Some((url, start)) = self.links.pop() {
                    let shown: String = if self.table.is_none() {
                        self.inline[start.min(self.inline.len())..].iter().map(|(t, _)| t.as_str()).collect()
                    } else {
                        String::new()
                    };
                    if !url.is_empty() && shown.trim() != url && self.table.is_none() {
                        self.push(&format!(" ({url})"), Style::dim());
                    }
                }
            }
            TagEnd::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.header_rows = t.rows.len();
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.draw_table(t);
                }
                self.need_blank = true;
            }
            _ => {}
        }
    }

    fn draw_table(&mut self, table: Table) {
        let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let mut widths = vec![1usize; columns];
        for row in &table.rows {
            for (i, cell) in row.iter().enumerate() {
                widths[i] = widths[i].max(width_of(cell));
            }
        }
        // Shrink the widest columns until the table fits; their cells get wrapped.
        let available = self.available();
        while widths.iter().sum::<usize>() + 3 * columns + 1 > available {
            let (i, &w) = widths.iter().enumerate().max_by_key(|(_, w)| **w).unwrap();
            if w <= 8 {
                break;
            }
            widths[i] -= 1;
        }
        let border = |left: &str, mid: &str, right: &str| -> Vec<Piece> {
            let segments: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
            vec![(format!("{left}{}{right}", segments.join(mid)), Style::dim())]
        };
        self.emit(&border("┌", "┬", "┐"));
        for (r, row) in table.rows.iter().enumerate() {
            let header = r < table.header_rows;
            let cells: Vec<Vec<Vec<Piece>>> = (0..columns)
                .map(|i| {
                    let cell = row.get(i).cloned().unwrap_or_default();
                    let cell: Vec<Piece> = if header {
                        cell.into_iter().map(|(t, s)| (t, s.bold())).collect()
                    } else {
                        cell
                    };
                    wrap(&cell, widths[i])
                })
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            for l in 0..height {
                let mut line = vec![("│".to_string(), Style::dim())];
                for (i, cell) in cells.iter().enumerate() {
                    let content = cell.get(l).cloned().unwrap_or_default();
                    let pad = widths[i].saturating_sub(width_of(&content));
                    line.push((" ".to_string(), Style::default()));
                    line.extend(content);
                    line.push((" ".repeat(pad + 1), Style::default()));
                    line.push(("│".to_string(), Style::dim()));
                }
                self.emit(&line);
            }
            if header && r + 1 == table.header_rows {
                self.emit(&border("├", "┼", "┤"));
            }
        }
        self.emit(&border("└", "┴", "┘"));
    }
}

fn banner(label: &str, width: usize, style: Style, color: bool) -> String {
    let head = format!("━━━ {label} ");
    let rest = width.saturating_sub(head.width());
    paint(&[(format!("{head}{}", "━".repeat(rest)), style)], color) + "\n"
}

pub fn render(t: &Transcript, source: &str, answers: &dyn Fn(&[String]) -> Vec<String>, width: usize, color: bool) -> String {
    let mut out = String::new();
    let title = t.title.clone().unwrap_or_else(|| source.to_string());
    out.push_str(&paint(&[(title, Style::color(MAGENTA).bold())], color));
    out.push('\n');
    let mut info = vec![format!("Transcript: {source}")];
    if let Some(cwd) = &t.cwd {
        info.push(format!("Directory:  {cwd}"));
    }
    info.push(format!("Session:    {}", t.session_ids.join(", ")));
    if let (Some(first), Some(last)) = (t.turns.first(), t.turns.last()) {
        info.push(format!("Date:       {} → {}", local_time(&first.timestamp), local_time(&last.timestamp)));
    }
    for line in info {
        out.push_str(&paint(&[(line, Style::dim())], color));
        out.push('\n');
    }
    for (n, turn) in t.turns.iter().enumerate() {
        out.push('\n');
        let label = format!("Prompt {} · {}", n + 1, local_time(&turn.timestamp));
        out.push_str(&banner(&label, width, Style::color(YELLOW).bold(), color));
        out.push('\n');
        for line in turn.prompt.lines() {
            let bar = paint(&[("▌ ".to_string(), Style::color(YELLOW))], color);
            out.push_str(format!("{bar}{line}").trim_end());
            out.push('\n');
        }
        out.push('\n');
        let selected = answers(&turn.answers);
        if selected.is_empty() {
            out.push_str(&paint(&[("(no answer)".to_string(), Style::dim())], color));
            out.push('\n');
            continue;
        }
        out.push_str(&banner("Answer", width, Style::color(CYAN).bold(), color));
        out.push('\n');
        for (i, answer) in selected.iter().enumerate() {
            if i > 0 {
                out.push_str(&paint(&[("  ⋯".to_string(), Style::dim())], color));
                out.push_str("\n\n");
            }
            out.push_str(&Markdown::new(width, color).render(answer));
            out.push('\n');
        }
    }
    out
}
