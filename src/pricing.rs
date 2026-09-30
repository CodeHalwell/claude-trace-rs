//! Per-model token pricing used to estimate cost when an agent does not
//! report one itself.
//!
//! The built-in table covers the model families the supported agents drive
//! (Claude, GPT/o-series, Gemini, Qwen, Kimi, DeepSeek, GLM, Grok, …) at public
//! list prices per million tokens. It is an estimate for trend-spotting, not
//! billing data: prices change, and providers discount (batch, subscriptions,
//! regional tiers) in ways a log file cannot reveal.
//!
//! Users can override or extend the table with a JSON file — see
//! [`load_overrides`] — without waiting for a release when prices move.

use std::{
    path::{Path, PathBuf},
    sync::RwLock,
};

use serde::{Deserialize, Serialize};

use crate::event::TokenUsage;

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    #[serde(alias = "input")]
    pub input_per_mtok: f64,
    #[serde(alias = "output")]
    pub output_per_mtok: f64,
    #[serde(alias = "cache_read")]
    pub cache_read_per_mtok: f64,
    #[serde(alias = "cache_write", alias = "cache_creation")]
    pub cache_creation_per_mtok: f64,
}

impl Pricing {
    const fn new(input: f64, output: f64, cache_read: f64, cache_write: f64) -> Self {
        Self {
            input_per_mtok: input,
            output_per_mtok: output,
            cache_read_per_mtok: cache_read,
            cache_creation_per_mtok: cache_write,
        }
    }

    /// Standard Anthropic-style caching: reads at 0.1x, 5-minute writes at 1.25x.
    const fn std(input: f64, output: f64) -> Self {
        Self::new(input, output, input * 0.1, input * 1.25)
    }

    /// OpenAI/Google-style implicit caching: cached input at a discount, no
    /// separate write charge.
    const fn cached(input: f64, output: f64, cache_read: f64) -> Self {
        Self::new(input, output, cache_read, input)
    }

    pub fn cost(&self, u: &TokenUsage) -> f64 {
        let m = 1_000_000.0;
        (u.input as f64) / m * self.input_per_mtok
            + (u.output as f64) / m * self.output_per_mtok
            + (u.cache_read as f64) / m * self.cache_read_per_mtok
            + (u.cache_creation as f64) / m * self.cache_creation_per_mtok
    }
}

/// One table row: the first rule whose every `all` fragment occurs in the
/// lower-cased model name wins, so rows are ordered most specific first.
struct Rule {
    all: &'static [&'static str],
    price: Pricing,
}

const fn r(all: &'static [&'static str], price: Pricing) -> Rule {
    Rule { all, price }
}

/// Fallback when nothing matches: Claude Sonnet-class pricing, historically
/// the most common model behind these agents.
pub const DEFAULT: Pricing = Pricing::std(3.0, 15.0);

#[rustfmt::skip]
static BUILTIN: &[Rule] = &[
    // --- Anthropic Claude ---------------------------------------------------
    r(&["fable-5-1"], Pricing::new(10.0, 50.0, 0.25, 12.5)),
    r(&["fable-5.1"], Pricing::new(10.0, 50.0, 0.25, 12.5)),
    r(&["mythos-5-1"], Pricing::new(10.0, 50.0, 0.25, 12.5)),
    r(&["fable"], Pricing::std(10.0, 50.0)),
    r(&["mythos"], Pricing::std(10.0, 50.0)),
    r(&["opus-5-5"], Pricing::new(4.0, 20.0, 0.20, 5.0)),
    r(&["opus-5.5"], Pricing::new(4.0, 20.0, 0.20, 5.0)),
    r(&["opus-5"], Pricing::std(5.0, 25.0)),
    r(&["opus-4-5"], Pricing::std(5.0, 25.0)),
    r(&["opus-4.5"], Pricing::std(5.0, 25.0)),
    r(&["opus-4-6"], Pricing::std(5.0, 25.0)),
    r(&["opus-4.6"], Pricing::std(5.0, 25.0)),
    r(&["opus-4-7"], Pricing::std(5.0, 25.0)),
    r(&["opus-4.7"], Pricing::std(5.0, 25.0)),
    r(&["opus-4-8"], Pricing::std(5.0, 25.0)),
    r(&["opus-4.8"], Pricing::std(5.0, 25.0)),
    r(&["opus-4"], Pricing::std(15.0, 75.0)),   // Opus 4 / 4.1
    r(&["3-opus"], Pricing::std(15.0, 75.0)),
    r(&["opus"], Pricing::std(5.0, 25.0)),
    r(&["sonnet-5"], Pricing::new(2.0, 10.0, 0.20, 2.5)),
    r(&["sonnet"], Pricing::std(3.0, 15.0)),    // 3.5 / 3.7 / 4 / 4.5 / 4.6
    r(&["3-5-haiku"], Pricing::std(0.8, 4.0)),
    r(&["haiku-3-5"], Pricing::std(0.8, 4.0)),
    r(&["3-haiku"], Pricing::std(0.25, 1.25)),
    r(&["haiku"], Pricing::std(1.0, 5.0)),      // Haiku 4.5

    // --- OpenAI ---------------------------------------------------------------
    r(&["gpt-5", "nano"], Pricing::cached(0.05, 0.40, 0.005)),
    r(&["gpt-5", "mini"], Pricing::cached(0.25, 2.0, 0.025)),
    r(&["gpt-5", "pro"], Pricing::cached(15.0, 120.0, 15.0)),
    r(&["gpt-5"], Pricing::cached(1.25, 10.0, 0.125)),
    r(&["gpt5"], Pricing::cached(1.25, 10.0, 0.125)),
    r(&["codex-mini"], Pricing::cached(1.5, 6.0, 0.375)),
    r(&["gpt-4.1", "nano"], Pricing::cached(0.10, 0.40, 0.025)),
    r(&["gpt-4.1", "mini"], Pricing::cached(0.40, 1.60, 0.10)),
    r(&["gpt-4.1"], Pricing::cached(2.0, 8.0, 0.50)),
    r(&["gpt-4o", "mini"], Pricing::cached(0.15, 0.60, 0.075)),
    r(&["gpt-4o"], Pricing::cached(2.5, 10.0, 1.25)),
    r(&["gpt-4"], Pricing::cached(2.5, 10.0, 1.25)),
    r(&["gpt-oss"], Pricing::cached(0.10, 0.50, 0.10)),
    r(&["o4-mini"], Pricing::cached(1.10, 4.40, 0.275)),
    r(&["o3-mini"], Pricing::cached(1.10, 4.40, 0.55)),
    r(&["o3-pro"], Pricing::cached(20.0, 80.0, 20.0)),
    r(&["o3"], Pricing::cached(2.0, 8.0, 0.50)),
    r(&["o1"], Pricing::cached(15.0, 60.0, 7.5)),

    // --- Google Gemini --------------------------------------------------------
    r(&["gemini-3", "flash"], Pricing::cached(0.50, 3.0, 0.05)),
    r(&["gemini-3"], Pricing::cached(2.0, 12.0, 0.20)),
    r(&["gemini-2.5", "flash-lite"], Pricing::cached(0.10, 0.40, 0.025)),
    r(&["gemini-2.5", "flash"], Pricing::cached(0.30, 2.50, 0.075)),
    r(&["gemini-2.5", "pro"], Pricing::cached(1.25, 10.0, 0.31)),
    r(&["gemini-2.0", "flash"], Pricing::cached(0.10, 0.40, 0.025)),
    r(&["gemini", "flash"], Pricing::cached(0.30, 2.50, 0.075)),
    r(&["gemini"], Pricing::cached(1.25, 10.0, 0.31)),

    // --- Other providers commonly driven by coding agents ----------------------
    r(&["qwen3-coder", "flash"], Pricing::cached(0.30, 1.50, 0.06)),
    r(&["qwen3-coder"], Pricing::cached(1.0, 5.0, 0.20)),
    r(&["coder-model"], Pricing::cached(1.0, 5.0, 0.20)), // Qwen Code OAuth alias
    r(&["qwen"], Pricing::cached(0.40, 1.20, 0.08)),
    r(&["kimi"], Pricing::cached(0.60, 2.50, 0.15)),
    r(&["moonshot"], Pricing::cached(0.60, 2.50, 0.15)),
    r(&["k2"], Pricing::cached(0.60, 2.50, 0.15)),
    r(&["deepseek"], Pricing::cached(0.28, 0.42, 0.028)),
    r(&["glm"], Pricing::cached(0.60, 2.20, 0.11)),
    r(&["grok-code"], Pricing::cached(0.20, 1.50, 0.02)),
    r(&["grok"], Pricing::cached(3.0, 15.0, 0.75)),
    r(&["minimax"], Pricing::cached(0.30, 1.20, 0.03)),
    r(&["devstral"], Pricing::cached(0.40, 2.0, 0.40)),
    r(&["codestral"], Pricing::cached(0.30, 0.90, 0.30)),
    r(&["mistral"], Pricing::cached(0.40, 2.0, 0.40)),
];

/// A user-supplied pricing rule from the overrides file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverrideRule {
    /// Case-insensitive substring matched against the model name.
    #[serde(rename = "match")]
    pub pattern: String,
    #[serde(flatten)]
    pub price: Pricing,
}

static OVERRIDES: RwLock<Vec<OverrideRule>> = RwLock::new(Vec::new());

/// Default location of the overrides file:
/// `<config dir>/claude-trace-rs/pricing.json`.
pub fn default_overrides_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("rs", "claude-trace", "claude-trace-rs")
        .map(|d| d.config_dir().join("pricing.json"))
}

/// Load user pricing overrides from `path`. The file is a JSON array:
///
/// ```json
/// [{"match": "my-finetune", "input": 1.0, "output": 4.0,
///   "cache_read": 0.1, "cache_write": 1.25}]
/// ```
///
/// A missing file is not an error. Returns the number of rules loaded.
pub fn load_overrides(path: &Path) -> anyhow::Result<usize> {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e.into()),
    };
    let rules: Vec<OverrideRule> = serde_json::from_str(&body)?;
    set_overrides(rules.clone());
    Ok(rules.len())
}

pub fn set_overrides(mut rules: Vec<OverrideRule>) {
    for r in &mut rules {
        r.pattern = r.pattern.to_ascii_lowercase();
    }
    if let Ok(mut g) = OVERRIDES.write() {
        *g = rules;
    }
}

/// Price for a model name. User overrides win over the built-in table;
/// unknown or missing names fall back to [`DEFAULT`].
pub fn pricing_for(model: Option<&str>) -> Pricing {
    let m = model.unwrap_or("").to_ascii_lowercase();
    if m.is_empty() {
        return DEFAULT;
    }
    if let Ok(g) = OVERRIDES.read() {
        if let Some(o) = g.iter().find(|o| m.contains(&o.pattern)) {
            return o.price;
        }
    }
    BUILTIN
        .iter()
        .find(|rule| rule.all.iter().all(|frag| m.contains(frag)))
        .map(|rule| rule.price)
        .unwrap_or(DEFAULT)
}

/// Whether the model name is covered by the built-in table or an override.
pub fn is_known(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    OVERRIDES
        .read()
        .map(|g| g.iter().any(|o| m.contains(&o.pattern)))
        .unwrap_or(false)
        || BUILTIN
            .iter()
            .any(|rule| rule.all.iter().all(|frag| m.contains(frag)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mtok_in(model: &str) -> f64 {
        pricing_for(Some(model)).input_per_mtok
    }

    #[test]
    fn claude_generations_are_distinguished() {
        assert_eq!(mtok_in("claude-opus-5-5"), 4.0);
        assert_eq!(mtok_in("claude-opus-4-7"), 5.0);
        assert_eq!(mtok_in("claude-opus-4-1-20250805"), 15.0);
        assert_eq!(mtok_in("claude-opus-4-20250514"), 15.0);
        assert_eq!(mtok_in("claude-sonnet-5-5"), 2.0);
        assert_eq!(mtok_in("claude-sonnet-4-6"), 3.0);
        assert_eq!(mtok_in("claude-haiku-4-5"), 1.0);
        assert_eq!(mtok_in("claude-3-5-haiku-20241022"), 0.8);
        assert_eq!(mtok_in("claude-fable-5-1"), 10.0);
        assert_eq!(
            pricing_for(Some("claude-fable-5-1")).cache_read_per_mtok,
            0.25
        );
    }

    #[test]
    fn other_families() {
        assert_eq!(mtok_in("gpt-5-codex"), 1.25);
        assert_eq!(mtok_in("gpt-5-mini"), 0.25);
        assert_eq!(mtok_in("gemini-2.5-pro"), 1.25);
        assert_eq!(mtok_in("gemini-2.5-flash"), 0.30);
        assert_eq!(mtok_in("gemini-2.5-flash-lite"), 0.10);
        assert_eq!(mtok_in("kimi-k2-0905-preview"), 0.60);
        assert_eq!(mtok_in("o4-mini"), 1.10);
    }

    #[test]
    fn unknown_falls_back_to_default() {
        assert_eq!(pricing_for(None), DEFAULT);
        assert_eq!(pricing_for(Some("totally-new-model")), DEFAULT);
        assert!(!is_known("totally-new-model"));
        assert!(is_known("claude-opus-4-7"));
    }

    #[test]
    fn overrides_file_parses_and_wins() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pricing.json");
        std::fs::write(
            &p,
            r#"[{"match":"My-Tune","input":9.0,"output":9.0,"cache_read":0.9,"cache_write":9.0}]"#,
        )
        .unwrap();
        assert_eq!(load_overrides(&p).unwrap(), 1);
        assert_eq!(mtok_in("org/my-tune-v2"), 9.0);
        set_overrides(Vec::new());
        assert_eq!(load_overrides(&dir.path().join("missing.json")).unwrap(), 0);
    }
}
