use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use clap::Parser;
use notify::RecursiveMode;
use notify_debouncer_mini::{DebouncedEventKind, new_debouncer};

#[derive(Debug, Clone)]
struct Chunk {
    lang: String,
    name: String,
    code: String,
}

#[derive(Debug, Clone)]
enum ChunkOutput {
    Plot { rel_path: String },
    Text { content: String },
    Error { message: String },
}

#[derive(Parser, Debug)]
#[command(name = "typst-run")]
#[command(about = "Process Typst documents with embedded R code chunks")]
struct Args {
    #[arg(help = "Path to source .typ file")]
    source: PathBuf,

    #[arg(short, long, help = "Watch for changes and re-process")]
    watch: bool,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("typst-run: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse();
    let source_path = absolutize(&args.source)?;

    if args.watch {
        run_watch(&source_path)
    } else {
        run_once(&source_path)
    }
}

fn run_once(source_path: &Path) -> Result<(), String> {
    let source = fs::read_to_string(source_path)
        .map_err(|e| format!("failed to read {}: {e}", source_path.display()))?;

    let source_dir = source_path.parent().ok_or_else(|| {
        format!(
            "cannot derive parent directory for {}",
            source_path.display()
        )
    })?;

    let build_dir = source_dir.join("_build");
    let fig_dir = build_dir.join("fig");
    fs::create_dir_all(&fig_dir)
        .map_err(|e| format!("failed to create {}: {e}", fig_dir.display()))?;

    let (doc_without_chunks, chunks) = parse_chunks(&source)?;

    let mut outputs: HashMap<String, ChunkOutput> = HashMap::new();
    for chunk in &chunks {
        let out = run_chunk(chunk, &build_dir);
        outputs.insert(chunk.name.clone(), out);
    }

    let enriched = render_outputs(&doc_without_chunks, &outputs)?;
    let enriched_path = build_dir.join("enriched.typ");
    write_atomic(&enriched_path, &enriched)?;

    println!("Wrote {}", enriched_path.display());
    Ok(())
}

fn run_watch(source_path: &Path) -> Result<(), String> {
    println!("Watching {} for changes...", source_path.display());

    let (tx, rx) = std::sync::mpsc::channel();

    let mut debouncer = new_debouncer(Duration::from_millis(100), tx)
        .map_err(|e| format!("failed to create watcher: {e}"))?;

    debouncer
        .watcher()
        .watch(source_path, RecursiveMode::NonRecursive)
        .map_err(|e| format!("failed to watch {}: {e}", source_path.display()))?;

    run_once(source_path)?;

    loop {
        match rx.recv() {
            Ok(Ok(events)) => {
                if events.iter().any(|e| e.kind == DebouncedEventKind::Any) {
                    println!("\n--- File changed, re-processing ---");
                    if let Err(e) = run_once(source_path) {
                        eprintln!("typst-run: {e}");
                    }
                }
            }
            Ok(Err(e)) => {
                eprintln!("Watch error: {e}");
            }
            Err(_) => break,
        }
    }

    Ok(())
}

fn parse_chunks(input: &str) -> Result<(String, Vec<Chunk>), String> {
    let mut stripped = String::with_capacity(input.len());
    let mut chunks = Vec::new();
    let mut names = HashSet::new();

    let mut cursor = 0;
    while let Some(rel) = input[cursor..].find("#codechunk(") {
        let start = cursor + rel;
        stripped.push_str(&input[cursor..start]);

        let (chunk, end) = parse_codechunk_at(input, start)?;

        if chunk.lang != "r" {
            return Err(format!(
                "unsupported language {:?} for chunk {:?}; MVP supports only \"r\"",
                chunk.lang, chunk.name
            ));
        }

        if !is_valid_chunk_name(&chunk.name) {
            return Err(format!(
                "invalid chunk name {:?}; use only ASCII letters/digits plus '-', '_' or '.'",
                chunk.name
            ));
        }

        if !names.insert(chunk.name.clone()) {
            return Err(format!("duplicate chunk name {:?}", chunk.name));
        }

        chunks.push(chunk);
        cursor = end;
    }

    stripped.push_str(&input[cursor..]);
    Ok((stripped, chunks))
}

fn parse_codechunk_at(input: &str, start: usize) -> Result<(Chunk, usize), String> {
    let mut i = start + "#codechunk(".len();

    i = skip_ws(input, i);
    let (lang, next) = parse_quoted_string(input, i)?;
    i = skip_ws(input, next);

    i = expect_char(input, i, ',', "expected ',' after language")?;
    i = skip_ws(input, i);

    let (name, next) = parse_quoted_string(input, i)?;
    i = skip_ws(input, next);

    i = expect_char(input, i, ')', "expected ')' after chunk name")?;
    i = skip_ws(input, i);

    i = expect_char(input, i, '[', "expected '[' before chunk body")?;
    let open_bracket = i - 1;
    let body_end = find_matching_bracket(input, open_bracket)?;
    let code = input[i..body_end].to_string();

    Ok((Chunk { lang, name, code }, body_end + 1))
}

fn render_outputs(doc: &str, outputs: &HashMap<String, ChunkOutput>) -> Result<String, String> {
    let mut out = String::with_capacity(doc.len());
    let mut cursor = 0;

    while let Some(rel) = doc[cursor..].find("#codeoutput(") {
        let start = cursor + rel;
        out.push_str(&doc[cursor..start]);

        let (name, end) = parse_codeoutput_at(doc, start)?;
        let replacement = match outputs.get(&name) {
            Some(chunk_out) => render_chunk_output(&name, chunk_out),
            None => render_missing_output(&name),
        };

        out.push_str(&replacement);
        cursor = end;
    }

    out.push_str(&doc[cursor..]);
    Ok(out)
}

fn parse_codeoutput_at(input: &str, start: usize) -> Result<(String, usize), String> {
    let mut i = start + "#codeoutput(".len();

    i = skip_ws(input, i);
    let (name, next) = parse_quoted_string(input, i)?;
    i = skip_ws(input, next);

    i = expect_char(input, i, ')', "expected ')' after codeoutput name")?;
    Ok((name, i))
}

fn run_chunk(chunk: &Chunk, build_dir: &Path) -> ChunkOutput {
    let fig_dir = build_dir.join("fig");
    let script_path = build_dir.join(format!("chunk_{}.R", chunk.name));
    let plot_path = fig_dir.join(format!("{}.png", chunk.name));

    let _ = fs::remove_file(&plot_path);

    let script = build_r_wrapper_script(chunk, &plot_path);

    if let Err(e) = fs::write(&script_path, script) {
        return ChunkOutput::Error {
            message: format!("failed to write R script {}: {e}", script_path.display()),
        };
    }

    let output = match Command::new("Rscript").arg(&script_path).output() {
        Ok(o) => o,
        Err(e) => {
            return ChunkOutput::Error {
                message: format!("failed to run Rscript: {e}"),
            };
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let has_plot = fs::metadata(&plot_path)
        .map(|m| m.len() > 0)
        .unwrap_or(false);

    if has_plot {
        return ChunkOutput::Plot {
            rel_path: format!("fig/{}.png", chunk.name),
        };
    }

    if !output.status.success() {
        return ChunkOutput::Error {
            message: combine_streams(stdout.as_ref(), stderr.as_ref()),
        };
    }

    ChunkOutput::Text {
        content: combine_streams(stdout.as_ref(), stderr.as_ref()),
    }
}

fn build_r_wrapper_script(chunk: &Chunk, plot_path: &Path) -> String {
    let plot = escape_r_string(&plot_path.to_string_lossy());
    let code = escape_r_string(&chunk.code);

    format!(
        r#"options(device = function(...) grDevices::png(filename = "{plot}", width = 7, height = 5, units = "in", res = 600 ))
.code <- "{code}"

tryCatch({{
  .exprs <- parse(text = .code)
  for (.expr in .exprs) {{
    .value <- withVisible(eval(.expr, envir = .GlobalEnv))
    if (.value$visible) print(.value$value)
  }}
  if (grDevices::dev.cur() != 1) grDevices::dev.off()
}}, error = function(e) {{
  if (grDevices::dev.cur() != 1) grDevices::dev.off()
  message("ERROR: ", conditionMessage(e))
  quit(status = 1)
}})
"#
    )
}

fn render_chunk_output(name: &str, output: &ChunkOutput) -> String {
    match output {
        ChunkOutput::Plot { rel_path } => format!(
            "#figure(\n  image(\"{}\"),\n  caption: [{}],\n)",
            escape_typst_string(rel_path),
            escape_typst_text(name)
        ),
        ChunkOutput::Text { content } => {
            if content.trim().is_empty() {
                "#block[]".to_string()
            } else {
                format!("#block[\n{}\n]", escape_typst_text(content))
            }
        }
        ChunkOutput::Error { message } => format!(
            "#block(fill: red.lighten(80%))[\nR error in chunk \"{}\":\n{}\n]",
            escape_typst_text(name),
            escape_typst_text(message)
        ),
    }
}

fn render_missing_output(name: &str) -> String {
    format!(
        "#block(fill: red.lighten(80%))[\nMissing chunk \"{}\" for #codeoutput.\n]",
        escape_typst_text(name)
    )
}

fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", path.display()))?;

    fs::create_dir_all(parent)
        .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;

    let tmp_path = parent.join("enriched.tmp.typ");
    fs::write(&tmp_path, content)
        .map_err(|e| format!("failed to write {}: {e}", tmp_path.display()))?;

    fs::rename(&tmp_path, path).map_err(|e| {
        format!(
            "failed to rename {} -> {}: {e}",
            tmp_path.display(),
            path.display()
        )
    })?;

    Ok(())
}

fn escape_typst_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '[' => out.push_str("\\["),
            ']' => out.push_str("\\]"),
            '#' => out.push_str("\\#"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_typst_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_r_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

fn combine_streams(stdout: &str, stderr: &str) -> String {
    let a = stdout.trim_end();
    let b = stderr.trim_end();

    match (a.is_empty(), b.is_empty()) {
        (true, true) => String::new(),
        (false, true) => a.to_string(),
        (true, false) => b.to_string(),
        (false, false) => format!("{a}\n{b}"),
    }
}

fn is_valid_chunk_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn skip_ws(input: &str, mut i: usize) -> usize {
    while i < input.len() {
        let ch = input[i..].chars().next().unwrap();
        if ch.is_whitespace() {
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    i
}

fn parse_quoted_string(input: &str, mut i: usize) -> Result<(String, usize), String> {
    let Some(first) = input.get(i..).and_then(|s| s.chars().next()) else {
        return Err(parse_error(input, i, "expected string literal"));
    };

    if first != '"' {
        return Err(parse_error(input, i, "expected opening '\"'"));
    }

    i += first.len_utf8();
    let mut out = String::new();
    let mut escaped = false;

    while i < input.len() {
        let ch = input[i..].chars().next().unwrap();
        let len = ch.len_utf8();

        if escaped {
            let real = match ch {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '"' => '"',
                '\\' => '\\',
                other => other,
            };
            out.push(real);
            escaped = false;
            i += len;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '"' => {
                i += len;
                return Ok((out, i));
            }
            _ => out.push(ch),
        }

        i += len;
    }

    Err(parse_error(input, i, "unterminated string literal"))
}

fn expect_char(input: &str, i: usize, expected: char, message: &str) -> Result<usize, String> {
    let Some(ch) = input.get(i..).and_then(|s| s.chars().next()) else {
        return Err(parse_error(input, i, message));
    };

    if ch == expected {
        Ok(i + ch.len_utf8())
    } else {
        Err(parse_error(input, i, message))
    }
}

fn find_matching_bracket(input: &str, open_idx: usize) -> Result<usize, String> {
    let mut i = open_idx + 1;
    let mut depth: usize = 1;

    let mut in_single = false;
    let mut in_double = false;
    let mut in_comment = false;
    let mut escaped = false;

    while i < input.len() {
        let ch = input[i..].chars().next().unwrap();
        let len = ch.len_utf8();

        if in_comment {
            if ch == '\n' {
                in_comment = false;
            }
            i += len;
            continue;
        }

        if in_single {
            if escaped {
                escaped = false;
                i += len;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '\'' => in_single = false,
                _ => {}
            }
            i += len;
            continue;
        }

        if in_double {
            if escaped {
                escaped = false;
                i += len;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_double = false,
                _ => {}
            }
            i += len;
            continue;
        }

        match ch {
            '#' => in_comment = true,
            '\'' => {
                in_single = true;
                escaped = false;
            }
            '"' => {
                in_double = true;
                escaped = false;
            }
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }

        i += len;
    }

    Err(parse_error(
        input,
        open_idx,
        "unclosed '[' in codechunk body",
    ))
}

fn parse_error(input: &str, idx: usize, message: &str) -> String {
    format!("{message} at {}", position_label(input, idx))
}

fn position_label(input: &str, idx: usize) -> String {
    let mut line = 1usize;
    let mut col = 1usize;

    for (byte_pos, ch) in input.char_indices() {
        if byte_pos >= idx {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }

    format!("line {line}, col {col}")
}

fn absolutize(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    let cwd = env::current_dir().map_err(|e| format!("failed to read current directory: {e}"))?;
    Ok(cwd.join(path))
}
