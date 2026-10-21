//! How a person names an application or a file, reduced to something comparable: case, diacritics, the Turkish dotless i, a ".app" suffix, a possessive or case
//! suffix ("Safari'yi", "Chrome’u") and filler words ("Safari uygulamasını") are all dropped.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;
use unicode_normalization::UnicodeNormalization;

const FILLER: [&str; 11] = [
    "uygulama",
    "uygulamasi",
    "uygulamasini",
    "uygulamayi",
    "uygulamasina",
    "uygulamasinda",
    "program",
    "programi",
    "programini",
    "app",
    "application",
];

/// One word, one spelling: case, diacritics and the Turkish dotless i are all dropped ("İşlem", "islem" and "ISLEM" are the same word).
pub fn fold(value: &str) -> String {
    value
        .replace('İ', "i")
        .to_lowercase()
        .replace('ı', "i")
        .nfd()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .collect()
}

pub fn normalize(value: &str) -> String {
    let mut text = value.trim().to_string();
    if text.to_lowercase().ends_with(".app") {
        text.truncate(text.len() - 4);
    }
    let folded: String = text
        .replace('İ', "i")
        .to_lowercase()
        .replace('ı', "i")
        .nfd()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .collect();
    let mut tokens: Vec<String> = Vec::new();
    for raw in folded.split_whitespace() {
        // "safari'yi", "chrome’u": the suffix after an apostrophe is grammar.
        let token = raw.split(['\'', '’', '`', '´']).next().unwrap_or("");
        let cleaned: String = token
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '+' {
                    c
                } else {
                    ' '
                }
            })
            .collect();
        tokens.extend(cleaned.split_whitespace().map(str::to_string));
    }
    let meaningful: Vec<&String> = tokens
        .iter()
        .filter(|token| !FILLER.contains(&token.as_str()) && token.as_str() != "the")
        .collect();
    if meaningful.is_empty() {
        tokens.join(" ")
    } else {
        meaningful
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// 1.0 exact, 0.93 whole-word prefix, otherwise a blend of shared words and edit distance.
pub fn score(name: &str, query: &str) -> f64 {
    let candidate = normalize(name);
    if candidate.is_empty() || query.is_empty() {
        return 0.0;
    }
    if candidate == query {
        return 1.0;
    }
    if candidate.starts_with(&format!("{query} ")) || query.starts_with(&format!("{candidate} ")) {
        return 0.93;
    }
    if candidate.starts_with(query) || query.starts_with(&candidate) {
        return 0.88;
    }
    let query_tokens: std::collections::HashSet<&str> = query.split(' ').collect();
    let candidate_tokens: std::collections::HashSet<&str> = candidate.split(' ').collect();
    let overlap = query_tokens.intersection(&candidate_tokens).count();
    let token_score = if query_tokens.is_empty() {
        0.0
    } else {
        overlap as f64 / query_tokens.len() as f64
    };
    let distance = levenshtein(&candidate, query);
    let longest = candidate.chars().count().max(query.chars().count());
    let edit_score = if longest == 0 {
        0.0
    } else {
        1.0 - distance as f64 / longest as f64
    };
    token_score * 0.6 + edit_score * 0.4
}

pub fn levenshtein(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (left_index, left_char) in left.chars().enumerate() {
        let mut current = vec![left_index + 1];
        for (right_index, right_char) in right.iter().enumerate() {
            let substitution = previous[right_index] + usize::from(left_char != *right_char);
            let insertion = current[right_index] + 1;
            let deletion = previous[right_index + 1] + 1;
            current.push(substitution.min(insertion).min(deletion));
        }
        previous = current;
    }
    *previous.last().unwrap_or(&0)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Group {
    mac_bundle: String,
    names: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct File {
    groups: Vec<Group>,
}

/// The other names people use for common applications (`contracts/companion/app-aliases.json`), found by the macOS bundle identifier or by any of the names.
pub struct AppAliases {
    by_bundle: HashMap<String, Vec<String>>,
    groups: Vec<Vec<String>>,
}

impl AppAliases {
    pub fn get() -> &'static AppAliases {
        static ALIASES: OnceLock<AppAliases> = OnceLock::new();
        ALIASES.get_or_init(|| {
            let file: File =
                serde_json::from_str(crate::APP_ALIASES_JSON).expect("app-aliases.json is valid");
            AppAliases {
                by_bundle: file
                    .groups
                    .iter()
                    .map(|group| (group.mac_bundle.clone(), group.names.clone()))
                    .collect(),
                groups: file.groups.into_iter().map(|group| group.names).collect(),
            }
        })
    }

    pub fn for_bundle(&self, bundle: &str) -> &[String] {
        self.by_bundle.get(bundle).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every name an application answers to, given one of them: "Notlar" also answers to "Notes".
    pub fn siblings_of(&self, name: &str) -> Vec<String> {
        let wanted = normalize(name);
        self.groups
            .iter()
            .find(|group| group.iter().any(|candidate| normalize(candidate) == wanted))
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoken_names_reduce_to_the_same_comparable_form() {
        assert_eq!(normalize("Safari'yi"), "safari");
        assert_eq!(normalize("Chrome’u"), "chrome");
        assert_eq!(normalize("Safari uygulamasını"), "safari");
        assert_eq!(normalize("Sistem Ayarları.app"), "sistem ayarlari");
        assert_eq!(normalize("İndirilenler"), "indirilenler");
    }

    #[test]
    fn scores_rank_exact_then_prefix_then_close() {
        assert_eq!(score("Safari", "safari"), 1.0);
        assert!(score("Google Chrome", "google") >= 0.93);
        assert!(score("Safari", "safri") > score("Safari", "firefox"));
    }

    #[test]
    fn a_turkish_and_an_english_name_reach_the_same_application() {
        let siblings = AppAliases::get().siblings_of("Notlar");
        assert!(siblings.iter().any(|name| name == "Notes"));
        assert!(AppAliases::get()
            .for_bundle("com.google.Chrome")
            .iter()
            .any(|name| name == "Chrome"));
    }
}
