//! Semgrep v1.181.0 OSS parser targets with a conservative, unambiguous extension route.
//!
//! Source inventory: semgrep/semgrep v1.181.0 `Language.ml`. The binary-backed
//! runtime set excludes proprietary Apex/Elixir/Gosu and parsers unavailable in
//! the checked-in v1.181.0 package (PowerShell, Vue). Generic/regex/none and
//! ambiguous extensions deliberately remain on fallback.

pub struct Language {
    pub semgrep_name: &'static str,
    pub extensions: &'static [&'static str],
}

/// OSS dedicated parser inventory in Semgrep v1.181.0 after excluding
/// proprietary and unavailable parsers. Rust is already handled by RepoDB's
/// built-in parser; the remaining names are routed by an unambiguous suffix
/// where one exists.
pub const PARSER_LANGUAGES: &[&str] = &[
    "bash",
    "c",
    "cairo",
    "circom",
    "clojure",
    "cpp",
    "csharp",
    "dart",
    "dockerfile",
    "fga",
    "go",
    "hack",
    "html",
    "java",
    "javascript",
    "json",
    "jsonnet",
    "julia",
    "kotlin",
    "lisp",
    "lua",
    "move_on_sui",
    "move_on_aptos",
    "ocaml",
    "php",
    "promql",
    "protobuf",
    "python2",
    "python3",
    "python",
    "r",
    "ruby",
    "rust",
    "scala",
    "scheme",
    "solidity",
    "swift",
    "terraform",
    "typescript",
    "xml",
    "yaml",
];

pub const LANGUAGES: &[Language] = &[
    Language {
        semgrep_name: "bash",
        extensions: &["sh", "bash"],
    },
    Language {
        semgrep_name: "c",
        extensions: &["c"],
    },
    Language {
        semgrep_name: "cairo",
        extensions: &["cairo"],
    },
    Language {
        semgrep_name: "circom",
        extensions: &["circom"],
    },
    Language {
        semgrep_name: "clojure",
        extensions: &["clj", "cljs", "cljc"],
    },
    Language {
        semgrep_name: "cpp",
        extensions: &["cc", "cpp", "cxx"],
    },
    Language {
        semgrep_name: "csharp",
        extensions: &["cs"],
    },
    Language {
        semgrep_name: "dart",
        extensions: &["dart"],
    },
    Language {
        semgrep_name: "dockerfile",
        extensions: &["dockerfile"],
    },
    Language {
        semgrep_name: "fga",
        extensions: &["fga"],
    },
    Language {
        semgrep_name: "go",
        extensions: &["go"],
    },
    Language {
        semgrep_name: "hack",
        extensions: &["hack"],
    },
    Language {
        semgrep_name: "html",
        extensions: &["html", "htm"],
    },
    Language {
        semgrep_name: "java",
        extensions: &["java"],
    },
    Language {
        semgrep_name: "javascript",
        extensions: &["js", "jsx", "mjs", "cjs"],
    },
    Language {
        semgrep_name: "json",
        extensions: &["json"],
    },
    Language {
        semgrep_name: "jsonnet",
        extensions: &["jsonnet", "libsonnet"],
    },
    Language {
        semgrep_name: "julia",
        extensions: &["jl"],
    },
    Language {
        semgrep_name: "kotlin",
        extensions: &["kt", "kts"],
    },
    Language {
        semgrep_name: "lisp",
        extensions: &["lisp", "lsp"],
    },
    Language {
        semgrep_name: "lua",
        extensions: &["lua"],
    },
    Language {
        semgrep_name: "ocaml",
        extensions: &["ml", "mli"],
    },
    Language {
        semgrep_name: "php",
        extensions: &["php"],
    },
    Language {
        semgrep_name: "promql",
        extensions: &["promql"],
    },
    Language {
        semgrep_name: "protobuf",
        extensions: &["proto"],
    },
    Language {
        semgrep_name: "python2",
        extensions: &["py2"],
    },
    Language {
        semgrep_name: "python3",
        extensions: &["py3"],
    },
    Language {
        semgrep_name: "python",
        extensions: &["py"],
    },
    Language {
        semgrep_name: "r",
        extensions: &["r"],
    },
    Language {
        semgrep_name: "ruby",
        extensions: &["rb"],
    },
    Language {
        semgrep_name: "scala",
        extensions: &["scala"],
    },
    Language {
        semgrep_name: "scheme",
        extensions: &["scm", "ss"],
    },
    Language {
        semgrep_name: "solidity",
        extensions: &["sol"],
    },
    Language {
        semgrep_name: "swift",
        extensions: &["swift"],
    },
    Language {
        semgrep_name: "terraform",
        extensions: &["tf"],
    },
    Language {
        semgrep_name: "typescript",
        extensions: &["ts", "tsx"],
    },
    Language {
        semgrep_name: "xml",
        extensions: &["xml"],
    },
    Language {
        semgrep_name: "yaml",
        extensions: &["yaml", "yml"],
    },
];

/// Converts a v1.181.0 generic AST dump into syntax spans. Each `Pr` child is
/// one syntax unit; intervening source bytes are added as separate gap units by
/// the caller. Token text is checked against the original source bytes.
pub fn syntax_ranges_from_ast(
    language: &str,
    ast: &serde_json::Value,
    source: &[u8],
) -> Option<Vec<[usize; 2]>> {
    let source_text = std::str::from_utf8(source).ok()?;
    let nodes = ast.get("Pr")?.as_array()?;
    if nodes.is_empty() || source.is_empty() {
        return None;
    }
    let mut syntax = Vec::with_capacity(nodes.len());
    for node in nodes {
        let mut tokens = Vec::new();
        collect_tokens(node, &mut tokens, language == "yaml")?;
        for (start, end, text) in &tokens {
            if *start > *end
                || *end > source.len()
                || !source_text.is_char_boundary(*start)
                || !source_text.is_char_boundary(*end)
                || &source[*start..*end] != text.as_bytes()
            {
                return None;
            }
        }
        let start = tokens.iter().map(|(start, _, _)| *start).min()?;
        let end = tokens.iter().map(|(_, end, _)| *end).max()?;
        syntax.push([start, end]);
    }
    syntax.sort_unstable_by_key(|range| range[0]);
    let mut cursor = 0;
    for [start, end] in &syntax {
        if *start < cursor
            || *start >= *end
            || *end > source.len()
            || !source_text.is_char_boundary(*start)
            || !source_text.is_char_boundary(*end)
        {
            return None;
        }
        cursor = *end;
    }
    Some(syntax)
}

fn collect_tokens(
    value: &serde_json::Value,
    tokens: &mut Vec<(usize, usize, String)>,
    skip_yaml_container_delimiters: bool,
) -> Option<()> {
    match value {
        serde_json::Value::Object(object) => {
            if skip_yaml_container_delimiters && let Some(container) = object.get("Container") {
                let container = container.as_array()?;
                if container.len() != 2 {
                    return None;
                }
                container[0].as_str()?;
                let slots = container[1].as_array()?;
                if slots.len() != 3 {
                    return None;
                }
                validate_container_delimiter(&slots[0], "(")?;
                validate_container_delimiter(&slots[2], ")")?;
                collect_tokens(&slots[1], tokens, true)?;
                for (key, child) in object {
                    if key != "Container" {
                        collect_tokens(child, tokens, true)?;
                    }
                }
                return Some(());
            }
            if let Some(token) = object.get("OriginTok") {
                let token = token.as_object()?;
                let text = token.get("str")?.as_str()?;
                let start = usize::try_from(token.get("bytepos")?.as_u64()?).ok()?;
                let end = start.checked_add(text.len())?;
                tokens.push((start, end, text.to_owned()));
            }
            for child in object.values() {
                collect_tokens(child, tokens, skip_yaml_container_delimiters)?;
            }
        }
        serde_json::Value::Array(array) => {
            for child in array {
                collect_tokens(child, tokens, skip_yaml_container_delimiters)?;
            }
        }
        _ => {}
    }
    Some(())
}

fn validate_container_delimiter(value: &serde_json::Value, expected: &str) -> Option<()> {
    let slot = value.as_object()?;
    if slot.len() != 1 {
        return None;
    }
    let token = slot.get("token")?.as_object()?;
    if token.len() != 1 {
        return None;
    }
    if let Some(origin) = token.get("OriginTok") {
        let origin = origin.as_object()?;
        if origin.get("str")?.as_str()? != expected {
            return None;
        }
        origin.get("bytepos")?.as_u64()?;
    } else {
        let fake = token.get("FakeTok")?.as_array()?;
        if fake.len() != 2 || fake[0].as_str()? != expected {
            return None;
        }
    }
    Some(())
}

pub fn language_for_extension(extension: Option<&[u8]>) -> &'static str {
    let Some(extension) = extension else {
        return "fallback";
    };
    LANGUAGES
        .iter()
        .find(|language| {
            language
                .extensions
                .iter()
                .any(|candidate| candidate.as_bytes() == extension)
        })
        .map_or("fallback", |language| {
            if PARSER_LANGUAGES.contains(&language.semgrep_name) {
                language.semgrep_name
            } else {
                "fallback"
            }
        })
}

#[cfg(test)]
mod tests {
    use super::{PARSER_LANGUAGES, syntax_ranges_from_ast};

    #[test]
    fn runtime_parser_inventory_has_41_dedicated_oss_languages() {
        assert_eq!(PARSER_LANGUAGES.len(), 41);
        assert!(!PARSER_LANGUAGES.contains(&"powershell"));
        assert!(!PARSER_LANGUAGES.contains(&"vue"));
        for language in PARSER_LANGUAGES {
            assert!(
                *language == "rust"
                    || language.starts_with("move_on_")
                    || super::LANGUAGES
                        .iter()
                        .any(|routed| routed.semgrep_name == *language),
                "{language} must be routed or explicitly use existing built-in/ambiguous fallback"
            );
        }
    }

    #[test]
    fn ast_tokens_create_syntax_units_and_preserve_every_gap_byte() {
        let source = "// leading\nfn a() { let x = \"é\"; }\n\n// between\nfn b() {}\n";
        let a_start = source.find("fn a").unwrap();
        let a_end = source.find("\n\n").unwrap();
        let b_start = source.find("fn b").unwrap();
        let b_end = source.len() - 1;
        let ast = serde_json::json!({"Pr": [
            {"tokens": [
                {"OriginTok": {"str": "fn", "bytepos": a_start}},
                {"OriginTok": {"str": "}", "bytepos": a_end - 1}}
            ]},
            {"tokens": [
                {"OriginTok": {"str": "fn", "bytepos": b_start}},
                {"OriginTok": {"str": "}", "bytepos": b_end - 1}}
            ]}
        ]});
        let syntax = syntax_ranges_from_ast("rust", &ast, source.as_bytes()).unwrap();
        assert_eq!(syntax, [[a_start, a_end], [b_start, b_end]]);
        let mut partition = Vec::new();
        let mut cursor = 0;
        for [start, end] in syntax {
            if cursor < start {
                partition.push([cursor, start]);
            }
            partition.push([start, end]);
            cursor = end;
        }
        if cursor < source.len() {
            partition.push([cursor, source.len()]);
        }
        assert_eq!(
            partition
                .iter()
                .map(|[start, end]| end - start)
                .sum::<usize>(),
            source.len()
        );
        assert_eq!(partition.first(), Some(&[0, a_start]));
        assert_eq!(partition.last(), Some(&[b_end, source.len()]));
        assert_eq!(source.as_bytes()[a_start..a_end].last(), Some(&b'}'));
        assert_eq!(source.as_bytes()[b_start..b_end].last(), Some(&b'}'));
    }

    fn origin(text: &str, bytepos: usize) -> serde_json::Value {
        serde_json::json!({"token": {"OriginTok": {"str": text, "bytepos": bytepos}}})
    }

    fn yaml_container(
        open: serde_json::Value,
        items: serde_json::Value,
        close: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({"Container": ["Dict", [open, items, close]]})
    }

    #[test]
    fn yaml_skips_only_structural_container_delimiter_slots() {
        let source = b"(a)";
        let ast = serde_json::json!({"Pr": [yaml_container(
            origin("(", 0),
            serde_json::json!([origin("a", 1)]),
            origin(")", 1)
        )]});
        assert_eq!(
            syntax_ranges_from_ast("yaml", &ast, source),
            Some(vec![[1, 2]])
        );

        let ast = serde_json::json!({"Pr": [yaml_container(
            origin("(", 1),
            serde_json::json!([origin("a", 1)]),
            origin(")", 2)
        )]});
        assert_eq!(
            syntax_ranges_from_ast("yaml", &ast, source),
            Some(vec![[1, 2]])
        );
    }

    #[test]
    fn yaml_rejects_mismatching_origin_token_outside_container_delimiters() {
        let ast = serde_json::json!({"Pr": [yaml_container(
            origin("(", 0),
            serde_json::json!([origin("not-a", 1)]),
            origin(")", 1)
        )]});
        assert!(syntax_ranges_from_ast("yaml", &ast, b"(a)").is_none());
    }

    #[test]
    fn ast_token_mismatch_or_invalid_position_is_rejected() {
        let ast =
            serde_json::json!({"Pr": [{"token": {"OriginTok": {"str": "bad", "bytepos": 0}}}]});
        assert!(syntax_ranges_from_ast("rust", &ast, b"good").is_none());
        let ast = serde_json::json!({"Pr": [{"token": {"OriginTok": {"str": "x", "bytepos": 9}}}]});
        assert!(syntax_ranges_from_ast("rust", &ast, b"x").is_none());
    }
}
