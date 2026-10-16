pub struct Unit {
    pub text: String,
    pub line_start: usize,
    pub line_end: usize,
}

pub fn kind(path: &[u8]) -> &'static str {
    let name = path.rsplit(|b| *b == b'/').next().unwrap_or(path);
    let extension = name
        .iter()
        .rposition(|b| *b == b'.')
        .map(|i| &name[i + 1..]);
    match extension {
        Some(b"md" | b"markdown" | b"txt") => "paragraph",
        Some(b"rs") => "rust",
        _ => "fallback",
    }
}

// Bounds apply to UTF-8 bytes, including preserved internal line endings.
const MAX_UNIT_BYTES: usize = 4096;

fn bounded(text: &str, mut line_start: usize, out: &mut Vec<Unit>) {
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
        out.push(Unit {
            text: piece.to_owned(),
            line_start,
            line_end: line_start + newlines - usize::from(piece.ends_with('\n')),
        });
        line_start += newlines;
        remaining = &remaining[end..];
    }
}

fn paragraphs(text: &str, base_line: usize, out: &mut Vec<Unit>) {
    let mut start = 0;
    let mut offset = 0;
    let mut start_line = base_line;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if line.trim().is_empty() {
            if start < offset {
                bounded(&text[start..offset], start_line, out);
            }
            start = offset + line.len();
            start_line = base_line + i + 1;
        }
        offset += line.len();
    }
    if start < text.len() {
        bounded(&text[start..], start_line, out);
    }
}

fn rust(text: &str) -> Option<Vec<Unit>> {
    use syn::spanned::Spanned;

    let file = syn::parse_file(text).ok()?;
    // syn removes these prefixes before parsing; spans address the remainder.
    let prefix = usize::from(text.starts_with('\u{feff}')) * '\u{feff}'.len_utf8()
        + file.shebang.as_ref().map_or(0, String::len);
    let mut out = vec![];
    let mut cursor = 0;
    let mut line = 1;
    for item in file.items {
        let range = item.span().byte_range();
        let start = prefix + range.start;
        let end = prefix + range.end;
        // Retain all text outside syntax spans, including comments and file attrs.
        paragraphs(&text[cursor..start], line, &mut out);
        line += text[cursor..start].bytes().filter(|b| *b == b'\n').count();
        bounded(&text[start..end], line, &mut out);
        line += text[start..end].bytes().filter(|b| *b == b'\n').count();
        cursor = end;
    }
    paragraphs(&text[cursor..], line, &mut out);
    Some(out)
}

pub fn segment(kind: &str, text: &str) -> Vec<Unit> {
    if kind == "rust"
        && let Some(units) = rust(text)
    {
        return units;
    }
    let mut out = vec![];
    paragraphs(text, 1, &mut out);
    out
}
