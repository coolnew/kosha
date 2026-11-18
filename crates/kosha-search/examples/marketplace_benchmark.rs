// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: 2026 Neeraj Sharma <neerajsharma.9@outlook.com> and The kosha contributors

//! Deterministic recipe-only stress probe; no scans, files, network or clock I/O.
use kosha_search::{RecipeBook, SearchContext, SearchMode};
use serde_json::json;
use std::{hint::black_box, time::Instant};

fn letters(mut n: usize) -> String {
    let mut s = String::new();
    for _ in 0..4 {
        s.push((b'a' + (n % 26) as u8) as char);
        n /= 26;
    }
    s
}

fn measure(name: &str, book: &RecipeBook, input: &str, runs: usize) {
    let context = SearchContext::new(1795003200, [2026, 11, 18]);
    for _ in 0..10 {
        black_box(book.translate_with_resolver(input, context, SearchMode::Natural, |_| None));
    }
    let mut samples = vec![];
    for _ in 0..7 {
        let start = Instant::now();
        for _ in 0..runs {
            black_box(book.translate_with_resolver(
                black_box(input),
                context,
                SearchMode::Natural,
                |_| None,
            ));
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6 / runs as f64);
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "{name}: median_us={:.3} min_us={:.3} runs={runs}",
        samples[3], samples[0]
    );
}

fn main() {
    let start = Instant::now();
    let mut exact = RecipeBook::default();
    for pack in 0..32 {
        let rules: Vec<_> = (0..256).map(|i| json!({"phrase":format!("review batch {}",pack*256+i),"dsl":"file and ext = md"})).collect();
        exact
            .append_fallback(RecipeBook::from_json(&json!(rules).to_string()).unwrap())
            .unwrap();
    }
    println!(
        "exact_8192_load_ms={:.3}",
        start.elapsed().as_secs_f64() * 1e3
    );
    measure("exact_8192_last_hit", &exact, "review batch 8191", 2000);
    measure(
        "exact_8192_builtin",
        &exact,
        "files greater than 10 MB",
        2000,
    );
    let mut patterns = RecipeBook::default();
    for pack in 0..2 {
        let rules: Vec<_> = (0..64).map(|i| json!({"type":"regex","phrase":format!("item value end{}",pack*64+i),"pattern":format!("item (?P<value>.+) end{}",pack*64+i),"bindings":{"value":"path"},"dsl":"file and path ~ {value}"})).collect();
        patterns
            .append_fallback(RecipeBook::from_json(&json!(rules).to_string()).unwrap())
            .unwrap();
    }
    measure(
        "patterns_128_long_miss",
        &patterns,
        &format!("item {}", "x".repeat(16000)),
        10,
    );
    let rules: Vec<_> = (0..32)
        .map(|i| {
            let phrase = format!("{} {}", "a".repeat(230), letters(i * 32));
            let aliases: Vec<_> = (1..32)
                .map(|j| format!("{} {}", "a".repeat(230), letters(i * 32 + j)))
                .collect();
            json!({"type":"fuzzy-suggestions","phrase":phrase,"aliases":aliases,"dsl":"file"})
        })
        .collect();
    let fuzzy = RecipeBook::from_json(&json!(rules).to_string()).unwrap();
    measure(
        "fuzzy_1024_long_near_miss",
        &fuzzy,
        &format!("{} zzzz", "a".repeat(230)),
        5,
    );
    let rules: Vec<_> = (0..32).map(|i| json!({"type":"fuzzy-suggestions",
        "phrase":format!("{} {}","ab".repeat(115),letters(i*32)),
        "aliases":(1..32).map(|j|format!("{} {}","ab".repeat(115),letters(i*32+j))).collect::<Vec<_>>(),
        "dsl":"file"})).collect();
    let shifted = RecipeBook::from_json(&json!(rules).to_string()).unwrap();
    measure(
        "fuzzy_1024_shifted_near_miss",
        &shifted,
        &format!("{} zzzz", "ba".repeat(115)),
        5,
    );
}
