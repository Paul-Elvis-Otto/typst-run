use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use clap::Parser;
use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};

#[derive(Debug, Clone)]
struct Chunk {
    name: String,
    code: String,
    key: Option<String>,
    echo: bool,
    caption: Option<String>,
}

#[derive(Debug, Clone)]
enum ChunkOutput {
    Plot { rel_path: String },
    Text { content: String },
    Error { message: String },
}

#[derive(Debug, Clone)]
struct ChunkResult {
    output: ChunkOutput,
    echo: bool,
    caption: Option<String>,
    code: String,
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

    let mut results: HashMap<String, ChunkResult> = HashMap::new();
    for chunk in &chunks {
        let out = run_chunk(chunk, &build_dir);
        let output_key = chunk.key.clone().unwrap_or_else(|| chunk.name.clone());
        results.insert(
            output_key,
            ChunkResult {
                output: out,
                echo: chunk.echo,
                caption: chunk.caption.clone(),
                code: chunk.code.clone(),
            },
        );
    }

    let enriched = render_outputs(&doc_without_chunks, &results)?;
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
    let mut used_keys = HashSet::new();

    let mut cursor = 0;
    while let Some(start) = input[cursor..].find("```r") {
        let fence_start = cursor + start;
        stripped.push_str(&input[cursor..fence_start]);

        // Find the end of the fenced code block
        let mut i = fence_start + 4; // Skip ```r

        // Parse frontmatter
        let mut key = None;
        let mut echo = false;
        let mut caption = None;

        // Parse frontmatter lines that start with #|
        loop {
            i = skip_ws(input, i);
            if i >= input.len() || !input[i..].starts_with("#|") {
                break;
            }

            i += 2; // Skip #|
            i = skip_ws(input, i);

            // Parse key: value (quoted or unquoted)
            if input[i..].starts_with("key:") {
                i += 4; // past "key:"
                i = skip_ws(input, i);
                // Check for quoted string
                if input.chars().nth(i) == Some('"') {
                    let (k, next) = parse_quoted_string(input, i)?;
                    key = Some(k);
                    i = next;
                } else {
                    // Unquoted - get until end of line
                    let mut end = i;
                    while end < input.len() && !input[end..].starts_with('\n') {
                        end += 1;
                    }
                    key = Some(input[i..end].trim().to_string());
                    i = end;
                }
            }
            // Parse echo: true/false
            else if input[i..].starts_with("echo:") {
                i += 5; // past "echo:"
                i = skip_ws(input, i);
                let remaining = &input[i..];
                if remaining.starts_with("true")
                    || remaining.starts_with("true ")
                    || remaining.starts_with("true/")
                {
                    echo = true;
                    i += 4;
                } else if remaining.starts_with("false")
                    || remaining.starts_with("false ")
                    || remaining.starts_with("false/")
                {
                    echo = false;
                    i += 5;
                } else if remaining.starts_with("true /") {
                    echo = true;
                    i += 4;
                } else if remaining.starts_with("false /") {
                    echo = false;
                    i += 5;
                } else {
                    echo = true;
                    i += 4;
                }
            }
            // Parse caption: [value] or "value"
            else if input[i..].starts_with("caption:") {
                i += 8; // past "caption:"
                i = skip_ws(input, i);
                let ch = input.chars().nth(i);
                if ch == Some('[') {
                    i += 1;
                    if let Some(close) = input[i..].find(']') {
                        caption = Some(input[i..i + close].to_string());
                        i += close + 1;
                    }
                } else if ch == Some('"') {
                    let (c, next) = parse_quoted_string(input, i)?;
                    caption = Some(c);
                    i = next;
                } else {
                    // Unquoted - get until newline
                    let mut end = i;
                    while end < input.len() && input[end..].chars().next() != Some('\n') {
                        end += 1;
                    }
                    caption = Some(input[i..end].trim().to_string());
                    i = end;
                }
            }
            // Skip unknown lines
            else {
                if let Some(nl) = input[i..].find('\n') {
                    i += nl + 1;
                } else {
                    break;
                }
            }

            // Skip to next line
            i = skip_ws(input, i);
            if i < input.len() && input[i..].starts_with('\n') {
                i += 1;
            }

            i += 2; // Skip #|
            i = skip_ws(input, i);

            // Parse key: value (quoted or unquoted)
            if input[i..].starts_with("key:") {
                i += 4; // past "key:"
                i = skip_ws(input, i);
                // Check for quoted string
                if input.chars().nth(i) == Some('"') {
                    let (k, next) = parse_quoted_string(input, i)?;
                    key = Some(k);
                    i = next;
                } else {
                    // Unquoted - get until end of line
                    let mut end = i;
                    while end < input.len() && !input[end..].starts_with('\n') {
                        end += 1;
                    }
                    key = Some(input[i..end].trim().to_string());
                    i = end;
                }
            }
            // Parse echo: true/false
            else if input[i..].starts_with("echo:") {
                i += 5; // past "echo:"
                i = skip_ws(input, i);
                let remaining = &input[i..];
                if remaining.starts_with("true")
                    || remaining.starts_with("true ")
                    || remaining.starts_with("true/")
                {
                    echo = true;
                    i += 4;
                } else if remaining.starts_with("false")
                    || remaining.starts_with("false ")
                    || remaining.starts_with("false/")
                {
                    echo = false;
                    i += 5;
                } else if remaining.starts_with("true /") {
                    echo = true;
                    i += 4;
                } else if remaining.starts_with("false /") {
                    echo = false;
                    i += 5;
                } else {
                    // Default - just set true for compatibility
                    echo = true;
                    i += 4;
                }
            }
            // Parse caption: [value] or "value"
            else if input[i..].starts_with("caption:") {
                i += 8; // past "caption:"
                i = skip_ws(input, i);
                let ch = input.chars().nth(i);
                if ch == Some('[') {
                    i += 1;
                    if let Some(close) = input[i..].find(']') {
                        caption = Some(input[i..i + close].to_string());
                        i += close + 1;
                    }
                } else if ch == Some('"') {
                    let (c, next) = parse_quoted_string(input, i)?;
                    caption = Some(c);
                    i = next;
                } else {
                    // Unquoted - get until newline
                    let mut end = i;
                    while end < input.len() && input[end..].chars().next() != Some('\n') {
                        end += 1;
                    }
                    caption = Some(input[i..end].trim().to_string());
                    i = end;
                }
            }
            // Skip unknown lines
            else {
                if let Some(nl) = input[i..].find('\n') {
                    i += nl + 1;
                } else {
                    break;
                }
            }

            // Skip to next line
            i = skip_ws(input, i);
            if i < input.len() && input[i..].starts_with('\n') {
                i += 1;
            }
        }

        // Find the closing fence
        let mut fence_depth = 1;
        let code_start = i;
        while i < input.len() && fence_depth > 0 {
            if &input[i..i + 3] == "```" {
                fence_depth -= 1;
                if fence_depth == 0 {
                    break;
                }
                i += 3;
            } else if &input[i..i + 4] == "```r" {
                fence_depth += 1;
                i += 4;
            } else {
                i += 1;
            }
        }

        if fence_depth > 0 {
            return Err("unclosed fenced code block".to_string());
        }

        let code_end = i;
        let code = input[code_start..code_end].to_string();

        // Skip the closing fence
        i += 3;

        // Use key as name for backward compatibility, or generate one
        let chunk_name = key
            .clone()
            .unwrap_or_else(|| format!("chunk_{}", chunks.len()));

        // Validate key uniqueness if key is provided
        if let Some(ref k) = key {
            if !used_keys.insert(k.clone()) {
                return Err(format!("duplicate chunk key {:?}", k));
            }
        }

        chunks.push(Chunk {
            name: chunk_name,
            code,
            key,
            echo,
            caption,
        });

        cursor = i;
    }

    stripped.push_str(&input[cursor..]);
    Ok((stripped, chunks))
}

fn parse_bool(input: &str, mut i: usize) -> Result<(bool, usize), String> {
    i = skip_ws(input, i);
    if input[i..].starts_with("true") {
        Ok((true, i + 4))
    } else if input[i..].starts_with("false") {
        Ok((false, i + 5))
    } else if input[i..].starts_with("true ") {
        Ok((true, i + 5))
    } else if input[i..].starts_with("false ") {
        Ok((false, i + 6))
    } else if input[i..].starts_with("true/") {
        Ok((true, i + 5))
    } else if input[i..].starts_with("false/") {
        Ok((false, i + 6))
    } else if input[i..].starts_with("true /") {
        Ok((true, i + 6))
    } else if input[i..].starts_with("false /") {
        Ok((false, i + 7))
    } else if input[i..].starts_with("true / false") {
        Ok((true, i + 11))
    } else if input[i..].starts_with("false / true") {
        Ok((false, i + 11))
    } else {
        Err(format!(
            "expected boolean value at {}",
            position_label(input, i)
        ))
    }
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

    Ok((
        Chunk {
            name,
            code,
            key: None,
            echo: false,
            caption: None,
        },
        body_end + 1,
    ))
}

fn render_outputs(doc: &str, results: &HashMap<String, ChunkResult>) -> Result<String, String> {
    let mut out = String::with_capacity(doc.len());
    let mut cursor = 0;

    while let Some(rel) = doc[cursor..].find("#codekey(") {
        let start = cursor + rel;
        out.push_str(&doc[cursor..start]);

        let (name, end) = parse_codekey_at(doc, start)?;
        let replacement = match results.get(&name) {
            Some(result) => render_chunk_result(&name, result),
            None => render_missing_output(&name),
        };

        out.push_str(&replacement);
        cursor = end;
    }

    out.push_str(&doc[cursor..]);
    Ok(out)
}

fn parse_codekey_at(input: &str, start: usize) -> Result<(String, usize), String> {
    let mut i = start + "#codekey(".len();

    i = skip_ws(input, i);
    let name: String;
    // Check for quoted or unquoted
    if input.chars().nth(i) == Some('"') {
        let (n, next) = parse_quoted_string(input, i)?;
        name = n;
        i = next;
    } else {
        // Unquoted - read until )
        let mut end = i;
        while end < input.len() && input[end..].chars().next() != Some(')') {
            end += 1;
        }
        name = input[i..end].trim().to_string();
        i = end;
    }

    i = skip_ws(input, i);
    if i < input.len() && input[i..].starts_with(')') {
        i += 1;
    }
    Ok((name, i))
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

fn render_chunk_result(name: &str, result: &ChunkResult) -> String {
    match &result.output {
        ChunkOutput::Plot { rel_path } => {
            let caption = result.caption.clone().unwrap_or_else(|| name.to_string());
            format!(
                "#figure(\n  image(\"{}\"),\n  caption: [{}],\n)",
                escape_typst_string(rel_path),
                escape_typst_text(&caption)
            )
        }
        ChunkOutput::Text { content } => {
            let mut parts = Vec::new();
            if result.echo && !result.code.trim().is_empty() {
                parts.push(format!(
                    "#block(fill: gray.lighten(90%))[\n```r\n{}\n```\n]",
                    result.code
                ));
            }
            if !content.trim().is_empty() {
                parts.push(format!("#block[\n{}\n]", escape_typst_text(content)));
            }
            if parts.is_empty() {
                "#block[]".to_string()
            } else {
                parts.join("\n")
            }
        }
        ChunkOutput::Error { message } => {
            let mut parts = Vec::new();
            if result.echo && !result.code.trim().is_empty() {
                parts.push(format!(
                    "#block(fill: gray.lighten(90%))[\n```r\n{}\n```\n]",
                    result.code
                ));
            }
            parts.push(format!(
                "#block(fill: red.lighten(80%))[\nR error in chunk \"{}\":\n{}\n]",
                escape_typst_text(name),
                escape_typst_text(message)
            ));
            parts.join("\n")
        }
    }
}

fn render_missing_output(name: &str) -> String {
    format!(
        "#block(fill: red.lighten(80%))[\nMissing chunk \"{}\" for #codekey.\n]",
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
