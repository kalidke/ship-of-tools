// preview/highlight.rs — tree-sitter-backed syntax highlighting.
//
// One `HighlightService` lives on `State`; the markdown preview's fenced
// code-block walk and (later) the editor pane both call
// `service.highlight(lang, source)` to get a `Vec<HighlightSpan>`, one
// span per highlighted byte range with a stable scope name like
// "keyword" / "function.call" / "type.builtin".
//
// Per Codex's industry-standard recommendation: tree-sitter is the
// local synchronous base; LSP semantic-token overlay (versioned, async)
// is the planned next layer for accuracy on the `.jl` editor surface.
// This module ships only the tree-sitter layer; semantic overlay is
// queued as a separate stage once an editor surface lands.
//
// v1 supports Julia only (the audience's primary language). Rust /
// Python / Markdown grammars layer on by adding their crate to
// Cargo.toml and registering in `HighlightService::new`.

use std::collections::HashMap;

use anyhow::{Context, Result};
use cosmic_text::Color;
use tree_sitter_highlight::{
    HighlightConfiguration, HighlightEvent, Highlighter,
};

/// Standard tree-sitter capture-name list. Names not present in a
/// grammar's `highlights.scm` are simply unused — no error from the
/// recognizer. Add a name here when adding a grammar that uses it.
pub const RECOGNIZED_HIGHLIGHTS: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "escape",
    "function",
    "function.builtin",
    "function.call",
    "function.macro",
    "function.method",
    "keyword",
    "keyword.control",
    "keyword.function",
    "label",
    "module",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.special",
    "string",
    "string.escape",
    "string.special",
    "tag",
    "type",
    "type.builtin",
    "type.parameter",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

/// One highlighted byte range from `HighlightService::highlight`. Source
/// regions not in any returned span are unhighlighted (caller renders
/// in default fg). Returned in source order; spans don't overlap.
#[derive(Debug, Clone)]
pub struct HighlightSpan {
    pub start: usize,
    pub end: usize,
    /// One of `RECOGNIZED_HIGHLIGHTS` — owned `'static` so callers can
    /// store + ferry it without lifetimes.
    pub scope: &'static str,
}

/// Per-language highlight pipeline (one `HighlightConfiguration` per
/// registered language, cached). Construction is moderately expensive
/// (compiles the grammar's highlight query); keep the service on State
/// and reuse across redraws.
pub struct HighlightService {
    julia: HighlightConfiguration,
    rust: HighlightConfiguration,
    python: HighlightConfiguration,
    bash: HighlightConfiguration,
    json: HighlightConfiguration,
    toml: HighlightConfiguration,
}

impl HighlightService {
    pub fn new() -> Result<Self> {
        // Each grammar crate exposes `LANGUAGE: LanguageFn` (not a
        // `language()` fn). `.into()` resolves to a `Language` linked
        // to our `tree-sitter = 0.26` via the `tree-sitter-language`
        // shim — bridges version drift between core + grammar crates.
        //
        // Most grammar crates export their highlight query as a `&str`
        // const (`HIGHLIGHTS_QUERY` or `HIGHLIGHT_QUERY` — singular for
        // bash, plural for the rest). `tree-sitter-julia` 0.23 is the
        // outlier; its highlight query ships in the crate source tree
        // but isn't exported, so we vendor it under
        // `frontend/queries/julia-highlights.scm` and `include_str!`.
        //
        // `HighlightConfiguration::new(language, name, hl, inj, loc)`
        // — pass `""` for injection / locals queries on grammars that
        // don't ship them (most here; rust + bash ship injections but
        // we don't dispatch nested languages yet).
        //
        // `configure(&RECOGNIZED_HIGHLIGHTS)` is mandatory after
        // `new()`. Without it, every `HighlightStart` event resolves
        // to index 0 / first recognized name — visually looks like
        // "all keywords" and is wrong. The call also filters out
        // captures whose name isn't in our list.
        const JULIA_HIGHLIGHTS: &str =
            include_str!("../../queries/julia-highlights.scm");

        fn build(
            language: tree_sitter::Language,
            name: &str,
            hl: &str,
        ) -> Result<HighlightConfiguration> {
            let mut c = HighlightConfiguration::new(language, name, hl, "", "")
                .with_context(|| format!("init tree-sitter-{name} HighlightConfiguration"))?;
            c.configure(RECOGNIZED_HIGHLIGHTS);
            Ok(c)
        }

        Ok(Self {
            julia: build(
                tree_sitter_julia::LANGUAGE.into(),
                "julia",
                JULIA_HIGHLIGHTS,
            )?,
            rust: build(
                tree_sitter_rust::LANGUAGE.into(),
                "rust",
                tree_sitter_rust::HIGHLIGHTS_QUERY,
            )?,
            python: build(
                tree_sitter_python::LANGUAGE.into(),
                "python",
                tree_sitter_python::HIGHLIGHTS_QUERY,
            )?,
            bash: build(
                tree_sitter_bash::LANGUAGE.into(),
                "bash",
                tree_sitter_bash::HIGHLIGHT_QUERY,
            )?,
            json: build(
                tree_sitter_json::LANGUAGE.into(),
                "json",
                tree_sitter_json::HIGHLIGHTS_QUERY,
            )?,
            toml: build(
                tree_sitter_toml_ng::LANGUAGE.into(),
                "toml",
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
            )?,
        })
    }

    fn config_for(&self, lang_alias: &str) -> Option<&HighlightConfiguration> {
        let key = lang_alias.trim().to_ascii_lowercase();
        match key.as_str() {
            "julia" | "jl" => Some(&self.julia),
            "rust" | "rs" => Some(&self.rust),
            "python" | "py" => Some(&self.python),
            "bash" | "sh" | "shell" | "zsh" => Some(&self.bash),
            "json" => Some(&self.json),
            "toml" => Some(&self.toml),
            _ => None,
        }
    }

    /// Highlight `source` with the grammar registered under `lang_alias`.
    /// Returns an empty Vec for unknown languages, parse failures, or
    /// sources with no captures — callers fall back to rendering the
    /// source in default fg. Spans are non-overlapping and in source
    /// order; each carries a scope name from `RECOGNIZED_HIGHLIGHTS`.
    pub fn highlight(&self, lang_alias: &str, source: &str) -> Vec<HighlightSpan> {
        let Some(config) = self.config_for(lang_alias) else {
            return Vec::new();
        };
        let mut highlighter = Highlighter::new();
        // `None` for the injection-language callback: we don't support
        // injections yet (would need a second LanguageRegistry lookup
        // per nested-language region, e.g. SQL-in-Python).
        let events = match highlighter.highlight(
            config,
            source.as_bytes(),
            None,
            |_| None,
        ) {
            Ok(it) => it,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    lang = %lang_alias,
                    "tree-sitter highlight failed; falling back to plain"
                );
                return Vec::new();
            }
        };
        let mut stack: Vec<&'static str> = Vec::new();
        let mut spans = Vec::new();
        for evt in events {
            match evt {
                Ok(HighlightEvent::Source { start, end }) => {
                    if let Some(&scope) = stack.last() {
                        if !scope.is_empty() && start < end {
                            spans.push(HighlightSpan { start, end, scope });
                        }
                    }
                }
                Ok(HighlightEvent::HighlightStart(h)) => {
                    let name = RECOGNIZED_HIGHLIGHTS.get(h.0).copied().unwrap_or("");
                    stack.push(name);
                }
                Ok(HighlightEvent::HighlightEnd) => {
                    stack.pop();
                }
                Err(e) => {
                    tracing::warn!(error = %e, "tree-sitter highlight stream error");
                    break;
                }
            }
        }
        spans
    }
}

/// Map a tree-sitter capture-name scope to a `cosmic_text::Color` from
/// the VS Code Dark+ palette. Returns `None` for scopes we leave at the
/// pane's default fg (e.g. plain `variable`, anonymous text).
///
/// Add a scope here when a grammar starts emitting one we haven't
/// themed yet; nothing in this module enforces the mapping is total.
pub fn color_for_scope(scope: &str) -> Option<Color> {
    Some(match scope {
        "keyword" | "keyword.control" | "keyword.function" => Color::rgb(86, 156, 214),
        "type" | "type.builtin" | "type.parameter" | "constructor" => {
            Color::rgb(78, 201, 176)
        }
        "number" | "constant" | "constant.builtin" => Color::rgb(181, 206, 168),
        "string" | "string.escape" | "string.special" | "escape" => {
            Color::rgb(206, 145, 120)
        }
        "comment" => Color::rgb(106, 153, 85),
        "function" | "function.call" | "function.builtin" | "function.method" => {
            Color::rgb(220, 220, 170)
        }
        "function.macro" => Color::rgb(220, 180, 80),
        "variable.builtin" => Color::rgb(156, 220, 254),
        "attribute" | "tag" => Color::rgb(78, 201, 176),
        _ => return None,
    })
}

// `HashMap` is held for future per-language config storage; currently
// we keep `julia` inline since the registry has exactly one entry.
// Drop this allow when a second language lands.
#[allow(dead_code)]
fn _unused_hashmap_seed() -> HashMap<String, HighlightConfiguration> {
    HashMap::new()
}
