//! Rendering a transcript as LaTeX and compiling it to PDF with LuaLaTeX.
//!
//! Prompts are typeset verbatim in a framed box, so their line breaks and
//! indentation are kept. Answers are Markdown, converted to LaTeX. The fonts
//! are Charter and Menlo with a fallback chain (Menlo, Apple Symbols, Arial
//! Unicode MS) for the arrows, box-drawing characters and other symbols that
//! Claude uses. A character that no font has is dropped with a warning.

use crate::transcript::{local_time, Transcript};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::fs;
use std::path::Path;
use std::process::Command;

const PREAMBLE: &str = r#"\documentclass[11pt]{article}
\usepackage[a4paper,margin=2.2cm]{geometry}
\usepackage{fontspec}
\directlua{luaotfload.add_fallback("transcriptfallback",
  {"Menlo:mode=node;", "Apple Symbols:mode=node;", "Arial Unicode MS:mode=node;"})}
\setmainfont{Charter}[Ligatures=TeXOff,RawFeature={fallback=transcriptfallback}]
\setmonofont{Menlo}[Scale=MatchLowercase,RawFeature={fallback=transcriptfallback}]
\usepackage{xcolor}
\usepackage{fvextra}
\usepackage[most]{tcolorbox}
\usepackage{longtable,array,booktabs}
\usepackage{enumitem}
\usepackage[normalem]{ulem}
\usepackage{parskip}
\usepackage[colorlinks,urlcolor=linkcolor,linkcolor=linkcolor]{hyperref}
\definecolor{linkcolor}{HTML}{1F5FA8}
\definecolor{codecolor}{HTML}{2E6B30}
\definecolor{promptframe}{HTML}{B8860B}
\definecolor{promptback}{HTML}{FFF8E6}
\definecolor{answercolor}{HTML}{1B7A8C}
\definecolor{metacolor}{HTML}{6B6B6B}
\newcommand{\code}[1]{\texttt{\textcolor{codecolor}{#1}}}
\tcbset{codebox/.style={breakable,enhanced,colback=black!4,colframe=black!15,
  boxrule=0.4pt,arc=1mm,left=1mm,right=1mm,top=0.5mm,bottom=0.5mm}}
\setlist{itemsep=2pt,topsep=3pt,parsep=0pt}
\setlength{\parskip}{0.6em}
\setlength{\emergencystretch}{3em}
\begin{document}
"#;

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str(r"\textbackslash{}"),
            '{' => out.push_str(r"\{"),
            '}' => out.push_str(r"\}"),
            '$' | '&' | '#' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            '~' => out.push_str(r"\textasciitilde{}"),
            '^' => out.push_str(r"\textasciicircum{}"),
            '\t' => out.push_str("    "),
            c if c.is_control() && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

/// Inline code: escaped, with break points after path and identifier separators
/// so that long paths do not run into the margin.
fn escape_code(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        out.push_str(&escape(&ch.to_string()));
        if matches!(ch, '/' | '.' | '_' | '-' | ':' | ',' | '=') {
            out.push_str(r"\allowbreak{}");
        }
    }
    out
}

fn escape_url(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if matches!(ch, '#' | '%' | '\\' | '{' | '}') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

fn verbatim(text: &str) -> String {
    // The environment ends wherever \end{Verbatim} appears, so break that string
    // with an invisible zero-width space.
    let body: Vec<String> = text
        .trim_end_matches('\n')
        .lines()
        .map(|l| l.replace('\t', "    ").replace(r"\end{Verbatim}", "\\end\u{200B}{Verbatim}"))
        .collect();
    format!(
        "\\begin{{Verbatim}}[breaklines,breakanywhere,fontsize=\\small]\n{}\n\\end{{Verbatim}}\n",
        body.join("\n")
    )
}

struct Cell {
    latex: String,
    length: usize,
    longest_word: usize,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<Cell>>,
    header_rows: usize,
}

struct Markdown {
    out: String,
    code_block: Option<String>,
    table: Option<Table>,
}

impl Markdown {
    fn push(&mut self, latex: &str, length: usize) {
        if let Some(cell) = self.table.as_mut().and_then(|t| t.rows.last_mut()).and_then(|r| r.last_mut()) {
            cell.latex.push_str(latex);
            cell.length += length;
        } else {
            self.out.push_str(latex);
        }
    }

    /// Text that may not be broken inside a table cell (inline code), so the
    /// column must be at least that wide.
    fn note_word(&mut self, text: &str) {
        if let Some(cell) = self.table.as_mut().and_then(|t| t.rows.last_mut()).and_then(|r| r.last_mut()) {
            let longest = text.split_whitespace().map(|w| w.chars().count()).max().unwrap_or(0);
            cell.longest_word = cell.longest_word.max(longest);
        }
    }

    fn render(markdown: &str) -> String {
        let mut m = Markdown { out: String::new(), code_block: None, table: None };
        let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
        for event in Parser::new_ext(markdown, options) {
            m.event(event);
        }
        m.out
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) | Event::Html(t) | Event::InlineHtml(t) => {
                if let Some(code) = self.code_block.as_mut() {
                    code.push_str(&t);
                } else {
                    self.note_word(&t);
                    self.push(&escape(&t), t.chars().count());
                }
            }
            Event::Code(t) => {
                self.note_word(&t);
                self.push(&format!("\\code{{{}}}", escape_code(&t)), t.chars().count());
            }
            Event::SoftBreak => self.push("\n", 1),
            Event::HardBreak => self.push("\\newline\n", 0),
            Event::Rule => self.push("\n\\noindent\\rule{\\linewidth}{0.4pt}\n\n", 0),
            Event::TaskListMarker(done) => self.push(if done { "☑ " } else { "☐ " }, 2),
            Event::FootnoteReference(name) => self.push(&format!("[{}]", escape(&name)), name.len() + 2),
            Event::InlineMath(t) | Event::DisplayMath(t) => self.push(&escape(&t), t.chars().count()),
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Heading { level, .. } => {
                let command = match level {
                    HeadingLevel::H1 | HeadingLevel::H2 => "subsection",
                    _ => "subsubsection",
                };
                self.push(&format!("\n\\{command}*{{"), 0);
            }
            Tag::BlockQuote(_) => self.push("\\begin{quote}\n", 0),
            Tag::CodeBlock(_) => self.code_block = Some(String::new()),
            Tag::List(Some(start)) => self.push(&format!("\\begin{{enumerate}}[start={start}]\n"), 0),
            Tag::List(None) => self.push("\\begin{itemize}\n", 0),
            // The braces stop a following "[" from being read as the item label.
            Tag::Item => self.push("\\item{} ", 0),
            Tag::Emphasis => self.push("\\emph{", 0),
            Tag::Strong => self.push("\\textbf{", 0),
            Tag::Strikethrough => self.push("\\sout{", 0),
            Tag::Link { dest_url, .. } => self.push(&format!("\\href{{{}}}{{", escape_url(&dest_url)), 0),
            Tag::Image { .. } => self.push("[image: ", 8),
            Tag::Table(_) => self.table = Some(Table::default()),
            Tag::TableHead | Tag::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    t.rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                if let Some(row) = self.table.as_mut().and_then(|t| t.rows.last_mut()) {
                    row.push(Cell { latex: "{}".to_string(), length: 0, longest_word: 0 });
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.push("\n\n", 0),
            TagEnd::Heading(_) => self.push("}\n", 0),
            TagEnd::BlockQuote(_) => self.push("\\end{quote}\n", 0),
            TagEnd::CodeBlock => {
                let code = self.code_block.take().unwrap_or_default();
                let block = format!("\\begin{{tcolorbox}}[codebox]\n{}\\end{{tcolorbox}}\n", verbatim(&code));
                self.push(&block, 0);
            }
            TagEnd::List(true) => self.push("\\end{enumerate}\n", 0),
            TagEnd::List(false) => self.push("\\end{itemize}\n", 0),
            TagEnd::Item => self.push("\n", 0),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => self.push("}", 0),
            TagEnd::Image => self.push("]", 1),
            TagEnd::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.header_rows = t.rows.len();
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.out.push_str(&longtable(t));
                }
            }
            _ => {}
        }
    }
}

/// About how many characters of the table's small font fit on one line.
const CHARS_PER_LINE: f64 = 85.0;

/// Column widths follow the length of the longest cell of each column, clamped
/// so that a column with one long cell does not squeeze the others to nothing.
/// A column is never narrower than its longest unbreakable word.
fn longtable(t: Table) -> String {
    let columns = t.rows.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return String::new();
    }
    let mut weights = vec![3usize; columns];
    let mut minimum = vec![0.0f64; columns];
    for row in &t.rows {
        for (i, cell) in row.iter().enumerate() {
            weights[i] = weights[i].max(cell.length.clamp(3, 60));
            minimum[i] = minimum[i].max((cell.longest_word as f64 + 3.0) / CHARS_PER_LINE);
        }
    }
    let total: usize = weights.iter().sum();
    let mut fractions: Vec<f64> = weights.iter().map(|w| 0.98 * *w as f64 / total as f64).collect();
    let widened: Vec<bool> = (0..columns).map(|i| fractions[i] < minimum[i]).collect();
    let fixed: f64 = (0..columns).filter(|&i| widened[i]).map(|i| minimum[i]).sum();
    let free: f64 = (0..columns).filter(|&i| !widened[i]).map(|i| fractions[i]).sum();
    if fixed > 0.0 && fixed < 0.98 && free > 0.0 {
        for i in 0..columns {
            fractions[i] = if widened[i] { minimum[i] } else { fractions[i] * (0.98 - fixed) / free };
        }
    }
    let spec: String = fractions
        .iter()
        .map(|fraction| {
            format!(">{{\\raggedright\\arraybackslash}}p{{\\dimexpr {fraction:.3}\\linewidth-2\\tabcolsep\\relax}}")
        })
        .collect();
    let mut out = format!("\n{{\\small\n\\begin{{longtable}}{{{spec}}}\n\\toprule\n");
    for (r, row) in t.rows.iter().enumerate() {
        let header = r < t.header_rows;
        let cells: Vec<String> = (0..columns)
            .map(|i| match row.get(i) {
                Some(c) if header => format!("\\textbf{{{}}}", c.latex),
                Some(c) => c.latex.clone(),
                None => String::new(),
            })
            .collect();
        out.push_str(&cells.join(" & "));
        out.push_str(" \\\\\n");
        if header && r + 1 == t.header_rows {
            out.push_str("\\midrule\n\\endhead\n");
        }
    }
    out.push_str("\\bottomrule\n\\end{longtable}\n}\n");
    out
}

/// Text for a PDF bookmark: the first words of the prompt, without TeX specials.
fn bookmark(prompt: &str) -> String {
    let words: String = prompt
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !"\\{}$&#^_%~".contains(*c))
        .collect();
    let words: Vec<&str> = words.split_whitespace().collect();
    let mut text = words.iter().take(8).cloned().collect::<Vec<_>>().join(" ");
    if words.len() > 8 {
        text.push_str(" …");
    }
    text
}

pub fn render(t: &Transcript, source: &str, answers: &dyn Fn(&[String]) -> Vec<String>) -> String {
    let mut out = String::from(PREAMBLE);
    let title = t.title.clone().unwrap_or_else(|| source.to_string());
    out.push_str(&format!("{{\\LARGE\\bfseries {}\\par}}\n\\medskip\n", escape(&title)));
    let mut info = vec![format!("Transcript: {}", escape(source))];
    if let Some(cwd) = &t.cwd {
        info.push(format!("Directory: {}", escape_code(cwd)));
    }
    info.push(format!("Session: {}", escape(&t.session_ids.join(", "))));
    if let (Some(first), Some(last)) = (t.turns.first(), t.turns.last()) {
        info.push(format!("Date: {} → {}", local_time(&first.timestamp), local_time(&last.timestamp)));
    }
    out.push_str(&format!("{{\\small\\color{{metacolor}}{}\\par}}\n\\bigskip\n", info.join("\\\\\n")));

    for (n, turn) in t.turns.iter().enumerate() {
        let number = n + 1;
        out.push_str(&format!(
            "\n\\pdfbookmark[1]{{{number}. {}}}{{prompt{number}}}\n",
            bookmark(&turn.prompt)
        ));
        out.push_str(&format!(
            "\\begin{{tcolorbox}}[breakable,enhanced,colback=promptback,colframe=promptframe,\
             fonttitle=\\bfseries,title={{Prompt {number}\\hfill\\mdseries {}}}]\n",
            local_time(&turn.timestamp)
        ));
        out.push_str(&verbatim(&turn.prompt));
        out.push_str("\\end{tcolorbox}\n\n");
        let selected = answers(&turn.answers);
        if selected.is_empty() {
            out.push_str("{\\color{metacolor}\\itshape (no answer)}\n\n");
            continue;
        }
        out.push_str("{\\color{answercolor}\\bfseries Answer}\\par\n");
        for (i, answer) in selected.iter().enumerate() {
            if i > 0 {
                out.push_str("\n\\begin{center}{\\color{metacolor}⋯}\\end{center}\n");
            }
            out.push_str(&Markdown::render(answer));
        }
        out.push_str("\n\\bigskip\n");
    }
    out.push_str("\\end{document}\n");
    out
}

/// Compile `tex` with LuaLaTeX in a temporary directory and move the PDF to `pdf`.
pub fn compile(tex: &str, pdf: &Path) -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    fs::write(dir.path().join("transcript.tex"), tex).map_err(|e| e.to_string())?;
    let log_path = dir.path().join("transcript.log");
    for _ in 0..2 {
        let status = Command::new("lualatex")
            .args(["-interaction=nonstopmode", "-halt-on-error", "-file-line-error", "transcript.tex"])
            .current_dir(dir.path())
            .output()
            .map_err(|e| format!("cannot run lualatex: {e}"))?;
        let log = fs::read_to_string(&log_path).unwrap_or_default();
        if !status.status.success() {
            let tail: Vec<&str> = log.lines().rev().take(25).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            return Err(format!("lualatex failed:\n{}", tail.join("\n")));
        }
        if !log.contains("Rerun") {
            break;
        }
    }
    fs::copy(dir.path().join("transcript.pdf"), pdf).map_err(|e| e.to_string())?;
    Ok(())
}
