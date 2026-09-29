//! `JUnit` XML report generation for CI integration.
//!
//! Converts [`VerifyReport`] results into `JUnit` XML format compatible with
//! GitHub Actions, GitLab CI, Jenkins, and other CI systems.

use std::time::Duration;

use crate::assertions::{CheckResult, VerifyReport};

/// A complete `JUnit` XML test suite for serialization.
#[derive(Debug)]
pub struct JunitReport {
    /// Name of the test suite (defaults to "victauri").
    pub name: String,
    /// Total wall-clock duration of the suite.
    pub duration: Duration,
    /// Individual test results.
    pub test_cases: Vec<JunitTestCase>,
}

/// A single test case within a `JUnit` report.
#[derive(Debug)]
pub struct JunitTestCase {
    /// Name of the test case.
    pub name: String,
    /// Name of the class/suite this case belongs to.
    pub classname: String,
    /// Duration of this specific test.
    pub duration: Duration,
    /// Failure message, if the test failed.
    pub failure: Option<JunitFailure>,
}

/// Failure details for a `JUnit` test case.
#[derive(Debug)]
pub struct JunitFailure {
    /// Failure type label.
    pub failure_type: String,
    /// Human-readable failure message.
    pub message: String,
}

impl JunitReport {
    /// Creates a report from a [`VerifyReport`] with the given suite name and duration.
    #[must_use]
    pub fn from_verify_report(report: &VerifyReport, suite_name: &str, duration: Duration) -> Self {
        let per_case = if report.results.is_empty() {
            Duration::ZERO
        } else {
            duration / report.results.len() as u32
        };

        let test_cases = report
            .results
            .iter()
            .map(|r| JunitTestCase::from_check_result(r, suite_name, per_case))
            .collect();

        Self {
            name: suite_name.to_string(),
            duration,
            test_cases,
        }
    }

    /// Renders the report as a `JUnit` XML string.
    #[must_use]
    pub fn to_xml(&self) -> String {
        let tests = self.test_cases.len();
        let failures = self
            .test_cases
            .iter()
            .filter(|t| t.failure.is_some())
            .count();
        let time = format_duration(self.duration);
        let name = xml_escape(&self.name);

        let mut xml = String::with_capacity(1024);
        xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
        xml.push_str(&format!(
            "<testsuite name=\"{name}\" tests=\"{tests}\" failures=\"{failures}\" errors=\"0\" time=\"{time}\">\n"
        ));

        for tc in &self.test_cases {
            let tc_name = xml_escape(&tc.name);
            let classname = xml_escape(&tc.classname);
            let tc_time = format_duration(tc.duration);

            if let Some(failure) = &tc.failure {
                let ftype = xml_escape(&failure.failure_type);
                let msg = xml_escape(&failure.message);
                xml.push_str(&format!(
                    "  <testcase name=\"{tc_name}\" classname=\"{classname}\" time=\"{tc_time}\">\n"
                ));
                xml.push_str(&format!(
                    "    <failure type=\"{ftype}\" message=\"{msg}\" />\n"
                ));
                xml.push_str("  </testcase>\n");
            } else {
                xml.push_str(&format!(
                    "  <testcase name=\"{tc_name}\" classname=\"{classname}\" time=\"{tc_time}\" />\n"
                ));
            }
        }

        xml.push_str("</testsuite>\n");
        xml
    }
}

impl JunitTestCase {
    /// Creates a test case from a [`CheckResult`].
    #[must_use]
    pub fn from_check_result(result: &CheckResult, classname: &str, duration: Duration) -> Self {
        let failure = if result.passed {
            None
        } else {
            Some(JunitFailure {
                failure_type: "AssertionError".to_string(),
                message: result.detail.clone(),
            })
        };

        Self {
            name: result.description.clone(),
            classname: classname.to_string(),
            duration,
            failure,
        }
    }
}

/// Writes a `JUnit` XML report to disk.
///
/// # Errors
///
/// Returns an IO error if the file cannot be written.
pub fn write_junit_report(report: &JunitReport, path: &std::path::Path) -> std::io::Result<()> {
    std::fs::write(path, report.to_xml())
}

fn format_duration(d: Duration) -> String {
    format!("{:.3}", d.as_secs_f64())
}

/// Escape `s` for an XML 1.0 ATTRIBUTE value (every value this report writes is an attribute).
///
/// Characters outside XML 1.0's `Char` production — the C0 controls other than tab/LF/CR
/// (e.g. the ESC of an ANSI colour code in a console error), U+FFFE and U+FFFF — cannot appear
/// in an XML 1.0 document at all, not even as character references; one of them made the whole
/// report unparseable (R5-XML1). They are replaced with U+FFFD. Tab, LF and CR are written as
/// character references: literally, attribute-value normalization would turn them into spaces.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            c if c < '\u{20}' || c == '\u{FFFE}' || c == '\u{FFFF}' => out.push('\u{FFFD}'),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass_result(desc: &str) -> CheckResult {
        CheckResult {
            description: desc.to_string(),
            passed: true,
            detail: String::new(),
        }
    }

    fn fail_result(desc: &str, detail: &str) -> CheckResult {
        CheckResult {
            description: desc.to_string(),
            passed: false,
            detail: detail.to_string(),
        }
    }

    #[test]
    fn empty_report_produces_valid_xml() {
        let report = VerifyReport { results: vec![] };
        let junit = JunitReport::from_verify_report(&report, "smoke", Duration::from_millis(100));
        let xml = junit.to_xml();

        assert!(xml.contains("<?xml version=\"1.0\""));
        assert!(xml.contains("tests=\"0\""));
        assert!(xml.contains("failures=\"0\""));
        assert!(xml.contains("</testsuite>"));
    }

    #[test]
    fn passing_checks_have_no_failure_element() {
        let report = VerifyReport {
            results: vec![pass_result("ipc healthy"), pass_result("no console errors")],
        };
        let junit = JunitReport::from_verify_report(&report, "health", Duration::from_secs(1));
        let xml = junit.to_xml();

        assert!(xml.contains("tests=\"2\""));
        assert!(xml.contains("failures=\"0\""));
        assert!(!xml.contains("<failure"));
        assert!(xml.contains("ipc healthy"));
        assert!(xml.contains("no console errors"));
    }

    #[test]
    fn failing_check_includes_failure_element() {
        let report = VerifyReport {
            results: vec![
                pass_result("connected"),
                fail_result("no ghost commands", "found 3 ghost commands"),
            ],
        };
        let junit = JunitReport::from_verify_report(&report, "ghosts", Duration::from_secs(2));
        let xml = junit.to_xml();

        assert!(xml.contains("tests=\"2\""));
        assert!(xml.contains("failures=\"1\""));
        assert!(xml.contains("<failure type=\"AssertionError\""));
        assert!(xml.contains("found 3 ghost commands"));
    }

    #[test]
    fn xml_escapes_special_chars() {
        let report = VerifyReport {
            results: vec![fail_result("value <\"test\"> & 'check'", "a & b < c > d")],
        };
        let junit = JunitReport::from_verify_report(&report, "escape", Duration::from_millis(50));
        let xml = junit.to_xml();

        assert!(xml.contains("&lt;"));
        assert!(xml.contains("&gt;"));
        assert!(xml.contains("&amp;"));
        assert!(xml.contains("&quot;"));
        assert!(xml.contains("&apos;"));
    }

    /// XML 1.0 `Char` production.
    fn is_xml_char(c: char) -> bool {
        matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..)
    }

    /// R5-XML1: page text (console errors, ANSI-coloured output…) carries C0 control
    /// characters. XML 1.0 forbids them outright, so one ESC made the whole report unparseable
    /// by CI (Python's parser rejected it). They are replaced; tab/newline/CR survive as
    /// character references (a literal newline in an attribute is normalized to a space).
    #[test]
    fn control_characters_never_reach_the_xml() {
        let detail = "\u{1b}[31mTypeError\u{1b}[0m: boom\u{0}\u{7}\u{8}\u{b}\u{c}\u{1f}\u{FFFE}\u{FFFF}\nline2\r\tend";
        let report = VerifyReport {
            results: vec![fail_result("check \u{1b}name", detail)],
        };
        let xml = JunitReport::from_verify_report(&report, "suite\u{2}", Duration::ZERO).to_xml();
        for c in xml.chars() {
            assert!(
                is_xml_char(c),
                "U+{:04X} is not an XML 1.0 char: {xml:?}",
                c as u32
            );
        }
        let failure = xml
            .lines()
            .find(|l| l.contains("<failure"))
            .expect("failure element");
        assert!(failure.contains("TypeError"), "{failure}");
        assert!(
            failure.contains("&#10;line2&#13;&#9;end"),
            "whitespace kept as character references: {failure}"
        );
        assert!(!failure.contains('\u{1b}'));
    }

    #[test]
    fn from_verify_report_distributes_duration() {
        let report = VerifyReport {
            results: vec![pass_result("a"), pass_result("b")],
        };
        let junit = JunitReport::from_verify_report(&report, "suite", Duration::from_secs(4));

        assert_eq!(junit.test_cases.len(), 2);
        assert_eq!(junit.test_cases[0].duration, Duration::from_secs(2));
        assert_eq!(junit.test_cases[1].duration, Duration::from_secs(2));
    }

    #[test]
    fn write_junit_report_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("results.xml");

        let report = VerifyReport {
            results: vec![pass_result("works")],
        };
        let junit = JunitReport::from_verify_report(&report, "test", Duration::from_millis(100));
        write_junit_report(&junit, &path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("<?xml version=\"1.0\""));
        assert!(content.contains("works"));
    }
}
