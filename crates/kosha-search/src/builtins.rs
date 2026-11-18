// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Platform-specific, metadata-only Natural aliases. Both frontends use this
//! catalogue for discovery as well as translation.

pub struct RecipeGroup {
    pub label: &'static str,
    /// Complete natural phrase and its executable DSL.
    pub recipes: &'static [(&'static str, &'static str)],
}

#[cfg(not(target_os = "linux"))]
pub const BUILTIN_RECIPE_GROUPS: &[RecipeGroup] = &[];

#[cfg(target_os = "linux")]
pub const BUILTIN_RECIPE_GROUPS: &[RecipeGroup] = &[
    RecipeGroup {
        label: "Linux logs and caches",
        recipes: &[
            ("linux system logs", r#"file and path ~ "/var/log/""#),
            (
                "old linux system logs",
                r#"file and path ~ "/var/log/" and age > 30d"#,
            ),
            (
                "systemd journal files",
                r#"file and path ~ "/var/log/journal/" and ext in (journal, "journal~")"#,
            ),
            (
                "linux crash dumps",
                r#"file and path ~ "/var/lib/systemd/coredump/""#,
            ),
            (
                "linux package download caches",
                r#"file and (path ~ "/var/cache/apt/archives/" or path ~ "/var/cache/pacman/pkg/" or path ~ "/var/cache/dnf/" or path ~ "/var/cache/libdnf5/")"#,
            ),
            (
                "large linux cache files",
                r#"file and path ~ "/.cache/" and size > 100mib"#,
            ),
        ],
    },
    RecipeGroup {
        label: "Build and development files",
        recipes: &[
            (
                "rust build files",
                r#"file and (path ~ "/target/debug/" or path ~ "/target/release/")"#,
            ),
            (
                "rust incremental build files",
                r#"file and (path ~ "/target/debug/incremental/" or path ~ "/target/release/incremental/")"#,
            ),
            (
                "cargo dependency caches",
                r#"file and (path ~ "/.cargo/registry/" or path ~ "/.cargo/git/")"#,
            ),
            (
                "cmake build files",
                r#"file and (name = CMakeCache.txt or path ~ "/CMakeFiles/")"#,
            ),
            ("go build cache", r#"file and path ~ "/.cache/go-build/""#),
            ("go module cache", r#"file and path ~ "/go/pkg/mod/""#),
            (
                "python bytecode caches",
                r#"file and path ~ "/__pycache__/" and ext = pyc"#,
            ),
            (
                "python package caches",
                r#"file and (path ~ "/.cache/pip/" or path ~ "/.cache/uv/")"#,
            ),
            (
                "python test and lint caches",
                r#"file and (path ~ "/.pytest_cache/" or path ~ "/.mypy_cache/" or path ~ "/.ruff_cache/")"#,
            ),
            ("node dependencies", r#"file and path ~ "/node_modules/""#),
            ("npm cache", r#"file and path ~ "/.npm/_cacache/""#),
            (
                "ccache files",
                r#"file and (path ~ "/.cache/ccache/" or path ~ "/.ccache/")"#,
            ),
        ],
    },
    RecipeGroup {
        label: "Coding agents and editors",
        recipes: &[
            (
                "project agent instructions",
                r#"file and (name in (AGENTS.md, CLAUDE.md, GEMINI.md) or path endswith "/.github/copilot-instructions.md")"#,
            ),
            (
                "claude code settings",
                r#"file and (path endswith "/.claude/settings.json" or path endswith "/.claude/settings.local.json")"#,
            ),
            (
                "claude code sessions",
                r#"file and path ~ "/.claude/projects/" and ext = jsonl"#,
            ),
            ("claude code logs", r#"file and path ~ "/.claude/debug/""#),
            (
                "codex settings",
                r#"file and path endswith "/.codex/config.toml""#,
            ),
            (
                "codex sessions",
                r#"file and (path ~ "/.codex/sessions/" or path ~ "/.codex/archived_sessions/") and ext = jsonl"#,
            ),
            ("codex logs", r#"file and path ~ "/.codex/log/""#),
            (
                "gemini cli settings",
                r#"file and path endswith "/.gemini/settings.json""#,
            ),
            (
                "gemini cli sessions",
                r#"file and path ~ "/.gemini/tmp/" and path ~ "/chats/" and ext = json"#,
            ),
            (
                "copilot cli settings",
                r#"file and path endswith "/.copilot/config.json""#,
            ),
            (
                "copilot cli sessions",
                r#"file and path ~ "/.copilot/session-state/""#,
            ),
            ("copilot cli logs", r#"file and path ~ "/.copilot/logs/""#),
            (
                "pi agent settings",
                r#"file and (path endswith "/.pi/agent/settings.json" or path endswith "/.pi/settings.json")"#,
            ),
            (
                "pi agent sessions",
                r#"file and path ~ "/.pi/agent/sessions/" and ext = jsonl"#,
            ),
            (
                "vscode settings",
                r#"file and (path endswith "/Code/User/settings.json" or path endswith "/.vscode/settings.json")"#,
            ),
            ("vscode logs", r#"file and path ~ "/.config/Code/logs/""#),
            (
                "vscode extensions",
                r#"file and path ~ "/.vscode/extensions/""#,
            ),
            (
                "vscode workspace storage",
                r#"file and path ~ "/.config/Code/User/workspaceStorage/""#,
            ),
            (
                "vscode chat sessions",
                r#"file and path ~ "/.config/Code/User/workspaceStorage/" and path ~ "/chatSessions/" and ext in (json, jsonl)"#,
            ),
            (
                "old coding agent sessions",
                r#"file and age > 30d and ((ext = jsonl and (path ~ "/.claude/projects/" or path ~ "/.codex/sessions/" or path ~ "/.codex/archived_sessions/" or path ~ "/.pi/agent/sessions/")) or (ext = json and path ~ "/.gemini/tmp/" and path ~ "/chats/") or path ~ "/.copilot/session-state/")"#,
            ),
        ],
    },
    RecipeGroup {
        label: "Local models and inference caches",
        recipes: &[
            (
                "hugging face cache",
                r#"file and path ~ "/.cache/huggingface/""#,
            ),
            (
                "hugging face model blobs",
                r#"file and path ~ "/.cache/huggingface/hub/models--" and path ~ "/blobs/""#,
            ),
            ("ollama models", r#"file and path ~ "/.ollama/models/""#),
            (
                "lm studio models",
                r#"file and path ~ "/.lmstudio/models/""#,
            ),
            ("vllm cache", r#"file and path ~ "/.cache/vllm/""#),
            (
                "vllm compile cache",
                r#"file and path ~ "/.cache/vllm/torch_compile_cache/""#,
            ),
            (
                "large local model files",
                r#"file and size > 1gib and ext in (gguf, safetensors)"#,
            ),
        ],
    },
];

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::{EntityKind, SearchContext, translate};
    use std::collections::HashSet;

    #[test]
    fn every_advertised_phrase_resolves_to_its_valid_unique_query() {
        let mut seen = HashSet::new();
        let context = SearchContext::new(1000, [1970, 1, 1]);
        for group in BUILTIN_RECIPE_GROUPS {
            for &(phrase, dsl) in group.recipes {
                assert!(seen.insert(phrase), "duplicate phrase {phrase}");
                let spaced = format!("  {}  ", phrase.to_uppercase().replace(' ', "  "));
                let result = translate(&spaced, context);
                assert!(result.is_valid(), "{phrase}: {:?}", result.diagnostics);
                assert_eq!(result.dsl.as_deref(), Some(dsl), "{phrase}");
                assert_eq!(result.entities[0].kind, EntityKind::Recipe);
                assert_eq!(result.entities[0].span, 0..spaced.len());
            }
        }
    }
}
