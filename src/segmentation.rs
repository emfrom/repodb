pub struct Unit<'a> {
    pub text: &'a str,
    pub line_start: usize,
    pub line_end: usize,
    pub byte_start: usize,
    pub byte_end: usize,
}

pub fn kind(path: &[u8]) -> &'static str {
    let name = path.rsplit(|b| *b == b'/').next().unwrap_or(path);
    let extension = name
        .iter()
        .rposition(|b| *b == b'.')
        .map(|i| &name[i + 1..]);
    if name == b"Dockerfile" {
        return "dockerfile";
    }
    if extension == Some(b"move".as_slice()) {
        return "move";
    }
    match extension {
        Some(b"md" | b"markdown" | b"txt") => "paragraph",
        _ => crate::semgrep_languages::language_for_extension(extension),
    }
}

// Bounds apply to UTF-8 bytes, including preserved internal line endings.
pub(crate) const MAX_UNIT_BYTES: usize = 4096;

fn bounded(
    text: &str,
    mut line_start: usize,
    mut byte_start: usize,
    visit: &mut impl FnMut(Unit<'_>) -> bool,
) -> bool {
    let text = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    let mut remaining = text;
    while !remaining.is_empty() {
        let mut end = remaining.len().min(MAX_UNIT_BYTES);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        if end < remaining.len() {
            if let Some(newline) = remaining[..end].rfind('\n') {
                end = newline + 1;
            } else if remaining.as_bytes()[end - 1] == b'\r' && remaining.as_bytes()[end] == b'\n' {
                end -= 1;
            }
        }
        let piece = &remaining[..end];
        let newlines = piece.bytes().filter(|b| *b == b'\n').count();
        if !visit(Unit {
            text: piece,
            line_start,
            line_end: line_start + newlines - usize::from(piece.ends_with('\n')),
            byte_start,
            byte_end: byte_start + piece.len(),
        }) {
            return false;
        }
        line_start += newlines;
        byte_start += end;
        remaining = &remaining[end..];
    }
    true
}

fn paragraphs(text: &str, base_line: usize, visit: &mut impl FnMut(Unit<'_>) -> bool) -> bool {
    let mut start = 0;
    let mut offset = 0;
    let mut start_line = base_line;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if line.trim().is_empty() {
            if start < offset && !bounded(&text[start..offset], start_line, start, visit) {
                return false;
            }
            start = offset + line.len();
            start_line = base_line + i + 1;
        }
        offset += line.len();
    }
    if start < text.len() {
        return bounded(&text[start..], start_line, start, visit);
    }
    true
}

/// Visits borrowed units in segmentation order; returning false stops before any later unit is staged.
pub fn visit(kind: &str, text: &str, visitor: impl FnMut(Unit<'_>) -> bool) -> Result<(), String> {
    visit_with_workers(kind, text, 1, visitor)
}

pub fn visit_with_workers(
    kind: &str,
    text: &str,
    worker_count: usize,
    mut visitor: impl FnMut(Unit<'_>) -> bool,
) -> Result<(), String> {
    if worker_count == 0 {
        return Err("Semgrep worker count must be positive".into());
    }
    if matches!(kind, "paragraph" | "fallback") {
        paragraphs(text, 1, &mut visitor);
        return Ok(());
    }

    let command = match crate::config::semgrep_core()? {
        Some(command) => command,
        None => {
            let config_path = crate::config::config_path()?;
            return Err(format!(
                "Semgrep core v1.181.0 is required for source parsing; set REPODB_SEMGREP_CORE or [tools].semgrep_core in {}",
                config_path.display()
            ));
        }
    };
    if !semgrep_core_is_pinned(&command) {
        return Err("configured Semgrep core must be version 1.181.0".into());
    }

    if let Some(segmenter) = crate::config::segmenter()?
        && let Some(ranges) = external_ranges(&segmenter, kind, text.as_bytes())
    {
        visit_ranges(text, &ranges, &mut visitor);
        return Ok(());
    }
    if let Some(ranges) = semgrep_ranges(&command, kind, text.as_bytes())? {
        visit_ranges(text, &ranges, &mut visitor);
    } else {
        paragraphs(text, 1, &mut visitor);
    }
    Ok(())
}

fn visit_ranges(text: &str, ranges: &[(usize, usize)], visitor: &mut impl FnMut(Unit<'_>) -> bool) {
    let mut line = 1;
    for &(start, end) in ranges {
        let mut remaining = &text[start..end];
        let mut byte_start = start;
        while !remaining.is_empty() {
            let mut length = remaining.len().min(MAX_UNIT_BYTES);
            while !remaining.is_char_boundary(length) {
                length -= 1;
            }
            if length < remaining.len() {
                if let Some(newline) = remaining[..length].rfind('\n') {
                    length = newline + 1;
                } else if remaining.as_bytes()[length - 1] == b'\r'
                    && remaining.as_bytes()[length] == b'\n'
                {
                    length -= 1;
                }
            }
            let piece = &remaining[..length];
            let newlines = piece.bytes().filter(|byte| *byte == b'\n').count();
            if !visitor(Unit {
                text: piece,
                line_start: line,
                line_end: line + newlines - usize::from(piece.ends_with('\n')),
                byte_start,
                byte_end: byte_start + piece.len(),
            }) {
                return;
            }
            line += newlines;
            byte_start += length;
            remaining = &remaining[length..];
        }
    }
}

fn semgrep_ranges(
    executable: &std::ffi::OsStr,
    language: &str,
    source: &[u8],
) -> Result<Option<Vec<(usize, usize)>>, String> {
    if language != "move" {
        return semgrep_ranges_for_language(executable, language, source);
    }
    let sui = semgrep_ranges_for_language(executable, "move_on_sui", source)?;
    let aptos = semgrep_ranges_for_language(executable, "move_on_aptos", source)?;
    Ok(reconcile_move_ranges(sui, aptos))
}

fn reconcile_move_ranges(
    sui: Option<Vec<(usize, usize)>>,
    aptos: Option<Vec<(usize, usize)>>,
) -> Option<Vec<(usize, usize)>> {
    match (sui, aptos) {
        (Some(ranges), None) | (None, Some(ranges)) => Some(ranges),
        (Some(sui), Some(aptos)) if sui == aptos => Some(sui),
        _ => None,
    }
}

fn semgrep_ranges_for_language(
    executable: &std::ffi::OsStr,
    language: &str,
    source: &[u8],
) -> Result<Option<Vec<(usize, usize)>>, String> {
    use std::{
        fs::OpenOptions,
        process::{Command, Stdio},
        time::Duration,
    };
    static TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    struct TemporarySource(std::path::PathBuf);
    impl Drop for TemporarySource {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    if !semgrep_core_is_pinned(executable) {
        return Ok(None);
    }

    let path = (0..16).find_map(|_| {
        let id = TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("repodb-semgrep-{}-{id}.src", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut file) => {
                use std::io::Write;
                let temp = TemporarySource(path);
                file.write_all(source).ok()?;
                Some(temp)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
            Err(_) => None,
        }
    });
    let Some(path) = path else {
        return Ok(None);
    };

    let mut output = None;
    for attempt in 0..2 {
        let mut command = Command::new(executable);
        command
            .args(["-l", language, "-json", "-full_token_info", "-dump_ast"])
            .arg(&path.0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) if attempt == 0 => continue,
            Err(_) => return Ok(None),
        };
        let attempt_output = match crate::process::capture_with_input_timeout(
            &mut child,
            &[],
            8 * 1024 * 1024,
            64 * 1024,
            Duration::from_secs(10),
        ) {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                return Err(error.to_string());
            }
            Err(_) if attempt == 0 => continue,
            Err(_) => return Ok(None),
        };
        if !attempt_output.status.success() {
            if attempt == 0 {
                continue;
            }
            return Ok(None);
        }
        output = Some(attempt_output);
        break;
    }
    let Some(output) = output else {
        return Ok(None);
    };
    if output.exceeded {
        return Ok(None);
    }
    let Ok(ast) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Ok(None);
    };
    let Some(syntax) = crate::semgrep_languages::syntax_ranges_from_ast(language, &ast, source)
    else {
        return Ok(None);
    };
    let mut ranges = Vec::with_capacity(syntax.len() * 2 + 1);
    let mut cursor = 0;
    for [start, end] in syntax {
        if start < cursor || start >= end || end > source.len() {
            return Ok(None);
        }
        if cursor < start {
            ranges.push([cursor, start]);
        }
        ranges.push([start, end]);
        cursor = end;
    }
    if cursor < source.len() {
        ranges.push([cursor, source.len()]);
    }
    Ok(validate_partition(source, ranges))
}

fn semgrep_core_is_pinned(executable: &std::ffi::OsStr) -> bool {
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;
    use std::time::Duration;
    static VERSION_OK: OnceLock<bool> = OnceLock::new();
    *VERSION_OK.get_or_init(|| {
        let mut command = Command::new(executable);
        command
            .arg("-version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let Ok(mut child) = command.spawn() else {
            return false;
        };
        let Ok(output) = crate::process::capture_with_input_timeout(
            &mut child,
            &[],
            8 * 1024,
            8 * 1024,
            Duration::from_secs(3),
        ) else {
            return false;
        };
        if !output.status.success() || output.exceeded {
            return false;
        }
        let version = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        version
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '.')
            .any(|part| part == "1.181.0")
    })
}

fn external_ranges(
    command: &std::ffi::OsStr,
    language: &str,
    source: &[u8],
) -> Option<Vec<(usize, usize)>> {
    use std::{
        process::{Command, Stdio},
        time::Duration,
    };
    let mut command = Command::new(command);
    command
        .arg(language)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().ok()?;
    let output = crate::process::capture_with_input_timeout(
        &mut child,
        source,
        8 * 1024 * 1024,
        64 * 1024,
        Duration::from_secs(10),
    )
    .ok()?;
    if !output.status.success() || output.exceeded {
        return None;
    }
    let ranges: Vec<[usize; 2]> = serde_json::from_slice(&output.stdout).ok()?;
    validate_partition(source, ranges)
}

fn validate_partition(source: &[u8], ranges: Vec<[usize; 2]>) -> Option<Vec<(usize, usize)>> {
    let text = std::str::from_utf8(source).ok()?;
    let mut cursor = 0;
    let mut validated = Vec::with_capacity(ranges.len());
    for [start, end] in ranges {
        if start != cursor
            || start >= end
            || end > source.len()
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
        {
            return None;
        }
        cursor = end;
        validated.push((start, end));
    }
    if cursor != source.len() {
        return None;
    }
    Some(validated)
}

#[cfg(test)]
mod tests {
    use super::visit;

    #[test]
    fn visitor_can_stop_before_later_units_are_staged() {
        let text = "first\n\nsecond\n\nthird";
        let mut visited = Vec::new();
        visit("paragraph", text, |unit| {
            visited.push((unit.text.len(), unit.line_start, unit.line_end));
            false
        })
        .unwrap();
        assert_eq!(visited, [(5, 1, 1)]);
    }

    #[test]
    fn parser_ranges_split_losslessly_at_utf8_boundaries() {
        let source = format!("{}é\n{}", "a".repeat(4095), "b".repeat(5000));
        let mut units = Vec::new();
        super::visit_ranges(&source, &[(0, source.len())], &mut |unit| {
            units.push((unit.text.to_owned(), unit.line_start, unit.line_end));
            true
        });
        assert_eq!(
            units
                .iter()
                .map(|(text, _, _)| text.as_str())
                .collect::<String>(),
            source
        );
        assert!(units.iter().all(|(text, _, _)| text.len() <= 4096));
        assert_eq!(units.first().unwrap().1..=units.last().unwrap().2, 1..=2);
    }

    #[test]
    fn selected_semgrep_languages_are_routed_by_source_extension() {
        assert_eq!(super::kind(b"src/lib.rs"), "rust");
        assert_eq!(super::kind(b"Dockerfile"), "dockerfile");
        assert_eq!(super::kind(b"src/module.move"), "move");
        for language in crate::semgrep_languages::LANGUAGES {
            for extension in language.extensions {
                assert_eq!(
                    super::kind(format!("source.{extension}").as_bytes()),
                    language.semgrep_name,
                    "extension {extension} must route to {}",
                    language.semgrep_name,
                );
            }
        }
    }

    #[test]
    fn move_parser_results_require_consensus_or_one_success() {
        let ranges = vec![(0, 12), (12, 20)];
        assert_eq!(
            super::reconcile_move_ranges(Some(ranges.clone()), Some(ranges.clone())),
            Some(ranges.clone())
        );
        assert_eq!(
            super::reconcile_move_ranges(Some(ranges.clone()), None),
            Some(ranges.clone())
        );
        assert_eq!(
            super::reconcile_move_ranges(None, Some(ranges.clone())),
            Some(ranges)
        );
        assert_eq!(
            super::reconcile_move_ranges(Some(vec![(0, 20)]), Some(vec![(0, 12), (12, 20)])),
            None
        );
        assert_eq!(super::reconcile_move_ranges(None, None), None);
    }

    #[test]
    fn ambiguous_and_unavailable_extensions_keep_fallback() {
        assert_eq!(super::kind(b"source.h"), "fallback");
        assert_eq!(super::kind(b"source.move"), "move");
        assert_eq!(super::kind(b"source.vue"), "fallback");
        assert_eq!(super::kind(b"source.ps1"), "fallback");
        assert_eq!(super::kind(b"source.md"), "paragraph");
    }
}
