# Claude_tooling
Tools for working with Claude.

## claude_transcript

Run Claude Code and keep a copy of the session transcript next to your work:

    claude_transcript FILE.jsonl [claude arguments...]

FILE.jsonl is updated after every answer. After a `/clear` (or a `/resume` of
another session) the previous transcript is moved to FILE_1.jsonl, FILE_2.jsonl,
and so on. The global Claude Code settings are not modified: the hooks are passed
with `--settings` for this session only.

## process_transcripts

Turn the transcripts into text and PDF. In a folder with .jsonl files, run:

    process_transcripts

For each FILE.jsonl it writes FILE.txt (colored, for `less -R`) and FILE.pdf
(through LuaLaTeX). Files that are already up to date are skipped. By default
each answer keeps only its last text block, the conclusion; `--all-answers`
keeps every text block. Tool calls and thinking are never included. See
`process_transcripts --help` for the other options.

Install with `cargo install --path process_transcripts`.
