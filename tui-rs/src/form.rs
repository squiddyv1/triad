//! The new-engagement form: its fields, the validation, and the argv the CLI is handed.
//!
//! This is a faithful port of `tui/src/control.ts` and the `Form` component in
//! `tui/src/index.tsx`. The field names, defaults, focus order, validation wording and the
//! `scan`/`engage` argv all match the Ink dashboard, because that version is the contract
//! the CLI was written against. Keeping the rule in one pure module means the dashboard and
//! the unit tests cannot drift from it.

use std::env;
use std::path::{Path, PathBuf};

/// Which command the submit runs: a single Strix scan, or the full Cairn engagement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Scan,
    Engage,
}

/// The scan depth, matching `--mode` on `triad scan`/`triad engage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    Quick,
    Standard,
    Deep,
}

impl ScanMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ScanMode::Quick => "quick",
            ScanMode::Standard => "standard",
            ScanMode::Deep => "deep",
        }
    }

    /// The order the `←`/`→` keys step through, as the Ink form uses.
    const ORDER: [ScanMode; 3] = [ScanMode::Quick, ScanMode::Standard, ScanMode::Deep];

    fn index(self) -> usize {
        Self::ORDER
            .iter()
            .position(|mode| *mode == self)
            .unwrap_or(0)
    }

    fn step(self, forward: bool) -> ScanMode {
        let len = Self::ORDER.len();
        let next = if forward {
            (self.index() + 1) % len
        } else {
            (self.index() + len - 1) % len
        };
        Self::ORDER[next]
    }
}

/// The fields a submit turns into an argv. Mirrors `NewEngagement` in the Ink app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEngagement {
    pub flow: Flow,
    pub target: String,
    pub title: String,
    pub goal: String,
    pub mode: ScanMode,
}

/// The editable form state: the fields plus the focused row and the last validation error.
/// Rows are `0 flow`, `1 target`, `2 title`, `3 goal`, `4 mode`, in the Ink focus order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormState {
    pub flow: Flow,
    pub target: String,
    pub title: String,
    pub goal: String,
    pub mode: ScanMode,
    pub row: usize,
    pub error: Option<String>,
}

/// The number of focusable rows; `tab` wraps within `0..=LAST_ROW`.
pub const LAST_ROW: usize = 4;

impl FormState {
    /// `BLANK_FORM`: scan, empty target/title/goal, quick mode. The Ink dashboard opens on
    /// row 1 (`target`), not row 0, and this keeps `row` at 0 so the caller can choose.
    pub fn blank() -> Self {
        Self {
            flow: Flow::Scan,
            target: String::new(),
            title: String::new(),
            goal: String::new(),
            mode: ScanMode::Quick,
            row: 0,
            error: None,
        }
    }

    /// The required fields that are still empty, in the order the Ink app reports them.
    /// `goal` is only required for an engagement; a scan ignores it.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.target.trim().is_empty() {
            missing.push("target");
        }
        if self.title.trim().is_empty() {
            missing.push("title");
        }
        if self.flow == Flow::Engage && self.goal.trim().is_empty() {
            missing.push("goal");
        }
        missing
    }

    /// `missing target, title`, exactly the text the Ink form shows; `None` when valid.
    pub fn validation_error(&self) -> Option<String> {
        let missing = self.missing();
        if missing.is_empty() {
            None
        } else {
            Some(format!("missing {}", missing.join(", ")))
        }
    }

    /// Validate and report: sets (or clears) the error line, returning whether the form may
    /// be submitted. An empty form never submits.
    pub fn check(&mut self) -> bool {
        let error = self.validation_error();
        let ok = error.is_none();
        self.error = error;
        ok
    }

    pub fn engagement(&self) -> NewEngagement {
        NewEngagement {
            flow: self.flow,
            target: self.target.clone(),
            title: self.title.clone(),
            goal: self.goal.clone(),
            mode: self.mode,
        }
    }

    pub fn toggle_flow(&mut self) {
        self.flow = match self.flow {
            Flow::Scan => Flow::Engage,
            Flow::Engage => Flow::Scan,
        };
    }

    pub fn cycle_mode(&mut self, forward: bool) {
        self.mode = self.mode.step(forward);
    }

    /// Edit the text field under focus (`target`/`title`/`goal`). Rows 0 and 4 have no text,
    /// so `change` is not run and the caller knows the key was not an edit.
    pub fn edit<F: FnOnce(&mut String)>(&mut self, change: F) -> bool {
        let field = match self.row {
            1 => &mut self.target,
            2 => &mut self.title,
            3 => &mut self.goal,
            _ => return false,
        };
        change(field);
        true
    }

    pub fn append(&mut self, text: &str) -> bool {
        self.edit(|value| value.push_str(text))
    }

    pub fn backspace(&mut self) -> bool {
        self.edit(|value| {
            value.pop();
        })
    }
}

/// The CLI's own slug rule (`_slug` in `triad.py`): lowercase, runs of non-alphanumerics
/// collapse to one `-`, trim the dashes, cap at 48, and fall back so the directory is never
/// empty.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in text.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch);
        } else {
            pending_dash = true;
        }
    }
    let capped: String = out.chars().take(48).collect();
    if capped.is_empty() {
        "engagement".to_string()
    } else {
        capped
    }
}

/// `~` (or `~/...`) at the start becomes `$HOME`; anything else is left alone. The Ink rule
/// is `/^~(?=\/|$)/`, so `~foo` is not a home reference.
fn expand_home(root: &str, home: &str) -> String {
    if let Some(rest) = root.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with('/') {
            return format!("{home}{rest}");
        }
    }
    root.to_string()
}

/// `$TRIAD_WORKDIR` (or `~/engagements`) joined with the title slug, the same directory the
/// Ink `engagementWorkdir` builds.
pub fn engagement_workdir(fields: &NewEngagement) -> PathBuf {
    let home = env::var("HOME").unwrap_or_default();
    let root = env::var("TRIAD_WORKDIR").unwrap_or_else(|_| "~/engagements".to_string());
    engagement_workdir_in(&root, &home, &fields.title)
}

/// The pure half of `engagement_workdir`, so the rule is testable without touching the
/// process environment.
pub fn engagement_workdir_in(root: &str, home: &str, title: &str) -> PathBuf {
    Path::new(&expand_home(root, home)).join(slug(title))
}

/// `scanArgs`: `scan --target <t> --workdir <w> --mode <m>`.
pub fn scan_args(fields: &NewEngagement) -> Vec<String> {
    scan_args_for(fields, &engagement_workdir(fields))
}

/// `engageArgs`: `engage --title <t> --target <t> --goal <g> --workdir <w> --mode <m>`.
pub fn engage_args(fields: &NewEngagement) -> Vec<String> {
    engage_args_for(fields, &engagement_workdir(fields))
}

fn scan_args_for(fields: &NewEngagement, workdir: &Path) -> Vec<String> {
    vec![
        "scan".to_string(),
        "--target".to_string(),
        fields.target.clone(),
        "--workdir".to_string(),
        workdir.to_string_lossy().into_owned(),
        "--mode".to_string(),
        fields.mode.as_str().to_string(),
    ]
}

fn engage_args_for(fields: &NewEngagement, workdir: &Path) -> Vec<String> {
    vec![
        "engage".to_string(),
        "--title".to_string(),
        fields.title.clone(),
        "--target".to_string(),
        fields.target.clone(),
        "--goal".to_string(),
        fields.goal.clone(),
        "--workdir".to_string(),
        workdir.to_string_lossy().into_owned(),
        "--mode".to_string(),
        fields.mode.as_str().to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(flow: Flow, mode: ScanMode) -> NewEngagement {
        NewEngagement {
            flow,
            target: "http://10.0.0.1".to_string(),
            title: "Recon One".to_string(),
            goal: "map the attack surface".to_string(),
            mode,
        }
    }

    #[test]
    fn slug_matches_the_cli_rule() {
        assert_eq!(slug("My Scan!"), "my-scan");
        assert_eq!(slug("  --Hello   World--  "), "hello-world");
        assert_eq!(slug("Caf\u{e9} men\u{fc}"), "caf-men");
        assert_eq!(slug(""), "engagement");
        assert_eq!(slug("!!!?!"), "engagement");
        assert_eq!(slug(&"a".repeat(60)), "a".repeat(48));
    }

    #[test]
    fn workdir_expands_home_and_joins_slug() {
        assert_eq!(
            engagement_workdir_in("~/engagements", "/home/u", "Recon One"),
            Path::new("/home/u/engagements/recon-one")
        );
        assert_eq!(
            engagement_workdir_in("~", "/home/u", "x"),
            Path::new("/home/u/x")
        );
        assert_eq!(
            engagement_workdir_in("/abs/root/", "/home/u", "My Scan"),
            Path::new("/abs/root/my-scan")
        );
        // `~foo` is not a home reference, matching `/^~(?=\/|$)/`.
        assert_eq!(
            engagement_workdir_in("~weird", "/home/u", "x"),
            Path::new("~weird/x")
        );
    }

    #[test]
    fn scan_argv_matches_the_ink_version() {
        let argv = scan_args_for(
            &fields(Flow::Scan, ScanMode::Deep),
            Path::new("/w/recon-one"),
        );
        println!("submit argv (scan):   triad.py {}", argv.join(" "));
        assert_eq!(
            argv,
            vec![
                "scan",
                "--target",
                "http://10.0.0.1",
                "--workdir",
                "/w/recon-one",
                "--mode",
                "deep",
            ]
        );
    }

    #[test]
    fn engage_argv_matches_the_ink_version() {
        let argv = engage_args_for(
            &fields(Flow::Engage, ScanMode::Standard),
            Path::new("/w/recon-one"),
        );
        println!("submit argv (engage): triad.py {}", argv.join(" "));
        assert_eq!(
            argv,
            vec![
                "engage",
                "--title",
                "Recon One",
                "--target",
                "http://10.0.0.1",
                "--goal",
                "map the attack surface",
                "--workdir",
                "/w/recon-one",
                "--mode",
                "standard",
            ]
        );
    }

    #[test]
    fn validation_matches_the_ink_wording() {
        let mut form = FormState::blank();
        assert_eq!(
            form.validation_error().as_deref(),
            Some("missing target, title")
        );
        assert!(!form.check());
        assert_eq!(form.error.as_deref(), Some("missing target, title"));

        form.target = "http://t".to_string();
        assert_eq!(form.validation_error().as_deref(), Some("missing title"));

        form.title = "t".to_string();
        assert_eq!(form.validation_error(), None);
        assert!(form.check());

        // `goal` is only required once the flow is an engagement.
        form.flow = Flow::Engage;
        assert_eq!(form.validation_error().as_deref(), Some("missing goal"));
        form.goal = "g".to_string();
        assert_eq!(form.validation_error(), None);

        // Whitespace-only is empty, exactly like `.trim()` in the Ink app.
        form.target = "   ".to_string();
        assert_eq!(form.validation_error().as_deref(), Some("missing target"));
    }

    #[test]
    fn mode_cycles_in_both_directions() {
        let mut form = FormState::blank();
        form.cycle_mode(true);
        assert_eq!(form.mode, ScanMode::Standard);
        form.cycle_mode(true);
        assert_eq!(form.mode, ScanMode::Deep);
        form.cycle_mode(true);
        assert_eq!(form.mode, ScanMode::Quick);
        form.cycle_mode(false);
        assert_eq!(form.mode, ScanMode::Deep);
    }

    #[test]
    fn editing_only_touches_text_rows() {
        let mut form = FormState::blank();
        form.row = 0;
        assert!(!form.append("x"));
        form.row = 1;
        assert!(form.append("ab"));
        assert_eq!(form.target, "ab");
        form.row = 3;
        assert!(form.append("goal"));
        assert_eq!(form.goal, "goal");
        form.row = 4;
        assert!(!form.backspace());
        form.row = 2;
        assert!(form.backspace());
        assert_eq!(form.title, "");
    }
}
