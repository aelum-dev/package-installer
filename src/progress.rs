//! Progress tracking for a running transaction: the only place that decides
//! what the bar shows. Pure — no GTK — the same shape as the installer's
//! `install/progress/pacstrap.go` (phase-banded progress), and the reason
//! `ui/txnlog.rs` can stay thin.
//!
//! Signals parsed out of pacman's own output (captured from pacman 7.1
//! through the helper's pipe, held under `tests/fixtures/` and replayed by
//! the tests at the bottom of this file):
//!
//! - the `Packages (N) …` header — the transaction size;
//! - the phase headers — `resolving dependencies...`,
//!   `:: Retrieving packages...`, `checking package integrity...`,
//!   `:: Processing package changes...`, `:: Running post-transaction hooks...`;
//! - one `installing/upgrading/removing X...` line per package — the
//!   counted numerator for the package changes;
//! - one ` name … downloading...` line per retrieved package — printed
//!   when a fetch **starts**. libalpm runs five fetches at a time and no
//!   line ever says a single download finished, so Retrieve counts the
//!   **distinct** line subjects less the batch that can be in flight
//!   (`MAX_DL_BATCH`): a start proves a finished download only once it
//!   frees a slot;
//! - any `( i/n) …` counter that does appear — hook lines always carry
//!   one, a TTY run carries them for the packages too — and a real counter
//!   overrides the count.
//!
//! **The bar never claims a number it does not have.** Without an honest
//! numerator for the current phase the fraction creeps inside that phase's
//! band and the label says the phase, not a percentage. A `( i/n)` counter
//! seen during one phase is never reused as another phase's progress —
//! pre-transaction hook counters arrive before the package changes, and
//! counting them toward them would move the bar past work that has not
//! happened.

use std::collections::HashSet;

/// libalpm fetches up to five files at once (measured: a timestamped pipe
/// run started llvm-libs, qt6-base, mesa, spirv-tools and qt6-translations
/// in the same instant). So a count of n started downloads proves n−5
/// finished, never more — the honest numerator is that floor.
const MAX_DL_BATCH: usize = 5;

/// What was asked, which decides the bands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Install,
    Remove,
    Refresh,
    Upgrade,
    File,
    /// One AUR job: several transactions and a build under one dialog. The
    /// steps are *told* through [`crate::txn::TxnEvent::Step`], never
    /// scraped from translated text.
    Aur,
}

/// The phase the tracker currently believes it is in. The AUR variants are
/// entered only by [`Track::step`]; pacman's own headers never produce them
/// (an AUR flow's root halves run real `-S` output, and that output must
/// not drag the AUR band around).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Sync,
    Resolve,
    Retrieve,
    Check,
    Process,
    Hooks,
    Plan,
    FetchDeps,
    Build,
    InstallBuilt,
    Cleanup,
    Done,
    Unknown,
}

/// Which of the AUR steps just started, as `remote::aur_steps` announces
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    Plan,
    FetchDeps,
    Build,
    InstallBuilt,
    Cleanup,
}

/// One band: a phase and the fraction range it owns. Bands are contiguous,
/// so a phase change moves the bar to the previous band's end at worst —
/// the fraction never goes backwards.
struct Band {
    phase: Phase,
    start: f64,
    end: f64,
}

fn bands(verb: Verb) -> Vec<Band> {
    // The widths are judgement, the phases are pacman's. Retrieval and the
    // package changes dominate a real transaction; resolving and checking
    // do not. (Anchored on the captured fixtures: a real install spends
    // its wall clock in Retrieve and Process.)
    let table: &[(Phase, f64, f64)] = match verb {
        Verb::Install | Verb::File => &[
            (Phase::Resolve, 0.00, 0.08),
            (Phase::Retrieve, 0.08, 0.45),
            (Phase::Check, 0.45, 0.55),
            (Phase::Process, 0.55, 0.85),
            (Phase::Hooks, 0.85, 1.00),
        ],
        Verb::Upgrade => &[
            (Phase::Sync, 0.00, 0.05),
            (Phase::Resolve, 0.05, 0.12),
            (Phase::Retrieve, 0.12, 0.45),
            (Phase::Check, 0.45, 0.55),
            (Phase::Process, 0.55, 0.85),
            (Phase::Hooks, 0.85, 1.00),
        ],
        Verb::Refresh => &[(Phase::Sync, 0.00, 1.00)],
        Verb::Remove => &[
            (Phase::Resolve, 0.00, 0.20),
            (Phase::Process, 0.20, 0.75),
            (Phase::Hooks, 0.75, 1.00),
        ],
        Verb::Aur => &[
            (Phase::Plan, 0.00, 0.15),
            (Phase::FetchDeps, 0.15, 0.35),
            (Phase::Build, 0.35, 0.80),
            (Phase::InstallBuilt, 0.80, 0.95),
            (Phase::Cleanup, 0.95, 1.00),
        ],
    };
    table
        .iter()
        .map(|(phase, start, end)| Band {
            phase: *phase,
            start: *start,
            end: *end,
        })
        .collect()
}

/// The state of one running transaction. Feed it every line ([`Track::feed`])
/// or every AUR step ([`Track::step`]); read [`Track::phase`],
/// [`Track::fraction`], and [`Track::counts`] for a label's "i of n".
pub struct Track {
    bands: Vec<Band>,
    phase: Phase,
    /// The transaction size — the `Packages (N)` header. It belongs to the
    /// whole transaction, not one phase, so a phase change does not clear
    /// it; Retrieve and Process count against it.
    txn_total: usize,
    /// The current `( i/n)` counter, when pacman printed one. Per-phase:
    /// cleared on every phase change.
    counter: usize,
    counter_total: usize,
    /// Packages whose action line has been seen (Process's counter).
    processed: usize,
    /// Packages whose download line has been seen (Retrieve's counter).
    /// Distinct first tokens: a slow download repeats one line per package
    /// and must not count twice.
    retrieved: HashSet<String>,
    /// Lines seen inside the current phase: the creep's numerator.
    lines: usize,
    /// The package or hook named by the last counter or action line.
    name: Option<String>,
}

impl Track {
    pub fn new(verb: Verb) -> Track {
        Track {
            bands: bands(verb),
            phase: Phase::Unknown,
            txn_total: 0,
            counter: 0,
            counter_total: 0,
            processed: 0,
            retrieved: HashSet::new(),
            lines: 0,
            name: None,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The honest "done of total" for the current phase, when one exists —
    /// what a label may put numbers on. `None` means the label names the
    /// phase and shows no count.
    pub fn counts(&self) -> Option<(usize, usize)> {
        match self.phase {
            Phase::Retrieve if self.txn_total > 0 => {
                Some((self.retrieved.len().saturating_sub(MAX_DL_BATCH), self.txn_total))
            }
            Phase::Process | Phase::FetchDeps | Phase::InstallBuilt | Phase::Cleanup
                if self.txn_total > 0 =>
            {
                Some((self.processed, self.txn_total))
            }
            Phase::Hooks if self.counter_total > 0 => Some((self.counter, self.counter_total)),
            _ => None,
        }
    }

    /// What is being worked on right now, when pacman named it. Used by
    /// the tests to pin the parsing; the dialog labels the phase, not the
    /// item — the log names every package it touches.
    #[cfg(test)]
    pub fn current_name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn feed(&mut self, line: &str) {
        let line = line.trim_end();
        // A counter line is the strongest signal; read it before anything
        // else so `(1/1) installing foo` also sets the phase.
        if let Some((i, n, rest)) = counter(line) {
            self.counter = i;
            self.counter_total = n;
            let rest = strip_bar(rest.trim());
            if !rest.is_empty() {
                if self.phase_for_action(rest) {
                    // A package counter (a TTY run): it *is* the Process
                    // count, and it sizes the transaction if the header
                    // was not seen.
                    self.processed = self.processed.max(i);
                    self.txn_total = self.txn_total.max(n);
                }
                self.name = Some(action_target(rest).to_string());
            }
            return;
        }

        let stripped = strip_bar(line);
        let stripped = stripped.trim();
        if stripped.is_empty() {
            return;
        }

        // Phase headers, in pacman's own words.
        if stripped.starts_with("resolving dependencies")
            || stripped.starts_with("checking dependencies")
            || stripped.starts_with("loading packages")
            || stripped.starts_with("looking for conflicting packages")
        {
            self.set(Phase::Resolve);
            return;
        }
        if stripped == ":: Synchronizing package databases..." {
            self.set(Phase::Sync);
            return;
        }
        if stripped.starts_with(":: Retrieving packages") {
            self.set(Phase::Retrieve);
            return;
        }
        if stripped.starts_with("checking keyring")
            || stripped.starts_with("checking package integrity")
            || stripped.starts_with("loading package files")
            || stripped.starts_with("checking for file conflicts")
            || stripped.starts_with("checking available disk space")
        {
            self.set(Phase::Check);
            return;
        }
        if stripped.starts_with(":: Processing package changes") {
            self.set(Phase::Process);
            return;
        }
        if stripped.starts_with(":: Running post-transaction hooks") {
            self.set(Phase::Hooks);
            return;
        }
        // Pre-transaction hooks run *before* the package changes; jumping
        // to the Hooks band there would move the bar past the work that
        // has not happened. The current band just creeps.
        if stripped.starts_with(":: Running pre-transaction hooks") {
            self.lines += 1;
            return;
        }
        if stripped.starts_with(":: Starting full system upgrade") {
            self.set(Phase::Resolve);
            return;
        }
        if stripped.starts_with(":: Proceed with") || stripped.starts_with(":: Do you want") {
            return; // --noconfirm answers these; they are not phases.
        }
        if stripped.contains("there is nothing to do") {
            self.phase = Phase::Done;
            self.name = None;
            return;
        }

        // The transaction size.
        if let Some(n) = packages_header(stripped) {
            self.txn_total = n;
            return;
        }

        // A bare action line — the piped shape, one per package. Each one
        // is a package the transaction has taken through.
        if let Some(target) = action_of(stripped) {
            let counted = self.phase_for_action(stripped);
            if counted {
                self.processed += 1;
            }
            if !target.is_empty() {
                self.name = Some(target.to_string());
            }
            self.lines += 1;
            return;
        }

        // A download line: ` name-… downloading...`, printed when a fetch
        // starts — one line per file, no completion line, and no carriage
        // returns even on a 1.3 GB download (measured: CR=0 through the
        // pipe). The subject is the first token; a subject seen again (a
        // pacman that redraws progress) stays one entry.
        if self.phase == Phase::Retrieve
            && let Some(tok) = stripped.split_whitespace().next()
            && tok.len() > 2
            && !tok.starts_with("error:")
            && !tok.starts_with("warning:")
            && !tok.starts_with("::")
        {
            self.retrieved.insert(tok.to_string());
            self.lines += 1;
            return;
        }

        self.lines += 1;
    }

    /// The AUR flow announces its steps; the bar follows what it is told.
    pub fn step(&mut self, kind: StepKind) {
        let phase = match kind {
            StepKind::Plan => Phase::Plan,
            StepKind::FetchDeps => Phase::FetchDeps,
            StepKind::Build => Phase::Build,
            StepKind::InstallBuilt => Phase::InstallBuilt,
            StepKind::Cleanup => Phase::Cleanup,
        };
        self.set(phase);
    }

    /// Where the bar stands: the start of the current band plus the share
    /// earned by real counters, or a bounded creep when there is none.
    /// Never reaches the next band's start without its counters.
    pub fn fraction(&self) -> f64 {
        let band = match self.bands.iter().find(|b| b.phase == self.phase) {
            Some(b) => b,
            None => {
                // A phase outside this verb's table (a pacman header
                // inside an AUR job) keeps the current band; Done is 1.0.
                if self.phase == Phase::Done {
                    return 1.0;
                }
                return 0.0;
            }
        };
        let width = band.end - band.start;
        // Retrieve earns its own share: proven completions (seen less the
        // batch) plus a bounded credit for the fetches in flight — their
        // byte totals are the mirror's, not pacman's, so the credit creeps
        // and never reaches the next whole package.
        let share = if self.phase == Phase::Retrieve && self.txn_total > 0 {
            let done = self.retrieved.len().saturating_sub(MAX_DL_BATCH);
            let inflight = if self.retrieved.is_empty() {
                0.0
            } else {
                0.1 + 0.8 * (self.lines.min(40)) as f64 / 40.0
            };
            ((done as f64 + inflight) / self.txn_total as f64).min(1.0)
        } else {
            match self.counts() {
                Some((done, total)) => (done as f64 / total as f64).min(1.0),
                // No honest count exists for the rest — the network's and
                // the compiler's, not pacman's — and counted phases with no
                // size named yet creep until `Packages (N)` arrives.
                None => self.creep(),
            }
        };
        // Creep is capped so an unfinished un-counted phase never shows a
        // full band; a counted one may reach its end.
        let capped = if self.counts().is_some() {
            share
        } else {
            share.min(0.9)
        };
        band.start + width * capped
    }

    /// A bounded, deterministic creep: each unseen line inside a phase
    /// advances a little, toward the band's end but never to it.
    fn creep(&self) -> f64 {
        let steps = (self.lines.min(40)) as f64;
        0.1 + 0.8 * (steps / 40.0)
    }

    fn set(&mut self, phase: Phase) {
        // A phase outside this verb's band table is not a phase this job
        // has: inside an AUR job, pacman's own -S headers must leave the
        // announced step alone.
        if !self.bands.iter().any(|b| b.phase == phase) {
            return;
        }
        if self.phase != phase {
            self.phase = phase;
            self.lines = 0;
            self.counter = 0;
            self.counter_total = 0;
            self.retrieved.clear();
            self.name = None;
            // `processed` is *not* reset: it counts the packages the
            // transaction has taken through, and `Packages (N)` sized them
            // for the whole run. (A -Syu's Sync and Resolve contribute no
            // action lines, so it stays 0 until Process begins.)
        }
    }

    /// True when the action belongs to a package-changes band (this verb's
    /// Process, or the AUR's FetchDeps/InstallBuilt/Cleanup, where a real
    /// -S/-U runs under the announced step).
    fn phase_for_action(&mut self, action: &str) -> bool {
        let is_package_action = action.starts_with("installing")
            || action.starts_with("upgrading")
            || action.starts_with("removing");
        if !is_package_action {
            return false;
        }
        if matches!(
            self.phase,
            Phase::Process | Phase::FetchDeps | Phase::InstallBuilt | Phase::Cleanup
        ) {
            return true;
        }
        // A stray action line outside those phases (a truncated log):
        // treat it as this verb's package phase if it has one. `set`
        // ignores the phase when this verb has no Process band.
        self.set(Phase::Process);
        self.phase == Phase::Process
    }
}

/// `( i/n) what remains` — pacman's counter prefix.
fn counter(line: &str) -> Option<(usize, usize, &str)> {
    let rest = line.strip_prefix('(')?;
    let close = rest.find(')')?;
    let inner = &rest[..close];
    let (i, n) = inner.split_once('/')?;
    let i: usize = i.parse().ok()?;
    let n: usize = n.parse().ok()?;
    if n == 0 || i > n {
        return None;
    }
    Some((i, n, &rest[close + 1..]))
}

/// `Packages (N) name-ver …` — the transaction size. pacman 7.1 prints
/// `Packages (1) linux-7.2.14.arch1-1`.
fn packages_header(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("Packages (")?;
    let close = rest.find(')')?;
    rest[..close].parse().ok()
}

/// `installing x…` / `upgrading x…` / `removing x…` → the target named.
fn action_of(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("installing ")
        .or_else(|| line.strip_prefix("upgrading "))
        .or_else(|| line.strip_prefix("removing "))?;
    Some(rest.trim().trim_end_matches('.'))
}

/// The name a line is about: the target of an action line, else the line.
fn action_target(line: &str) -> &str {
    action_of(line).unwrap_or(line)
}

/// Drop pacman's progress bar and speed columns from a line:
/// `foo [###---] 100%` → `foo`, `name  2.4 MiB/s  0:02` → `name`.
/// Only the trailing columns are touched — the interesting part of every
/// line is its head.
pub fn strip_bar(line: &str) -> &str {
    // The bar form: a bracketed run preceded by whitespace.
    if let Some(idx) = line.find(" [#") {
        return line[..idx].trim_end();
    }
    // The download speed form: `name  82.1 MiB  11.4 MiB/s  0:07` — pacman
    // separates columns with runs of spaces, and the speed column is the
    // one carrying "/s". Drop it and everything after it.
    if line.contains("/s") {
        let parts: Vec<&str> = line.split("  ").collect();
        if let Some(idx) = parts.iter().position(|p| p.contains("/s"))
            && idx >= 1
        {
            // The join reproduces the prefix exactly (split consumed the
            // separators), so its length is where the cut lands in `line`.
            let cut = parts[..idx].join("  ").len();
            return line[..cut].trim_end();
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(mut t: Track, lines: &[&str]) -> Track {
        for l in lines {
            t.feed(l);
        }
        t
    }

    /// Feed a captured fixture (the helper's real output, `exit=` line
    /// included and skipped) and assert the two properties every replay
    /// must hold: the fraction never goes backwards, and it ends where the
    /// fixture's work says it ends.
    fn replay(name: &str, verb: Verb) -> Track {
        let text = match name {
            "install" => include_str!("../tests/fixtures/install.txt"),
            "remove" => include_str!("../tests/fixtures/remove.txt"),
            "file" => include_str!("../tests/fixtures/file.txt"),
            "refresh" => include_str!("../tests/fixtures/refresh.txt"),
            "upgrade" => include_str!("../tests/fixtures/upgrade.txt"),
            "kernel-install" => include_str!("../tests/fixtures/kernel-install.txt"),
            "nothing-to-do" => include_str!("../tests/fixtures/nothing-to-do.txt"),
            other => panic!("no fixture named {other}"),
        };
        let mut t = Track::new(verb);
        let mut last = 0.0;
        for line in text.lines() {
            if line.starts_with("exit=") {
                continue; // the capture's own marker, not pacman's
            }
            t.feed(line);
            assert!(
                t.fraction() >= last - f64::EPSILON,
                "monotone through {name}: {line:?}"
            );
            last = t.fraction();
        }
        t
    }

    #[test]
    fn the_captured_install_ends_counted_and_complete() {
        // The 5-package moreutils install: every package line counted.
        let t = replay("install", Verb::Install);
        assert_eq!(t.phase(), Phase::Hooks);
        assert_eq!(t.counts(), Some((3, 3)), "three post-transaction hooks");
        assert_eq!(t.fraction(), 1.0);
    }

    #[test]
    fn the_captured_remove_counts_its_orphans() {
        // -Rns of moreutils: five removals after `checking dependencies`.
        let t = replay("remove", Verb::Remove);
        assert_eq!(t.phase(), Phase::Hooks);
        assert_eq!(t.fraction(), 1.0);
    }

    #[test]
    fn the_captured_file_install_counts_its_packages() {
        // -U opens with `loading packages...` and resolves like any install.
        let t = replay("file", Verb::File);
        assert_eq!(t.phase(), Phase::Hooks);
        assert_eq!(t.fraction(), 1.0);
    }

    #[test]
    fn the_captured_upgrade_counts_the_upgrade() {
        let t = replay("upgrade", Verb::Upgrade);
        assert_eq!(t.phase(), Phase::Hooks);
        assert_eq!(t.fraction(), 1.0);
    }

    #[test]
    fn a_scriptlet_heavy_install_still_finishes_its_counters() {
        // The linux install: mkinitcpio output between hook counters must
        // not be mistaken for anything.
        let t = replay("kernel-install", Verb::Install);
        assert_eq!(t.phase(), Phase::Hooks);
        assert_eq!(t.counts(), Some((4, 4)));
        assert_eq!(t.fraction(), 1.0);
    }

    #[test]
    fn the_captured_refresh_never_claims_done() {
        // Two mirrors failing, some databases fetched, exit 0: the parser
        // has no count for the network, so the bar creeps but never fills.
        let t = replay("refresh", Verb::Refresh);
        assert_eq!(t.phase(), Phase::Sync);
        assert!(t.counts().is_none(), "no honest count for a sync");
        assert!(t.fraction() < 1.0, "a bar that fills without a count lies");
    }

    #[test]
    fn nothing_to_do_ends_the_bar() {
        let t = replay("nothing-to-do", Verb::Install);
        assert_eq!(t.phase(), Phase::Done);
        assert_eq!(t.fraction(), 1.0);
    }

    #[test]
    fn the_packages_header_sizes_the_transaction() {
        let mut t = Track::new(Verb::Install);
        for line in [
            "resolving dependencies...",
            "looking for conflicting packages...",
            "Packages (2) gnu-netcat-0.3.3-3  readline-8.3.2-1",
            ":: Retrieving packages...",
        ] {
            t.feed(line);
        }
        assert_eq!(t.phase(), Phase::Retrieve);
        assert_eq!(t.counts(), Some((0, 2)), "the size survives the phase change");
    }

    #[test]
    fn action_lines_count_against_the_size() {
        let mut t = Track::new(Verb::Install);
        t.feed("Packages (3) a-1  b-1  c-1");
        t.feed(":: Processing package changes...");
        t.feed("installing a...");
        t.feed("installing b...");
        assert_eq!(t.counts(), Some((2, 3)));
        assert_eq!(t.current_name().unwrap(), "b");
        let f = t.fraction();
        assert!(f > 0.55 && f < 0.85, "two of three inside the Process band");
    }

    #[test]
    fn a_real_counter_overrides_the_count() {
        // The TTY shape: `( i/n) installing x [###] 100%`.
        let lines = [
            "Packages (3) a-1  b-1  c-1",
            ":: Processing package changes...",
            "(1/3) installing a                                     [########] 100%",
            "(2/3) installing b                                     [########] 100%",
        ];
        let t = feed_all(Track::new(Verb::Install), &lines);
        assert_eq!(t.counts(), Some((2, 3)));
        assert_eq!(t.current_name().unwrap(), "b");
    }

    #[test]
    fn download_lines_count_distinct_subjects_less_the_batch_in_flight() {
        // Measured through the pipe: one line per fetch at its start, and
        // five fetches run at a time. Seven starts prove two finished —
        // no more, because five could still be moving.
        let mut t = Track::new(Verb::Install);
        t.feed("Packages (7) a-1  b-1  c-1  d-1  e-1  f-1  g-1");
        t.feed(":: Retrieving packages...");
        for name in ["a", "b", "c", "d", "e", "f", "g"] {
            t.feed(&format!(" {name}-1-x86_64.pkg.tar.zst downloading..."));
        }
        assert_eq!(t.counts(), Some((2, 7)), "seven started, two proven done");
        let f = t.fraction();
        assert!(f > 0.0 && f < 1.0, "inside the Retrieve band, short of its end");
    }

    #[test]
    fn a_subject_seen_many_times_stays_one_download() {
        // Defensive for a pacman that streams progress lines for one file:
        // the count must not run ahead, and the bar must still move.
        let mut t = Track::new(Verb::Install);
        t.feed("Packages (7) a-1  b-1  c-1  d-1  e-1  f-1  g-1");
        t.feed(":: Retrieving packages...");
        for _ in 0..30 {
            t.feed(" a-1-x86_64.pkg.tar.zst   13.5 MiB  13.5 KiB/s (108 KiB/s)");
        }
        assert_eq!(t.counts(), Some((0, 7)), "one subject, nothing proven done");
        let stalled = t.fraction();
        for name in ["b", "c", "d", "e", "f", "g"] {
            t.feed(&format!(" {name}-1-x86_64.pkg.tar.zst downloading..."));
        }
        assert_eq!(t.counts(), Some((2, 7)), "seven subjects prove two done");
        assert!(t.fraction() > stalled, "and the bar moved with it");
    }

    #[test]
    fn a_pre_transaction_hook_counter_does_not_count_toward_the_packages() {
        // The captured shape: `(1/1) 90-aelum-snapper-pre.hook` runs
        // before Processing. It must not make the next phase report 1 of 1
        // packages done.
        let lines = [
            "Packages (2) a-1  b-1",
            ":: Running pre-transaction hooks...",
            "(1/1) 90-aelum-snapper-pre.hook",
            ":: Processing package changes...",
        ];
        let t = feed_all(Track::new(Verb::Install), &lines);
        assert_eq!(t.phase(), Phase::Process);
        assert_eq!(t.counts(), Some((0, 2)), "no package has been taken through yet");
        assert!(t.fraction() <= 0.55 + f64::EPSILON, "at the band floor, not past it");
    }

    #[test]
    fn without_a_size_the_fraction_creeps_inside_the_band() {
        let mut t = Track::new(Verb::Install);
        t.feed(":: Retrieving packages...");
        let start = t.fraction();
        for _ in 0..30 {
            t.feed(" something downloading...");
        }
        assert!(t.fraction() > start, "the bar creeps while work is happening");
        assert!(t.fraction() < 0.45, "but never leaves its band");
    }

    #[test]
    fn the_aur_bar_follows_being_told_not_being_read() {
        let mut t = Track::new(Verb::Aur);
        t.step(StepKind::Plan);
        let a = t.fraction();
        t.feed("makepkg output that means nothing to a parser");
        let b = t.fraction();
        t.step(StepKind::FetchDeps);
        let c = t.fraction();
        t.step(StepKind::Build);
        let d = t.fraction();
        t.step(StepKind::InstallBuilt);
        let e = t.fraction();
        t.step(StepKind::Cleanup);
        let f = t.fraction();
        assert!(a < b && b < c && c < d && d < e && e < f, "steps move it forward");
        assert!(f < 1.0, "cleanup is not done until it says so");
    }

    #[test]
    fn the_aur_steps_survive_pacman_noise() {
        // The root halves of an AUR install are real -S runs: their phase
        // headers must not drag an AUR band around.
        let mut t = Track::new(Verb::Aur);
        t.step(StepKind::Build);
        t.feed(":: Processing package changes...");
        t.feed("(1/4) Arming ConditionNeedsUpdate...");
        assert_eq!(t.phase(), Phase::Build, "a pacman header does not own the AUR bar");
    }

    #[test]
    fn bars_and_speed_columns_are_stripped() {
        assert_eq!(
            strip_bar("(2/5) upgrading glibc                           [#####-]  83%"),
            "(2/5) upgrading glibc"
        );
        assert_eq!(
            strip_bar(" firefox-159.0-1-x86_64.pkg.tar.zst   82.1 MiB  11.4 MiB/s  0:07"),
            " firefox-159.0-1-x86_64.pkg.tar.zst   82.1 MiB"
        );
        assert_eq!(strip_bar("no bars here at all"), "no bars here at all");
    }

    #[test]
    fn a_counter_line_alone_names_the_process_phase() {
        // The counter for `installing` arrives with the phase already set
        // in real output, but a truncated log must still classify.
        let mut t = Track::new(Verb::Install);
        t.feed("(1/1) installing x");
        assert_eq!(t.phase(), Phase::Process);
        assert_eq!(t.counts(), Some((1, 1)));
    }
}
