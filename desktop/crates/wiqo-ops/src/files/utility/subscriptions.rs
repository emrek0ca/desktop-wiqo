//! The subscription hunt (`findSubscriptions`): the bank and card statements and the e-invoices in the person's folders are read here, on their
//! computer, and what is charged again and again is found: the same merchant at about the same amount every week, month or year, and whether its
//! last charge went up. Only that list leaves the computer (the server builds the card from it, rova-backend money/subscriptions.ts); no
//! statement, line or balance does.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use regex::Regex;
use wiqo_contract::names::fold;

use super::*;

const MAX_FILES: usize = 120;
const MAX_DAYS: u64 = 430;
const TYPES: [&str; 5] = ["pdf", "csv", "xlsx", "xls", "txt"];
/// Lines that are a statement's own bookkeeping, not a merchant: carried balance, interest and its taxes, totals, refunds.
const NOT_A_CHARGE: [&str; 14] = [
    "devir", "bakiye", "faiz", "bsmv", "kkdf", "toplam", "iade", "refund", "asgari", "odeme tutari", "son odeme", "ekstre", "limit", "puan",
];

/// Names of the services that take a payment for someone else.
const PLATFORMS: [&str; 9] = ["google", "apple", "paypal", "iyzico", "amzn", "amazon", "param", "microsoft", "steam"];

#[derive(Debug, Clone, PartialEq)]
struct Charge {
    date: NaiveDate,
    merchant: String,
    key: String,
    amount: f64,
    currency: &'static str,
}

fn patterns() -> &'static (Regex, Regex) {
    static PATTERNS: std::sync::OnceLock<(Regex, Regex)> = std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        (
            Regex::new(r"(?x)(?P<d>\b\d{1,2})[./-](?P<m>\d{1,2})[./-](?P<y>\d{4}|\d{2})\b").expect("date pattern"),
            // 1.234,56 · 1234,56 · 1,234.56 · 1234.56, with an optional minus and currency next to it.
            Regex::new(r"(?x)(?P<sign>-)?\s*(?P<n>\d{1,3}(?:[.,]\d{3})*[.,]\d{2})\s*(?P<c>TL|TRY|₺|USD|\$|EUR|€)?").expect("amount pattern"),
        )
    })
}

fn parse_amount(raw: &str) -> Option<f64> {
    let decimal = raw.len().checked_sub(3)?;
    let (whole, cents) = raw.split_at(decimal);
    let whole: String = whole.chars().filter(char::is_ascii_digit).collect();
    format!("{whole}.{}", &cents[1..]).parse().ok()
}

fn currency(mark: Option<&str>) -> &'static str {
    match mark {
        Some("USD" | "$") => "USD",
        Some("EUR" | "€") => "EUR",
        _ => "TRY",
    }
}

/// The merchant's words, as a statement prints them: references, card numbers, dates and city or country codes taken off.
fn merchant_of(text: &str) -> Option<(String, String)> {
    let cleaned: String = text
        .split_whitespace()
        .filter(|word| {
            let digits = word.chars().filter(char::is_ascii_digit).count();
            digits * 2 < word.chars().count().max(1) && !word.contains('*') && word.chars().count() > 1
        })
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    let cleaned = cleaned.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    let folded = fold(&cleaned);
    if folded.chars().filter(|c| c.is_alphabetic()).count() < 3 || NOT_A_CHARGE.iter().any(|word| folded.contains(word)) {
        return None;
    }
    // The key is the merchant's first word: "NETFLIX.COM AMSTERDAM NL" and "NETFLIX.COM 866-579" are one merchant. A payment platform's name
    // says only who took the money, so the word after it is kept too ("GOOGLE *YouTube" and "GOOGLE *Cloud" are two).
    let words: Vec<&str> = folded
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.len() >= 2 && !["www", "com", "tr"].contains(word))
        .collect();
    let first = *words.first()?;
    let key = if PLATFORMS.contains(&first) { words.iter().take(2).copied().collect::<Vec<_>>().join(" ") } else { first.to_string() };
    (!key.is_empty()).then(|| (display_name(&cleaned), key))
}

fn display_name(text: &str) -> String {
    text.split_whitespace()
        .take(3)
        .map(|word| {
            let lower = word.to_lowercase();
            let mut chars = lower.chars();
            chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every charge a statement's lines hold: a line with a date, words and an amount is a charge; the last amount on the line is taken (a line often
/// ends with the running balance only in bank exports, which then prints it after the amount, so the first one after the words is the charge).
fn charges(text: &str) -> Vec<Charge> {
    let (date_pattern, amount_pattern) = patterns();
    let mut found = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(date_match) = date_pattern.captures(line) else { continue };
        let day: u32 = date_match["d"].parse().unwrap_or(0);
        let month: u32 = date_match["m"].parse().unwrap_or(0);
        let mut year: i32 = date_match["y"].parse().unwrap_or(0);
        if year < 100 {
            year += 2000;
        }
        let Some(date) = NaiveDate::from_ymd_opt(year, month, day) else { continue };
        let rest = &line[date_match.get(0).map(|m| m.end()).unwrap_or(0)..];
        let Some(amount_match) = amount_pattern.captures(rest) else { continue };
        let Some(whole) = amount_match.get(0) else { continue };
        let words = rest[..whole.start()].replace([';', '\t', '"'], " ");
        let Some(amount) = parse_amount(&amount_match["n"]) else { continue };
        if amount <= 0.0 || amount > 1_000_000.0 {
            continue;
        }
        let Some((merchant, key)) = merchant_of(&words) else { continue };
        found.push(Charge { date, merchant, key, amount, currency: currency(amount_match.name("c").map(|m| m.as_str())) });
    }
    found
}

/// An e-invoice is one charge: who issued it (its first line with letters), its date and the amount payable.
fn invoice_charge(text: &str) -> Option<Charge> {
    let folded = fold(text);
    if !(folded.contains("e-arsiv") || folded.contains("e-fatura") || folded.contains("earsiv")) {
        return None;
    }
    let (date_pattern, amount_pattern) = patterns();
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let issuer = lines.iter().find(|line| line.chars().filter(|c| c.is_alphabetic()).count() >= 4 && !fold(line).contains("fatura"))?;
    let date_line = lines.iter().find(|line| fold(line).contains("fatura tarihi")).or_else(|| lines.iter().find(|line| date_pattern.is_match(line)))?;
    let date_match = date_pattern.captures(date_line)?;
    let mut year: i32 = date_match["y"].parse().ok()?;
    if year < 100 {
        year += 2000;
    }
    let date = NaiveDate::from_ymd_opt(year, date_match["m"].parse().ok()?, date_match["d"].parse().ok()?)?;
    let total_line = lines.iter().find(|line| {
        let folded = fold(line);
        folded.contains("odenecek tutar") || folded.contains("vergiler dahil") || folded.contains("genel toplam")
    })?;
    let amount_match = amount_pattern.captures_iter(total_line).last()?;
    let amount = parse_amount(&amount_match["n"])?;
    let (merchant, key) = merchant_of(issuer)?;
    Some(Charge { date, merchant, key, amount, currency: currency(amount_match.name("c").map(|m| m.as_str())) })
}

#[derive(Debug, PartialEq)]
struct Recurring {
    name: String,
    amount: f64,
    currency: &'static str,
    period: &'static str,
    count: usize,
    last: NaiveDate,
    previous: Option<f64>,
}

/// The merchants charged on a rhythm: at least two charges whose usual gap is a week, a month or a year, at amounts close to each other.
fn recurring(mut all: Vec<Charge>) -> Vec<Recurring> {
    all.sort_by_key(|charge| charge.date);
    all.dedup_by(|a, b| a.key == b.key && a.date == b.date && (a.amount - b.amount).abs() < 0.005);
    let mut by_merchant: BTreeMap<String, Vec<Charge>> = BTreeMap::new();
    for charge in all {
        by_merchant.entry(charge.key.clone()).or_default().push(charge);
    }
    let mut found = Vec::new();
    for charges in by_merchant.into_values() {
        if charges.len() < 2 {
            continue;
        }
        let mut gaps: Vec<i64> = charges.windows(2).map(|pair| (pair[1].date - pair[0].date).num_days()).filter(|gap| *gap > 0).collect();
        if gaps.is_empty() {
            continue;
        }
        gaps.sort_unstable();
        let gap = gaps[gaps.len() / 2];
        let period = match gap {
            6..=8 => "weekly",
            26..=35 => "monthly",
            355..=375 => "yearly",
            _ => continue,
        };
        let mut amounts: Vec<f64> = charges.iter().map(|charge| charge.amount).collect();
        amounts.sort_by(f64::total_cmp);
        let median = amounts[amounts.len() / 2];
        if charges.iter().filter(|charge| (charge.amount - median).abs() <= median * 0.35).count() * 3 < charges.len() * 2 {
            continue;
        }
        let last = charges.last().expect("two charges");
        let previous = charges.iter().rev().nth(1).map(|charge| charge.amount).filter(|amount| (amount - last.amount).abs() >= 0.01);
        // The plainest of the ways the statement wrote it ("Netflix.com", not "Netflix.com Amsterdam Nl").
        let name = charges.iter().map(|charge| charge.merchant.as_str()).min_by_key(|name| name.chars().count()).unwrap_or(&last.merchant).to_string();
        found.push(Recurring { name, amount: last.amount, currency: last.currency, period, count: charges.len(), last: last.date, previous });
    }
    found.sort_by(|left, right| monthly(right).total_cmp(&monthly(left)));
    found
}

fn monthly(item: &Recurring) -> f64 {
    match item.period {
        "weekly" => item.amount * 52.0 / 12.0,
        "yearly" => item.amount / 12.0,
        _ => item.amount,
    }
}

fn folders(request: &Request) -> Result<Vec<PathBuf>> {
    let asked: Vec<String> = request.paths.clone().unwrap_or_default().into_iter().chain(request.path.clone()).collect();
    if asked.is_empty() {
        return Ok([dirs::download_dir(), dirs::document_dir()].into_iter().flatten().filter(|dir| dir.is_dir()).collect());
    }
    asked.iter().map(|path| existing_folder(&Some(path.clone()))).collect()
}

pub(super) fn find_subscriptions(request: &Request) -> Result<Done> {
    let roots = folders(request)?;
    let newest = SystemTime::now() - Duration::from_secs(MAX_DAYS * 86_400);
    let mut files: Vec<PathBuf> = roots
        .iter()
        .flat_map(|root| regular_files(root, true, 4_000))
        .filter(|path| TYPES.contains(&extension_of(path).as_str()) && modified(path) >= newest)
        .collect();
    files.sort_by_key(|path| std::cmp::Reverse(modified(path)));
    files.truncate(MAX_FILES);
    let mut all = Vec::new();
    let mut read = 0;
    for file in &files {
        let Ok(text) = crate::files::documents::extract_text(file) else { continue };
        if let Some(charge) = invoice_charge(&text) {
            all.push(charge);
            read += 1;
            continue;
        }
        let found = charges(&text);
        // A file with only a couple of dated amounts is not a statement (a receipt, a letter).
        if found.len() >= 3 {
            read += 1;
            all.extend(found);
        }
    }
    let items = recurring(all);
    let currency = items.first().map(|item| item.currency).unwrap_or("TRY");
    let total: f64 = items.iter().filter(|item| item.currency == currency).map(monthly).sum();
    let value = json!({
        "items": items.iter().map(|item| json!({
            "name": item.name, "amount": (item.amount * 100.0).round() / 100.0, "currency": item.currency, "period": item.period,
            "count": item.count, "lastDate": item.last.format("%Y-%m-%d").to_string(),
            "previousAmount": item.previous.map(|amount| (amount * 100.0).round() / 100.0),
        })).collect::<Vec<_>>(),
        "monthlyTotal": (total * 100.0).round() / 100.0, "currency": currency, "files": read,
    });
    let message = if read == 0 {
        format!("{} dosyaya bakıldı; içlerinde banka ya da kart ekstresi, e-fatura bulunamadı. Ekstreleri PDF, CSV ya da Excel olarak indirip yeniden dene.", files.len())
    } else if items.is_empty() {
        format!("{read} ekstre ve fatura tarandı; düzenli tekrar eden bir ödeme bulunamadı.")
    } else {
        format!("{read} ekstre ve fatura tarandı; {} tekrarlayan ödeme bulundu, aylık toplam yaklaşık {:.2} {currency}.", items.len(), total)
    };
    Ok(Done::new(message, Some(data("subscriptions", value))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statement_lines_become_charges_and_a_monthly_rhythm_a_subscription() {
        let statement = "Tarih Açıklama Tutar\n\
            05.08.2026 NETFLIX.COM AMSTERDAM NL 229,99 TL\n\
            05.09.2026 NETFLIX.COM 866-579 229,99 TL\n\
            05.10.2026 NETFLIX.COM AMSTERDAM NL 279,99 TL\n\
            12.10.2026 MİGROS KADIKÖY ISTANBUL 1.245,30 TL\n\
            15.10.2026 ÖNCEKİ DÖNEM DEVİR 5.000,00 TL\n\
            20.10.2026 SPOTIFY P1 STOCKHOLM 99,00 TL\n";
        let found = charges(statement);
        assert_eq!(found.len(), 5, "the carried balance is not a charge: {found:?}");
        let items = recurring(found);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Netflix.com");
        assert_eq!(items[0].period, "monthly");
        assert_eq!(items[0].amount, 279.99);
        assert_eq!(items[0].previous, Some(229.99));
        assert_eq!(items[0].count, 3);
    }

    #[test]
    fn an_e_invoice_is_one_charge_from_its_issuer() {
        let invoice = "TURKCELL İLETİŞİM HİZMETLERİ A.Ş.\ne-Arşiv Fatura\nFatura Tarihi: 01.10.2026\nÖdenecek Tutar 349,90 TL\n";
        let charge = invoice_charge(invoice).expect("an invoice");
        assert_eq!(charge.amount, 349.9);
        assert_eq!(charge.date, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
        assert!(charge.merchant.starts_with("Turkcell"));
    }

    #[test]
    fn amounts_in_both_notations_are_read() {
        assert_eq!(parse_amount("1.245,30"), Some(1245.3));
        assert_eq!(parse_amount("1,245.30"), Some(1245.3));
        assert_eq!(parse_amount("99,00"), Some(99.0));
    }
}
