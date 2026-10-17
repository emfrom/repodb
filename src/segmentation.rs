pub struct Unit<'a> {
    pub text: &'a str,
    pub line_start: usize,
    pub line_end: usize,
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
        Some(b"rs") => "rust",
        _ => crate::semgrep_languages::language_for_extension(extension),
    }
}

// Bounds apply to UTF-8 bytes, including preserved internal line endings.
const MAX_UNIT_BYTES: usize = 4096;

fn bounded(text: &str, mut line_start: usize, visit: &mut impl FnMut(Unit<'_>) -> bool) -> bool {
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
        }) {
            return false;
        }
        line_start += newlines;
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
            if start < offset && !bounded(&text[start..offset], start_line, visit) {
                return false;
            }
            start = offset + line.len();
            start_line = base_line + i + 1;
        }
        offset += line.len();
    }
    if start < text.len() {
        return bounded(&text[start..], start_line, visit);
    }
    true
}

fn rust(text: &str, visit: &mut impl FnMut(Unit<'_>) -> bool) -> Option<bool> {
    use syn::spanned::Spanned;

    let file = syn::parse_file(text).ok()?;
    // syn removes these prefixes before parsing; spans address the remainder.
    let prefix = usize::from(text.starts_with('\u{feff}')) * '\u{feff}'.len_utf8()
        + file.shebang.as_ref().map_or(0, String::len);
    let mut cursor = 0;
    let mut line = 1;
    for item in file.items {
        let range = item.span().byte_range();
        let start = prefix + range.start;
        let end = prefix + range.end;
        // Retain all text outside syntax spans, including comments and file attrs.
        if !paragraphs(&text[cursor..start], line, visit) {
            return Some(false);
        }
        line += text[cursor..start].bytes().filter(|b| *b == b'\n').count();
        if !bounded(&text[start..end], line, visit) {
            return Some(false);
        }
        line += text[start..end].bytes().filter(|b| *b == b'\n').count();
        cursor = end;
    }
    Some(paragraphs(&text[cursor..], line, visit))
}

/// Visits borrowed units in segmentation order; returning false stops before any later unit is staged.
pub fn visit(kind: &str, text: &str, mut visitor: impl FnMut(Unit<'_>) -> bool) {
    if !matches!(kind, "paragraph" | "fallback")
        && let Some(command) = std::env::var_os("REPODB_SEGMENTER")
        && let Some(ranges) = external_ranges(&command, kind, text.as_bytes())
    {
        visit_ranges(text, &ranges, &mut visitor);
        return;
    }
    if !matches!(kind, "paragraph" | "fallback" | "rust")
        && let Some(command) = std::env::var_os("REPODB_SEMGREP_CORE")
        && let Some(ranges) = semgrep_ranges(&command, kind, text.as_bytes())
    {
        visit_ranges(text, &ranges, &mut visitor);
        return;
    }
    if kind == "rust" && rust(text, &mut visitor).is_some() {
        return;
    }
    paragraphs(text, 1, &mut visitor);
}

fn visit_ranges(text: &str, ranges: &[(usize, usize)], visitor: &mut impl FnMut(Unit<'_>) -> bool) {
    let mut line = 1;
    for &(start, end) in ranges {
        let piece = &text[start..end];
        let newlines = piece.bytes().filter(|byte| *byte == b'\n').count();
        if !visitor(Unit {
            text: piece,
            line_start: line,
            line_end: line + newlines - usize::from(piece.ends_with('\n')),
        }) {
            return;
        }
        line += newlines;
    }
}

fn semgrep_ranges(
    executable: &std::ffi::OsStr,
    language: &str,
    source: &[u8],
) -> Option<Vec<(usize, usize)>> {
    if language != "move" {
        return semgrep_ranges_for_language(executable, language, source);
    }
    let sui = semgrep_ranges_for_language(executable, "move_on_sui", source);
    let aptos = semgrep_ranges_for_language(executable, "move_on_aptos", source);
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
) -> Option<Vec<(usize, usize)>> {
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
        return None;
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
    })?;

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
    let mut child = command.spawn().ok()?;
    let output = crate::process::capture_with_input_timeout(
        &mut child,
        &[],
        8 * 1024 * 1024,
        64 * 1024,
        Duration::from_secs(10),
    )
    .ok()?;
    if !output.status.success() || output.exceeded {
        return None;
    }
    let ast: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let syntax = crate::semgrep_languages::syntax_ranges_from_ast(language, &ast, source)?;
    let mut ranges = Vec::with_capacity(syntax.len() * 2 + 1);
    let mut cursor = 0;
    for [start, end] in syntax {
        if start < cursor || start >= end || end > source.len() {
            return None;
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
    validate_partition(source, ranges)
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
        });
        assert_eq!(visited, [(5, 1, 1)]);
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
    fn ambiguous_and_unavailable_extensions_keep_fallback() {
        assert_eq!(super::kind(b"source.h"), "fallback");
        assert_eq!(super::kind(b"source.move"), "move");
        assert_eq!(super::kind(b"source.vue"), "fallback");
        assert_eq!(super::kind(b"source.ps1"), "fallback");
        assert_eq!(super::kind(b"source.md"), "paragraph");
    }
}
