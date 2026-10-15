//! process_transcripts: turn Claude Code transcripts (.jsonl) into .txt and .pdf.

mod latex;
mod text;
mod transcript;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
usage: process_transcripts [options] [FILE.jsonl | DIRECTORY]...

Writes FILE.txt and FILE.pdf next to each FILE.jsonl. Without arguments, it
processes every .jsonl file of the current directory. Files whose .txt and .pdf
are newer than the .jsonl are skipped.

For each prompt, only the last text block of the answer is kept: that is the
conclusion, the earlier blocks are narration between tool calls. Tool calls,
tool results and thinking are never included.

options:
  -a, --all-answers   keep every text block of each answer
  -f, --force         rebuild even when the outputs are up to date
  -w, --width N       wrap the text at N columns (default 100)
      --no-color      plain text without ANSI styles
      --no-pdf        write only the .txt
      --keep-tex      also keep FILE.tex
  -h, --help          show this help

View the text with `less -R FILE.txt` (plain `less` is enough when LESS=-R).";

struct Config {
    all_answers: bool,
    force: bool,
    width: usize,
    color: bool,
    pdf: bool,
    keep_tex: bool,
    inputs: Vec<PathBuf>,
}

fn parse_args() -> Result<Config, String> {
    let mut c = Config {
        all_answers: false,
        force: false,
        width: 100,
        color: true,
        pdf: true,
        keep_tex: false,
        inputs: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-a" | "--all-answers" => c.all_answers = true,
            "-f" | "--force" => c.force = true,
            "-w" | "--width" => {
                let value = args.next().ok_or("--width needs a value")?;
                c.width = value.parse().map_err(|_| format!("bad width: {value}"))?;
            }
            "--no-color" => c.color = false,
            "--no-pdf" => c.pdf = false,
            "--keep-tex" => c.keep_tex = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            a if a.starts_with('-') => return Err(format!("unknown option: {a}")),
            a => c.inputs.push(PathBuf::from(a)),
        }
    }
    if c.inputs.is_empty() {
        c.inputs.push(PathBuf::from("."));
    }
    Ok(c)
}

fn jsonl_files(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for input in inputs {
        if input.is_dir() {
            let entries = fs::read_dir(input).map_err(|e| format!("{}: {e}", input.display()))?;
            let mut found: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl") && p.is_file())
                .collect();
            found.sort();
            files.extend(found);
        } else if input.is_file() {
            files.push(input.clone());
        } else {
            return Err(format!("{}: no such file or directory", input.display()));
        }
    }
    Ok(files)
}

fn is_newer(output: &Path, input: &Path) -> bool {
    let modified = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    match (modified(output), modified(input)) {
        (Some(o), Some(i)) => o >= i,
        _ => false,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().to_string()
}

fn process(path: &Path, c: &Config) -> Result<String, String> {
    let txt = path.with_extension("txt");
    let pdf = path.with_extension("pdf");
    if !c.force && is_newer(&txt, path) && (!c.pdf || is_newer(&pdf, path)) {
        return Ok("up to date".to_string());
    }
    let t = transcript::read(path)?;
    let source = file_name(path);
    let all = c.all_answers;
    let answers = move |blocks: &[String]| -> Vec<String> {
        if all {
            blocks.to_vec()
        } else {
            blocks.last().cloned().into_iter().collect()
        }
    };

    fs::write(&txt, text::render(&t, &source, &answers, c.width, c.color)).map_err(|e| e.to_string())?;
    let mut written = vec![file_name(&txt)];
    if c.pdf {
        let tex = latex::render(&t, &source, &answers);
        let tex_path = path.with_extension("tex");
        let result = latex::compile(&tex, &pdf);
        if c.keep_tex || result.is_err() {
            fs::write(&tex_path, &tex).map_err(|e| e.to_string())?;
        }
        if let Err(e) = result {
            return Err(format!("{e}\n(the LaTeX source is in {})", tex_path.display()));
        }
        written.push(file_name(&pdf));
    }
    Ok(format!("{} prompts → {}", t.turns.len(), written.join(", ")))
}

fn main() -> ExitCode {
    let config = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("process_transcripts: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let files = match jsonl_files(&config.inputs) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("process_transcripts: {e}");
            return ExitCode::from(2);
        }
    };
    if files.is_empty() {
        eprintln!("process_transcripts: no .jsonl file found");
        return ExitCode::from(1);
    }
    let mut failed = false;
    for f in &files {
        match process(f, &config) {
            Ok(message) => println!("{}: {message}", f.display()),
            Err(e) => {
                eprintln!("{}: {e}", f.display());
                failed = true;
            }
        }
    }
    if failed { ExitCode::from(1) } else { ExitCode::SUCCESS }
}
