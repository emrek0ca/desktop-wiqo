//! What keeps private data on this computer while the local agent works (local.rs). The model runs on the server; whatever a tool reads here (a file,
//! a command's output, a page) used to reach it as it was. Now every result passes through this vault first: e-mail addresses, phone numbers, IBANs,
//! card numbers, Turkish ID numbers, API keys, private keys, tokens and passwords are swapped for stand-ins such as ⟦EPOSTA_1⟧. The model reasons with
//! the stand-ins; when it calls a tool with one ("send to ⟦EPOSTA_1⟧"), the real value is put back here, just before the tool runs. The values never
//! leave this computer through the model; the table lives only in this run (and its session file on this computer).

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

#[derive(Default)]
pub(super) struct Vault {
    by_stand_in: HashMap<String, String>,
    by_value: HashMap<String, String>,
    counts: HashMap<&'static str, usize>,
}

struct Rule {
    kind: &'static str,
    pattern: Regex,
    /// The capture group that holds the private part (0: all of the match).
    group: usize,
    check: fn(&str) -> bool,
}

fn always(_: &str) -> bool {
    true
}

/// A card number's own checksum (Luhn): a long number that fails it is just a number.
fn luhn(text: &str) -> bool {
    let digits: Vec<u32> = text.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let sum: u32 = digits.iter().rev().enumerate().map(|(i, d)| if i % 2 == 1 { let x = d * 2; if x > 9 { x - 9 } else { x } } else { *d }).sum();
    sum % 10 == 0
}

/// A Turkish ID number's own checksum: eleven digits that fail it are just a number.
fn tc_kimlik(text: &str) -> bool {
    let d: Vec<u32> = text.chars().filter_map(|c| c.to_digit(10)).collect();
    if d.len() != 11 || d[0] == 0 {
        return false;
    }
    let odd = d[0] + d[2] + d[4] + d[6] + d[8];
    let even = d[1] + d[3] + d[5] + d[7];
    let tenth = (odd * 7 + 100 - even) % 10;
    tenth == d[9] && (d[..10].iter().sum::<u32>() % 10) == d[10]
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let rule = |kind, pattern: &str, group, check| Rule { kind, pattern: Regex::new(pattern).expect("vault pattern"), group, check };
        vec![
            rule("OZEL_ANAHTAR", r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----", 0, always),
            rule("ANAHTAR", r"\b(?:sk-(?:proj-|ant-)?[A-Za-z0-9_\-]{20,}|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,}|AKIA[0-9A-Z]{16}|xox[abprs]-[A-Za-z0-9\-]{10,}|AIza[0-9A-Za-z_\-]{35}|gsk_[A-Za-z0-9]{40,})\b", 0, always),
            rule("JETON", r"\beyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\b", 0, always),
            rule("SIFRE", r#"(?i)(?:password|passwd|pwd|parola|şifre|sifre|secret|api[_-]?key|access[_-]?token|auth[_-]?token)\b\s*["']?\s*[:=]\s*["']?([^\s"',;]{6,})"#, 1, always),
            rule("IBAN", r"\b[A-Z]{2}\d{2}(?:[ ]?[A-Z0-9]{4}){3,7}(?:[ ]?[A-Z0-9]{1,4})?\b", 0, |text| text.starts_with("TR") || text.replace(' ', "").len() >= 15),
            rule("KART", r"\b(?:\d[ \-]?){12,18}\d\b", 0, luhn),
            rule("TCKN", r"\b[1-9]\d{10}\b", 0, tc_kimlik),
            rule("EPOSTA", r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}\b", 0, always),
            rule("TELEFON", r"(?:\+90[ \-]?|\b0?)5\d{2}[ \-]?\d{3}[ \-]?\d{2}[ \-]?\d{2}\b|\+\d{1,3}[ \-]?\(?\d{2,4}\)?[ \-]?\d{3,4}[ \-]?\d{3,4}\b", 0, always),
        ]
    })
}

fn is_stand_in(text: &str) -> bool {
    text.starts_with('⟦') && text.ends_with('⟧')
}

impl Vault {
    /// The table kept with the run's session on this computer, so a resumed run can still put the values back.
    pub(super) fn from_saved(saved: &Value) -> Vault {
        let mut vault = Vault::default();
        if let Some(entries) = saved.as_object() {
            for (stand_in, value) in entries {
                if let Some(value) = value.as_str() {
                    vault.by_stand_in.insert(stand_in.clone(), value.to_string());
                    vault.by_value.insert(value.to_string(), stand_in.clone());
                }
            }
        }
        vault
    }

    pub(super) fn saved(&self) -> Value {
        Value::Object(self.by_stand_in.iter().map(|(key, value)| (key.clone(), Value::String(value.clone()))).collect())
    }

    pub(super) fn len(&self) -> usize {
        self.by_stand_in.len()
    }

    fn stand_in(&mut self, kind: &'static str, value: &str) -> String {
        if let Some(known) = self.by_value.get(value) {
            return known.clone();
        }
        let count = self.counts.entry(kind).or_insert_with(|| self.by_stand_in.keys().filter(|key| key.starts_with(&format!("⟦{kind}_"))).count());
        *count += 1;
        let stand_in = format!("⟦{kind}_{count}⟧");
        self.by_stand_in.insert(stand_in.clone(), value.to_string());
        self.by_value.insert(value.to_string(), stand_in.clone());
        stand_in
    }

    /// The text as the model may see it: every private value swapped for its stand-in (the same value always gets the same one).
    pub(super) fn seal(&mut self, text: &str) -> String {
        let mut out = text.to_string();
        for rule in rules() {
            let mut found: Vec<(usize, usize, String)> = Vec::new();
            for captures in rule.pattern.captures_iter(&out) {
                let Some(part) = captures.get(rule.group) else { continue };
                let value = part.as_str();
                if is_stand_in(value) || !(rule.check)(value) {
                    continue;
                }
                found.push((part.start(), part.end(), value.to_string()));
            }
            for (start, end, value) in found.into_iter().rev() {
                let stand_in = self.stand_in(rule.kind, &value);
                out.replace_range(start..end, &stand_in);
            }
        }
        out
    }

    /// The text with every stand-in turned back into its value (for a tool's arguments, and the answer shown here).
    pub(super) fn open(&self, text: &str) -> String {
        if self.by_stand_in.is_empty() || !text.contains('⟦') {
            return text.to_string();
        }
        let mut out = text.to_string();
        for (stand_in, value) in &self.by_stand_in {
            if out.contains(stand_in.as_str()) {
                out = out.replace(stand_in.as_str(), value);
            }
        }
        out
    }

    /// A tool call's arguments with the real values back in, every string at any depth.
    pub(super) fn open_value(&self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.open(text)),
            Value::Array(items) => Value::Array(items.iter().map(|item| self.open_value(item)).collect()),
            Value::Object(map) => Value::Object(map.iter().map(|(key, item)| (key.clone(), self.open_value(item))).collect()),
            other => other.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_values_become_stand_ins_and_come_back() {
        let mut vault = Vault::default();
        let sealed = vault.seal("Ali: ali.veli@example.com, 0532 123 45 67, IBAN TR33 0006 1005 1978 6457 8413 26, key sk-proj-abcdefghijklmnopqrstuvwx");
        assert!(!sealed.contains("ali.veli@example.com"));
        assert!(!sealed.contains("0532 123 45 67"));
        assert!(!sealed.contains("TR33"));
        assert!(!sealed.contains("sk-proj"));
        assert!(sealed.contains("⟦EPOSTA_1⟧"));
        assert_eq!(vault.open(&sealed), "Ali: ali.veli@example.com, 0532 123 45 67, IBAN TR33 0006 1005 1978 6457 8413 26, key sk-proj-abcdefghijklmnopqrstuvwx");
        // The same value keeps its stand-in.
        assert_eq!(vault.seal("ali.veli@example.com"), "⟦EPOSTA_1⟧");
    }

    #[test]
    fn numbers_that_only_look_private_stay() {
        let mut vault = Vault::default();
        assert_eq!(vault.seal("1234567890123 satır, 12345678901 kayıt"), "1234567890123 satır, 12345678901 kayıt");
        assert!(vault.seal("kart 4111 1111 1111 1111").contains("⟦KART_1⟧"));
    }

    #[test]
    fn a_password_is_hidden_but_its_name_stays() {
        let mut vault = Vault::default();
        assert_eq!(vault.seal("DB_PASSWORD=hunter2secret"), "DB_PASSWORD=⟦SIFRE_1⟧");
    }

    #[test]
    fn tool_arguments_get_the_real_values() {
        let mut vault = Vault::default();
        let sealed = vault.seal("ayse@example.org");
        let args = serde_json::json!({ "to": sealed, "list": [sealed] });
        assert_eq!(vault.open_value(&args), serde_json::json!({ "to": "ayse@example.org", "list": ["ayse@example.org"] }));
        assert_eq!(Vault::from_saved(&vault.saved()).open(&sealed), "ayse@example.org");
    }
}
