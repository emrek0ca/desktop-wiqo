//! What a build or a test run says went wrong, boiled down to `file:line message`.
//!
//! A compiler or a test runner prints pages; the answer is in a few lines of it. A person working in a terminal does not read the pages: `| grep error | head`
//! is the first thing they type. The model got the pages, shortened to their start and end (`clip`), and the middle (where the first errors are) went missing,
//! so it fixed what it could see and ran the build again to find the next one. This reads the whole output once and puts the errors first, counted and in order,
//! above it. It knows the formats of the tools people run most (TypeScript, Rust, Swift/Clang/GCC, Go, Python and pytest, ESLint, Jest/Vitest); on output it does
//! not recognise it says nothing.

use std::collections::BTreeSet;

/// At most this many distinct errors are listed.
const MOST_LISTED: usize = 25;

/// One error: where it is (when the tool says) and what it says.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
struct Problem {
    place: String,
    message: String,
}

fn short(text: &str, most: usize) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(most).collect()
}

/// `path:12:5: error: message` (Swift, Clang, GCC, Go vet, Biome) and `path(12,5): error TS2322: message` (tsc).
fn located(line: &str) -> Option<Problem> {
    let lower = line.to_lowercase();
    if !(lower.contains("error") || lower.contains("fatal") || lower.contains("failed")) {
        return None;
    }
    // path(12,5): error TS2322: ...
    if let (Some(open), Some(close)) = (line.find('('), line.find("): ")) {
        if open < close
            && line[open + 1..close]
                .chars()
                .all(|c| c.is_ascii_digit() || c == ',')
            && !line[..open].contains(' ')
        {
            let place = format!(
                "{}:{}",
                &line[..open],
                line[open + 1..close].split(',').next().unwrap_or("")
            );
            return Some(Problem {
                place,
                message: short(line[close + 3..].trim_start_matches("error "), 160),
            });
        }
    }
    // path:12:5: error: ...   (the path may contain a drive letter: C:\x\y.ts:12:5)
    let mut parts = line.splitn(5, ':');
    let first = parts.next()?;
    let (path, row) = if first.len() == 1 && first.chars().all(|c| c.is_ascii_alphabetic()) {
        (format!("{first}:{}", parts.next()?), parts.next()?)
    } else {
        (first.to_string(), parts.next()?)
    };
    if path.contains(' ') || row.is_empty() || !row.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut rest: Vec<&str> = parts.collect();
    // The column, when the tool gives one.
    if rest
        .first()
        .is_some_and(|column| !column.is_empty() && column.chars().all(|c| c.is_ascii_digit()))
    {
        rest.remove(0);
    }
    let message = rest.join(":");
    // What follows the position must open with the severity: `src/a.rs:12:  let error = x` (a search hit) is not an error.
    let lower = message.trim_start().to_lowercase();
    if !(lower.starts_with("error") || lower.starts_with("fatal error")) {
        return None;
    }
    Some(Problem {
        place: format!("{path}:{row}"),
        message: short(
            message.trim_start_matches(|c: char| c.is_ascii_digit() || c == ':' || c == ' '),
            160,
        ),
    })
}

fn problems_in(output: &str) -> (Vec<Problem>, usize) {
    let mut found: BTreeSet<Problem> = BTreeSet::new();
    let mut order: Vec<Problem> = Vec::new();
    let mut total = 0usize;
    let lines: Vec<&str> = output.lines().collect();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim_end();
        let trimmed = line.trim_start();
        let mut problem: Option<Problem> = None;
        // Rust: `error[E0599]: message` and, on the next lines, `--> src/main.rs:12:5`
        if (trimmed.starts_with("error[") || trimmed.starts_with("error:"))
            && !trimmed.starts_with("error: aborting")
            && !trimmed.starts_with("error: could not compile")
        {
            let message = trimmed
                .split_once("]: ")
                .map(|(_, rest)| rest)
                .or_else(|| trimmed.strip_prefix("error: "))
                .unwrap_or(trimmed);
            let place = lines.iter().skip(index + 1).take(4).find_map(|next| {
                next.trim_start()
                    .strip_prefix("--> ")
                    .map(|at| at.trim().to_string())
            });
            problem = Some(Problem {
                place: place.unwrap_or_default(),
                message: short(message, 160),
            });
        // pytest: `FAILED tests/test_a.py::test_b - AssertionError: assert 5 == 6`
        } else if let Some(rest) = trimmed
            .strip_prefix("FAILED ")
            .or_else(|| trimmed.strip_prefix("ERROR "))
            .filter(|rest| rest.contains("::") || rest.contains(".py"))
        {
            let (place, message) = rest.split_once(" - ").unwrap_or((rest, ""));
            problem = Some(Problem {
                place: place.trim().to_string(),
                message: short(message, 160),
            });
        // Jest / Vitest: `FAIL src/a.test.ts` and `● suite › test`
        } else if let Some(rest) = trimmed
            .strip_prefix("FAIL ")
            .filter(|rest| rest.contains('.') && !rest.contains(' '))
        {
            problem = Some(Problem {
                place: rest.to_string(),
                message: "testler düştü".to_string(),
            });
        // Python traceback: the last line names the error, the line before the frame names the place.
        } else if trimmed.starts_with("File \"") && trimmed.contains(", line ") {
            let place = trimmed
                .trim_start_matches("File \"")
                .split('"')
                .next()
                .unwrap_or("")
                .to_string()
                + ":"
                + trimmed
                    .split(", line ")
                    .nth(1)
                    .and_then(|row| row.split(',').next())
                    .unwrap_or("");
            let message = lines.iter().skip(index + 1).take(6).find(|next| {
                let t = next.trim_start();
                !t.is_empty()
                    && !t.starts_with("File \"")
                    && next.chars().next().is_some_and(|c| !c.is_whitespace())
                    && t.contains("Error")
            });
            if let Some(message) = message {
                problem = Some(Problem {
                    place,
                    message: short(message, 160),
                });
            }
        } else if let Some(found) = located(line) {
            problem = Some(found);
        }
        if let Some(problem) = problem {
            total += 1;
            if found.insert(problem.clone()) {
                order.push(problem);
            }
        }
        index += 1;
    }
    (order, total)
}

/// The errors of a build or test output, first ones first, or None when the output holds none this knows.
pub fn errors_of(output: &str) -> Option<String> {
    let (problems, total) = problems_in(output);
    if problems.is_empty() {
        return None;
    }
    let files: BTreeSet<&str> = problems
        .iter()
        .filter_map(|problem| {
            problem
                .place
                .rsplit_once(':')
                .map(|(path, _)| path)
                .or(Some(problem.place.as_str()))
                .filter(|path| !path.is_empty())
        })
        .collect();
    let mut lines = vec![format!(
        "HATA ÖZETİ: {total} hata satırı, {} ayrı hata, {} dosya. İlkleri sırayla (çıktının ortası kısaltılmış olabilir; bunlar tam taranmış çıktıdan):",
        problems.len(),
        files.len()
    )];
    for problem in problems.iter().take(MOST_LISTED) {
        lines.push(if problem.place.is_empty() {
            format!("  - {}", problem.message)
        } else {
            format!("  - {}  {}", problem.place, problem.message)
        });
    }
    if problems.len() > MOST_LISTED {
        lines.push(format!("  … ve {} hata daha", problems.len() - MOST_LISTED));
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::errors_of;

    #[test]
    fn typescript_errors_are_listed_with_file_and_line() {
        let output = "src/a.ts(12,5): error TS2322: Type 'string' is not assignable to type 'number'.\nsrc/b.ts(3,1): error TS2304: Cannot find name 'x'.\nFound 2 errors.";
        let digest = errors_of(output).expect("a digest");
        assert!(
            digest.contains("src/a.ts:12  TS2322: Type 'string' is not assignable")
                && digest.contains("src/b.ts:3  TS2304: Cannot find name 'x'")
        );
        assert!(digest.contains("2 ayrı hata, 2 dosya"));
    }

    #[test]
    fn swift_and_clang_errors_are_listed() {
        let output = "RovaIOS/CopilotView.swift:1710:43: error: cannot convert value of type 'A' to expected argument type 'String'\n  1710 | tone: x\nRovaIOS/A.swift:5:1: warning: unused\n";
        let digest = errors_of(output).expect("a digest");
        assert!(digest.contains("RovaIOS/CopilotView.swift:1710"));
        assert!(!digest.contains("A.swift:5"), "a warning is not an error");
    }

    #[test]
    fn rust_errors_take_their_place_from_the_arrow_line() {
        let output = "error[E0599]: no method named `foo` found for struct `Bar`\n   --> crates/x/src/lib.rs:41:9\n    |\nerror: could not compile `x` (lib) due to 1 previous error";
        let digest = errors_of(output).expect("a digest");
        assert!(digest.contains("crates/x/src/lib.rs:41:9  no method named `foo` found"));
        assert!(!digest.contains("could not compile"));
    }

    #[test]
    fn pytest_failures_and_python_tracebacks_are_listed() {
        let output = "FAILED tests/test_hesap.py::test_toplam - assert 5 == 6\n1 failed, 3 passed in 0.31s\n";
        assert!(errors_of(output)
            .expect("a digest")
            .contains("tests/test_hesap.py::test_toplam  assert 5 == 6"));
        let trace = "Traceback (most recent call last):\n  File \"/tmp/a.py\", line 7, in <module>\n    boom()\nZeroDivisionError: division by zero\n";
        assert!(errors_of(trace)
            .expect("a digest")
            .contains("/tmp/a.py:7  ZeroDivisionError: division by zero"));
    }

    #[test]
    fn a_search_hit_that_mentions_error_is_not_an_error() {
        assert!(errors_of(
            "src/a.rs:12:    let error = parse(x);\nsrc/b.rs:3:// error handling lives here\n"
        )
        .is_none());
    }

    #[test]
    fn a_repeated_error_is_counted_but_listed_once_and_clean_output_has_no_digest() {
        let line = "src/a.ts(1,1): error TS1: bad\n";
        let digest = errors_of(&line.repeat(5)).expect("a digest");
        assert!(digest.contains("5 hata satırı, 1 ayrı hata"));
        assert!(errors_of("Compiled successfully.\nAll tests passed.").is_none());
    }
}
