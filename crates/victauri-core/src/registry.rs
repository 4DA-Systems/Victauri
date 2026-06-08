//! Thread-safe command registry with substring search and
//! natural-language-to-command resolution.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// Metadata for a registered Tauri command, including intent and schema information.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandInfo {
    /// Fully qualified command name (e.g. "`get_settings`").
    pub name: String,
    /// Plugin namespace, if the command belongs to a Tauri plugin.
    pub plugin: Option<String>,
    /// Human-readable description of what the command does.
    pub description: Option<String>,
    /// Ordered list of arguments the command accepts.
    pub args: Vec<CommandArg>,
    /// Rust return type as a string (e.g. "Result<Settings, Error>").
    pub return_type: Option<String>,
    /// Whether the command handler is async.
    pub is_async: bool,
    /// Natural-language intent phrase for NL-to-command resolution.
    pub intent: Option<String>,
    /// Grouping category (e.g. "settings", "counter").
    pub category: Option<String>,
    /// Example natural-language queries that should resolve to this command.
    pub examples: Vec<String>,
}

/// Schema for a single argument of a registered command.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandArg {
    /// Argument name as declared in the Rust function signature.
    pub name: String,
    /// Rust type name (e.g. "String", "`Option<u32>`").
    pub type_name: String,
    /// Whether the argument must be provided (not `Option`).
    pub required: bool,
    /// Optional JSON Schema for the argument's expected shape.
    pub schema: Option<serde_json::Value>,
}

/// Factory function submitted by `#[inspectable]` for auto-discovery.
///
/// Wraps a `fn() -> CommandInfo` so it can be registered via `inventory`
/// (function pointers are const-constructible, unlike `CommandInfo` with its `String` fields).
#[doc(hidden)]
pub struct CommandInfoFactory(pub fn() -> CommandInfo);

inventory::collect!(CommandInfoFactory);

impl CommandInfo {
    /// Creates a new command with the given name and all optional fields set to `None`/empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use victauri_core::CommandInfo;
    ///
    /// let cmd = CommandInfo::new("greet");
    /// assert_eq!(cmd.name, "greet");
    /// assert!(cmd.description.is_none());
    /// ```
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            plugin: None,
            description: None,
            args: Vec::new(),
            return_type: None,
            is_async: false,
            intent: None,
            category: None,
            examples: Vec::new(),
        }
    }

    /// Sets the description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the intent phrase for natural-language resolution.
    #[must_use]
    pub fn with_intent(mut self, intent: impl Into<String>) -> Self {
        self.intent = Some(intent.into());
        self
    }

    /// Sets the category.
    #[must_use]
    pub fn with_category(mut self, category: impl Into<String>) -> Self {
        self.category = Some(category.into());
        self
    }
}

/// Thread-safe registry of known Tauri commands, indexed by name.
#[derive(Debug, Clone)]
pub struct CommandRegistry {
    commands: Arc<RwLock<BTreeMap<String, CommandInfo>>>,
}

impl CommandRegistry {
    /// Creates an empty command registry.
    ///
    /// ```
    /// use victauri_core::CommandRegistry;
    ///
    /// let registry = CommandRegistry::new();
    /// assert_eq!(registry.count(), 0);
    /// assert!(registry.list().is_empty());
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            commands: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    /// Registers a command, replacing any existing entry with the same name.
    ///
    /// ```
    /// use victauri_core::{CommandRegistry, CommandInfo};
    ///
    /// let registry = CommandRegistry::new();
    /// registry.register(CommandInfo::new("greet").with_description("Say hello"));
    /// assert_eq!(registry.count(), 1);
    /// assert!(registry.get("greet").is_some());
    /// ```
    pub fn register(&self, info: CommandInfo) {
        crate::acquire_write(&self.commands, "CommandRegistry").insert(info.name.clone(), info);
    }

    /// Looks up a command by exact name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<CommandInfo> {
        crate::acquire_read(&self.commands, "CommandRegistry")
            .get(name)
            .cloned()
    }

    /// Returns all registered commands in alphabetical order.
    #[must_use]
    pub fn list(&self) -> Vec<CommandInfo> {
        crate::acquire_read(&self.commands, "CommandRegistry")
            .values()
            .cloned()
            .collect()
    }

    /// Returns the number of registered commands.
    #[must_use]
    pub fn count(&self) -> usize {
        crate::acquire_read(&self.commands, "CommandRegistry").len()
    }

    /// Searches commands by substring match on name or description (case-insensitive).
    ///
    /// # Examples
    ///
    /// ```
    /// use victauri_core::{CommandRegistry, CommandInfo};
    ///
    /// let registry = CommandRegistry::new();
    /// registry.register(
    ///     CommandInfo::new("get_settings").with_description("Retrieve app settings"),
    /// );
    /// let results = registry.search("settings");
    /// assert_eq!(results.len(), 1);
    /// assert_eq!(results[0].name, "get_settings");
    /// ```
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<CommandInfo> {
        let query_lower = query.to_lowercase();
        crate::acquire_read(&self.commands, "CommandRegistry")
            .values()
            .filter(|cmd| {
                cmd.name.to_lowercase().contains(&query_lower)
                    || cmd
                        .description
                        .as_ref()
                        .is_some_and(|d| d.to_lowercase().contains(&query_lower))
            })
            .cloned()
            .collect()
    }

    /// Resolves a natural-language query to commands ranked by relevance score.
    ///
    /// # Examples
    ///
    /// ```
    /// use victauri_core::{CommandRegistry, CommandInfo};
    ///
    /// let registry = CommandRegistry::new();
    /// registry.register(
    ///     CommandInfo::new("get_settings")
    ///         .with_description("Retrieve app settings")
    ///         .with_intent("fetch configuration")
    ///         .with_category("settings"),
    /// );
    /// let results = registry.resolve("get settings");
    /// assert!(!results.is_empty());
    /// assert!(results[0].score > 0.0);
    /// ```
    #[must_use]
    pub fn resolve(&self, query: &str) -> Vec<ScoredCommand> {
        // Scoring is O(commands × query_words × field_len), so an unbounded query
        // is a CPU/allocation DoS. Cap the query length (audit #20); a few hundred
        // chars is far more than any real natural-language command query.
        const MAX_QUERY_LEN: usize = 512;
        let query_lower: String = query
            .chars()
            .take(MAX_QUERY_LEN)
            .collect::<String>()
            .to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();
        if query_words.is_empty() {
            return Vec::new();
        }

        let mut scored: Vec<ScoredCommand> = crate::acquire_read(&self.commands, "CommandRegistry")
            .values()
            .filter_map(|cmd| {
                let score = score_command(cmd, &query_lower, &query_words);
                if score > 0.0 {
                    Some(ScoredCommand {
                        command: cmd.clone(),
                        score,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Primary: descending score. Secondary: a DETERMINISTIC tiebreak by command name
        // so equal-scoring commands never come back in arbitrary (HashMap iteration) order —
        // the "degenerate N-way tie with no tiebreak" of VIC-3. Combined with the name-coverage
        // term in `score_command`, ranking now degrades gracefully instead of opaquely.
        scored.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.command.name.cmp(&b.command.name))
        });
        scored
    }
}

/// Returns all commands registered via `#[inspectable]` auto-discovery.
///
/// Collects every `CommandInfoFactory` submitted by the `#[inspectable]` macro
/// and calls each factory to produce `CommandInfo` values.
#[must_use]
pub fn auto_discovered_commands() -> Vec<CommandInfo> {
    inventory::iter::<CommandInfoFactory>
        .into_iter()
        .map(|factory| (factory.0)())
        .collect()
}

impl CommandRegistry {
    /// Creates a registry pre-populated with all `#[inspectable]` commands.
    ///
    /// Uses `inventory` to collect every `CommandInfo` that was submitted at
    /// link time by the `#[inspectable]` macro. This replaces manual
    /// `register_commands!` or `.commands(&[...])` calls.
    ///
    /// ```
    /// use victauri_core::CommandRegistry;
    ///
    /// let registry = CommandRegistry::from_auto_discovery();
    /// // Contains all #[inspectable] commands from the binary
    /// ```
    #[must_use]
    pub fn from_auto_discovery() -> Self {
        let registry = Self::new();
        for info in auto_discovered_commands() {
            registry.register(info);
        }
        registry
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// A command paired with its relevance score from natural-language resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredCommand {
    /// The matched command metadata.
    pub command: CommandInfo,
    /// Relevance score (higher is better); 0 means no match.
    pub score: f64,
}

impl fmt::Display for ScoredCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (score: {:.2})", self.command.name, self.score)
    }
}

const SCORE_EXACT_NAME: f64 = 10.0;
const SCORE_NAME_SUBSTRING: f64 = 3.0;
const SCORE_NAME_WORD: f64 = 2.0;
const SCORE_DESCRIPTION: f64 = 1.5;
const SCORE_INTENT: f64 = 2.5;
/// Bonus when the query exactly matches a command's natural-language intent.
/// Mirrors [`SCORE_EXACT_NAME`]: an exact intent hit is the entire purpose of the
/// `intent` field, so it must dominate incidental name-substring matches in
/// unrelated commands. Without it, `resolve("increase counter")` ranks
/// `get_counter` (whose *name* contains "counter") above `increment` (whose
/// *intent* is literally "increase counter"). Kept below `SCORE_EXACT_NAME` so a
/// literal command-name match still edges out a natural-language intent match.
const SCORE_EXACT_INTENT: f64 = 8.0;
const SCORE_CATEGORY: f64 = 1.0;
const SCORE_EXAMPLE_FULL: f64 = 4.0;
const SCORE_EXAMPLE_WORD: f64 = 0.5;
/// Whole-command specificity bonus: the fraction of the command's NAME tokens covered by
/// the query, scaled by this weight. It rewards a more complete name match (a query word
/// hitting the short `settings` outranks the same word buried in `get_app_settings_v2`),
/// so when several commands match a single query word equally — the VIC-3 N-way tie — the
/// more specific one ranks higher. Small by design: it breaks near-ties without ever
/// overriding intent/exact-name/description signal.
const SCORE_NAME_COVERAGE: f64 = 1.0;

/// Scores a command against a query. Per-word contributions (substring, word,
/// description, intent, category, example-word matches) are normalized by query
/// length so scores remain comparable across queries of different word counts.
/// Whole-query bonuses (exact name match, full example match) are not normalized.
fn score_command(cmd: &CommandInfo, query_lower: &str, query_words: &[&str]) -> f64 {
    let mut score = 0.0;
    let mut exact_bonus = 0.0;
    let name_lower = cmd.name.to_lowercase();
    let name_words: Vec<&str> = name_lower.split('_').collect();

    if name_lower == query_lower.replace(' ', "_") {
        exact_bonus += SCORE_EXACT_NAME;
    }

    for word in query_words {
        if name_lower.contains(word) {
            score += SCORE_NAME_SUBSTRING;
        }
        if name_words.contains(word) {
            score += SCORE_NAME_WORD;
        }
    }

    if let Some(desc) = &cmd.description {
        let desc_lower = desc.to_lowercase();
        for word in query_words {
            if desc_lower.contains(word) {
                score += SCORE_DESCRIPTION;
            }
        }
    }

    if let Some(intent) = &cmd.intent {
        let intent_lower = intent.to_lowercase();
        if intent_lower.as_str() == query_lower {
            exact_bonus += SCORE_EXACT_INTENT;
        }
        for word in query_words {
            if intent_lower.contains(word) {
                score += SCORE_INTENT;
            }
        }
    }

    if let Some(category) = &cmd.category {
        let cat_lower = category.to_lowercase();
        for word in query_words {
            if cat_lower.contains(word) {
                score += SCORE_CATEGORY;
            }
        }
    }

    for example in &cmd.examples {
        let ex_lower = example.to_lowercase();
        if ex_lower.contains(query_lower) {
            exact_bonus += SCORE_EXAMPLE_FULL;
            break;
        }
        for word in query_words {
            if ex_lower.contains(word) {
                score += SCORE_EXAMPLE_WORD;
            }
        }
    }

    // Name-coverage specificity (graceful tiebreak — see SCORE_NAME_COVERAGE). Fraction of
    // the command's name tokens the query covers; a whole-command bonus, not per-word.
    let matched_name_words = name_words
        .iter()
        .filter(|w| !w.is_empty() && query_words.contains(w))
        .count();
    let name_coverage = if name_words.is_empty() {
        0.0
    } else {
        matched_name_words as f64 / name_words.len() as f64
    };

    // Normalize per-word contributions so scores are comparable across queries of different lengths.
    let word_count = query_words.len() as f64;
    let per_word_score = score / word_count;
    exact_bonus + per_word_score + SCORE_NAME_COVERAGE * name_coverage
}
