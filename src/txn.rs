//! Package transactions: the one place this app asks for root.
//!
//! Every privileged action runs through the package's own helper under
//! pkexec — the polkit action's `exec.path` annotation authenticates the
//! *program*, exactly like `aelum-snapshot-restore`. The helper is the only
//! place pacman is invoked with privileges, so one action + one rule cover
//! install, removal, refresh, upgrade, and local files.
//!
//! The helper is the crate's second binary (`src/bin/`), not a script:
//! pkexec hands a root process the caller's environment, and that is an
//! interpreter's feast (BASH_ENV, imported functions, a caller-chosen PATH,
//! LD_*). The tests below pin that shape across the file boundary — an
//! invariant, like every other test reading the helper's source.
//!
//! `--noconfirm` everywhere: this dialog owns the confirmation, and a
//! conflict must exit non-zero with pacman's real output rather than wait
//! for a TTY that does not exist.

use std::io::BufRead;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use futures::channel::mpsc::UnboundedSender;

use crate::i18n;
use crate::progress;

/// The helper shipped by the package; pkexec resolves it to the
/// `dev.aelum.package.manage` action through the policy's exec.path.
pub const HELPER: &str = "/usr/bin/aelum-package-installer-helper";

/// How long a database refresh may run before the app stops waiting for it.
/// The refresh is the one transaction the app starts by itself, it is pure
/// network, and while it runs the gate holds every other act — including
/// Update mirrors, which is precisely what a dead mirror needs. An
/// unreachable mirror must not be able to hold the app open forever.
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(60);

/// …and how long a stopped run gets to notice SIGTERM — pacman removes its
/// own lock on a clean exit — before the kill becomes real.
const KILL_GRACE: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub enum Txn {
    Refresh,
    Install(Vec<String>),
    /// The AUR plan's repository half — installed **as dependencies**, the
    /// marking makepkg's `-s` uses (`run_pacman -S --asdeps`). A name
    /// installed by name is Explicit: it survives every `-Rns` and the next
    /// plan reads it as already satisfied, so a build dep left behind by a
    /// failed cleanup would be a permanent ghost.
    InstallAsDeps(Vec<String>),
    Remove(Vec<String>),
    Upgrade,
    /// Local package files — an opened `.pkg.tar.zst`, or the requested
    /// package's own files. Several files are one transaction: pacman resolves the
    /// order between them, which is what a split PKGBUILD needs.
    InstallFiles(Vec<PathBuf>),
    /// The built *dependencies'* archives — the same marking as
    /// `InstallAsDeps`. The requested package stays Explicit; a dependency
    /// that outlives the cleanup should be an orphan, not a ghost.
    InstallFilesAsDeps(Vec<PathBuf>),
    /// Rewrite the mirror list through the helper's reflector verb. `None` is
    /// the installer's worldwide location — every mirror, no country filter.
    /// This runs under the mirrors dialog's own spinner, never in a txn log:
    /// a mirror rewrite has no pacman progress to parse.
    UpdateMirrors(Option<String>),
}

impl Txn {
    /// The helper's argv (without the `pkexec` itself) — separate from the
    /// spawn so it can be tested without root.
    pub fn argv(&self) -> Vec<String> {
        match self {
            Txn::Refresh => vec!["refresh".into()],
            Txn::Install(pkgs) => {
                let mut a = vec!["install".into()];
                a.extend(pkgs.iter().cloned());
                a
            }
            Txn::InstallAsDeps(pkgs) => {
                let mut a = vec!["install-asdeps".into()];
                a.extend(pkgs.iter().cloned());
                a
            }
            Txn::Remove(pkgs) => {
                let mut a = vec!["remove".into()];
                a.extend(pkgs.iter().cloned());
                a
            }
            Txn::Upgrade => vec!["upgrade".into()],
            Txn::InstallFiles(paths) | Txn::InstallFilesAsDeps(paths) => {
                let verb = match self {
                    Txn::InstallFilesAsDeps(_) => "file-asdeps",
                    _ => "file",
                };
                let mut a = vec![verb.into()];
                a.extend(paths.iter().map(|p| p.display().to_string()));
                a
            }
            Txn::UpdateMirrors(country) => {
                let mut a = vec!["update-mirrors".into()];
                a.extend(country.iter().cloned());
                a
            }
        }
    }

    /// One line shown in the log while it runs.
    pub fn summary(&self) -> String {
        match self {
            Txn::Refresh => i18n::t("txn.summaryRefresh"),
            Txn::Install(pkgs) | Txn::InstallAsDeps(pkgs) => {
                i18n::t_fmt("txn.summaryInstall", &[("names", &pkgs.join(", "))])
            }
            Txn::Remove(pkgs) => i18n::t_fmt("txn.summaryRemove", &[("names", &pkgs.join(", "))]),
            Txn::Upgrade => i18n::t("txn.summaryUpgrade"),
            Txn::InstallFiles(paths) | Txn::InstallFilesAsDeps(paths) => i18n::t_fmt(
                "txn.summaryFile",
                &[(
                    "file",
                    &paths
                        .iter()
                        .map(|p| {
                            p.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned()
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                )],
            ),
            // The mirrors dialog is open over the job this names; no sentence
            // about it is ever printed next to it.
            Txn::UpdateMirrors(_) => i18n::t("mirrors.title"),
        }
    }

    /// Which progress bands this request runs through (`progress::Track`).
    /// The AUR job is not a `Txn` — its steps are announced, not parsed —
    /// and builds its track as `Verb::Aur` directly.
    pub fn verb(&self) -> crate::progress::Verb {
        match self {
            Txn::Refresh => crate::progress::Verb::Refresh,
            Txn::Install(_) | Txn::InstallAsDeps(_) => crate::progress::Verb::Install,
            Txn::Remove(_) => crate::progress::Verb::Remove,
            Txn::Upgrade => crate::progress::Verb::Upgrade,
            Txn::InstallFiles(_) | Txn::InstallFilesAsDeps(_) => crate::progress::Verb::File,
            // Not a pacman transaction, and the mirrors dialog streams its job
            // into a spinner, never a progress bar. Asking for a band is a
            // misuse of this variant — and a misuse that names its own exit.
            Txn::UpdateMirrors(_) => unreachable!(
                "update-mirrors runs under the mirrors dialog's spinner, never a progress bar"
            ),
        }
    }

    /// Whether this verb fetches the sync databases — the helper runs `-Sy`
    /// for `refresh` and `-Syu` for `upgrade`. Only those two leave the
    /// databases current, so only their success may mark a check as done;
    /// every other verb reads the databases and proves nothing about their age.
    pub fn syncs_databases(&self) -> bool {
        matches!(self, Txn::Refresh | Txn::Upgrade)
    }
}

/// Progress of a running transaction, streamed to the UI.
#[derive(Clone, Debug)]
pub enum TxnEvent {
    Line(String),
    /// A named step of a multi-step job (the AUR flow) began. The progress
    /// bar follows these for that flow — driving it by scraping translated
    /// log text is exactly what breaks in another language.
    Step(progress::StepKind),
    /// Ok(0) on success; Err(code) for anything else (pkexec's 127 means
    /// the action was refused, 1 means the user dismissed the agent).
    Finished(Result<(), i32>),
}

/// Start the transaction. Output lines and the exit status arrive on `tx`;
/// the child is reaped by the reader threads.
///
/// The helper path is not optional and there is no raw-`pacman` fallback:
/// every privileged operation must resolve to the one polkit action,
/// exactly like `aelum-snapshot-restore`. A host without the packaged
/// helper cannot authorize any transaction, so the caller checks the path
/// first and says so plainly.
pub fn run(txn: &Txn, tx: UnboundedSender<TxnEvent>) -> std::io::Result<()> {
    let mut child = spawn(txn)?;
    read_pipes(&mut child, &tx);
    // The caller is not held: the exit status arrives as an event.
    let deadline = timeout_for(txn);
    let tx_stop = tx.clone();
    thread::spawn(move || {
        let status = match deadline {
            Some(dur) => wait_or_stop(&mut child, dur, &tx_stop),
            None => child.wait().map(|s| s.code().unwrap_or(-1)),
        };
        let _ = tx.unbounded_send(match status {
            Ok(0) => TxnEvent::Finished(Ok(())),
            Ok(code) => TxnEvent::Finished(Err(code)),
            Err(_) => TxnEvent::Finished(Err(-1)),
        });
    });
    Ok(())
}

/// Which transactions run under a deadline, and what it is. Only the
/// refresh: stopping it costs a re-download, while stopping an install or
/// an upgrade mid-transaction costs a half-configured system. The clock
/// starts at the spawn, and on this desktop the wheel-session grant answers
/// authorization without asking — every second of the deadline is a second
/// given to the mirrors.
fn timeout_for(txn: &Txn) -> Option<Duration> {
    match txn {
        Txn::Refresh => Some(REFRESH_TIMEOUT),
        _ => None,
    }
}

/// The wait, bounded. At the deadline the run's whole process group is
/// stopped and one translated line names what happened — it lands in the
/// buffer the quiet refresh replays into the log dialog a failure opens, so
/// the timeout explains itself rather than reading as a hang that finally
/// gave up. What the kill leaves behind (`db.lck`, a `*.db.part`) is
/// exactly what the helper's stale-lock clear and alpm's partial-download
/// naming already recover from.
fn wait_or_stop(
    child: &mut std::process::Child,
    deadline: Duration,
    tx: &UnboundedSender<TxnEvent>,
) -> std::io::Result<i32> {
    if let Some(status) = poll_wait(child, deadline)? {
        return Ok(status.code().unwrap_or(-1));
    }
    let _ = tx.unbounded_send(TxnEvent::Line(i18n::t_fmt(
        "txn.timedOut",
        &[("seconds", &deadline.as_secs().to_string())],
    )));
    stop_group(child, libc::SIGTERM);
    if let Some(status) = poll_wait(child, KILL_GRACE)? {
        return Ok(status.code().unwrap_or(-1));
    }
    stop_group(child, libc::SIGKILL);
    // A group leader killed with SIGKILL has nowhere left to wait.
    Ok(child.wait()?.code().unwrap_or(-1))
}

/// `Child::try_wait` with a bound. std's own `wait_timeout` cannot be named
/// without an unstable feature gate (its trait now carries the pending
/// signal-send additions); this deadline is 60 seconds of wall clock, and
/// 50 ms of polling resolves far finer than that means.
fn poll_wait(
    child: &mut std::process::Child,
    for_long: Duration,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let left = for_long.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Ok(None);
        }
        thread::sleep(left.min(Duration::from_millis(50)));
    }
}

/// One signal, addressed to the child's whole process group. That is the
/// point: `spawn` gives the child a group of its own, so the negative pid
/// reaches pkexec, the root helper and pacman alike — a signal addressed to
/// pkexec alone is not obliged to travel any further, and a root pacman
/// left running would hold `db.lck` past the very deadline it was stopped
/// for, making every later transaction read as the stuck one.
fn stop_group(child: &std::process::Child, signal: i32) {
    // The child is ours and not yet reaped, so its pid — the group id —
    // cannot name anybody else's group, and a group that has already
    // emptied only answers ESRCH, which is the outcome being wished for.
    unsafe { libc::kill(-(child.id() as libc::pid_t), signal) };
}

/// Several transactions as one job: every step runs in order on a worker
/// (stopping at the first failure), all output streams into the same
/// channel, and the exit status of the job is the failing step's. This is
/// what lets a chooser-answered request install its providers `--asdeps`
/// before the names the user asked for, in one log dialog.
pub fn run_all(steps: &[Txn], tx: UnboundedSender<TxnEvent>) -> std::io::Result<()> {
    if let [only] = steps {
        return run(only, tx);
    }
    let steps = steps.to_vec();
    thread::spawn(move || {
        let mut outcome = Ok(());
        for step in &steps {
            if let Err(code) = run_wait(step, &tx) {
                outcome = Err(code);
                break;
            }
        }
        let _ = tx.unbounded_send(TxnEvent::Finished(outcome));
    });
    Ok(())
}

/// The same transaction, run to completion on the calling thread, with its
/// output on `tx` and its exit code as the return value.
///
/// This is what lets one user request be several steps: an AUR install brings
/// its repository dependencies in through here before it builds, and all of
/// it streams into the one log dialog the request already opened.
pub fn run_wait(txn: &Txn, tx: &UnboundedSender<TxnEvent>) -> Result<(), i32> {
    let mut child = match spawn(txn) {
        Ok(child) => child,
        Err(e) => {
            let _ = tx.unbounded_send(TxnEvent::Line(e.to_string()));
            return Err(-1);
        }
    };
    read_pipes(&mut child, tx);
    match child.wait() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(status.code().unwrap_or(-1)),
        Err(_) => Err(-1),
    }
}

/// The helper, under pkexec, with both pipes piped and not yet read.
fn spawn(txn: &Txn) -> std::io::Result<std::process::Child> {
    let mut cmd = Command::new("pkexec");
    cmd.arg(HELPER)
        .args(txn.argv())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, so `stop_group` has a group to stop: the child
    // leads it, and everything pkexec runs after it is spawned inherits.
    cmd.process_group(0);
    cmd.spawn()
}

/// Both pipes get their own reader: leaving one unread can deadlock a long
/// pacman run on a full pipe buffer.
fn read_pipes(child: &mut std::process::Child, tx: &UnboundedSender<TxnEvent>) {
    if let Some(out) = child.stdout.take() {
        pipe(out, tx);
    }
    if let Some(err) = child.stderr.take() {
        pipe(err, tx);
    }
}

fn pipe<R: std::io::Read + Send + 'static>(reader: R, tx: &UnboundedSender<TxnEvent>) {
    let tx = tx.clone();
    thread::spawn(move || {
        for line in std::io::BufReader::new(reader)
            .lines()
            .map_while(Result::ok)
        {
            let _ = tx.unbounded_send(TxnEvent::Line(line));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The helper's whole interface, in its own order. The tests that read
    /// the helper script ask about exactly these, so a verb added to the
    /// helper cannot be missed by one of them. Only the pacman verbs go
    /// through its `pacman_args` match; the mirror update is dispatched
    /// beside it.
    const PACMAN_VERBS: [&str; 7] = [
        "refresh",
        "install",
        "install-asdeps",
        "remove",
        "upgrade",
        "file",
        "file-asdeps",
    ];
    const ALL_VERBS: [&str; 8] = [
        "refresh",
        "install",
        "install-asdeps",
        "remove",
        "upgrade",
        "file",
        "file-asdeps",
        "update-mirrors",
    ];

    #[test]
    fn argv_matches_the_helper_subcommands() {
        assert_eq!(Txn::Refresh.argv(), ["refresh"]);
        assert_eq!(
            Txn::Install(vec!["htop".into()]).argv(),
            ["install", "htop"]
        );
        assert_eq!(
            Txn::InstallAsDeps(vec!["cmake".into()]).argv(),
            ["install-asdeps", "cmake"]
        );
        assert_eq!(
            Txn::Remove(vec!["a".into(), "b".into()]).argv(),
            ["remove", "a", "b"]
        );
        assert_eq!(Txn::Upgrade.argv(), ["upgrade"]);
        assert_eq!(
            Txn::InstallFiles(vec!["/home/u/x.pkg.tar.zst".into()]).argv(),
            ["file", "/home/u/x.pkg.tar.zst"]
        );
        // A split PKGBUILD: several files, one pacman call, in build order.
        assert_eq!(
            Txn::InstallFiles(vec!["/t/a.pkg.tar.zst".into(), "/t/b.pkg.tar.zst".into()]).argv(),
            ["file", "/t/a.pkg.tar.zst", "/t/b.pkg.tar.zst"]
        );
        // Built dependencies' archives carry makepkg's marking.
        assert_eq!(
            Txn::InstallFilesAsDeps(vec!["/t/helper.pkg.tar.zst".into()]).argv(),
            ["file-asdeps", "/t/helper.pkg.tar.zst"]
        );
        // The mirror update names its country by ISO code; no country is the
        // installer's worldwide location, and the verb then takes no argument.
        assert_eq!(
            Txn::UpdateMirrors(Some("ua".into())).argv(),
            ["update-mirrors", "ua"]
        );
        assert_eq!(Txn::UpdateMirrors(None).argv(), ["update-mirrors"]);
    }

    #[test]
    fn only_the_verbs_that_fetch_databases_mark_them_current() {
        // The freshness record the next launch reads is only honest when a
        // call that really re-fetched the databases wrote it: the helper's
        // `-Sy` and `-Syu`. Every other verb reads those databases, so its
        // success says nothing about how old they are.
        let helper = include_str!("bin/aelum-package-installer-helper.rs");
        assert!(
            helper.contains("\"-Sy\""),
            "refresh must fetch the databases"
        );
        assert!(
            helper.contains("\"-Syu\""),
            "upgrade must fetch the databases"
        );
        for txn in [Txn::Refresh, Txn::Upgrade] {
            assert!(txn.syncs_databases(), "{txn:?} fetches the databases");
        }
        for txn in [
            Txn::Install(vec!["htop".into()]),
            Txn::InstallAsDeps(vec!["cmake".into()]),
            Txn::Remove(vec!["htop".into()]),
            Txn::InstallFiles(vec!["/t/x.pkg.tar.zst".into()]),
            Txn::InstallFilesAsDeps(vec!["/t/x.pkg.tar.zst".into()]),
            // A mirror update rewrites what the databases are fetched through
            // — it fetches no database, so it leaves the freshness record as
            // untouched as a removal does.
            Txn::UpdateMirrors(Some("ua".into())),
            Txn::UpdateMirrors(None),
        ] {
            assert!(!txn.syncs_databases(), "{txn:?} only reads the databases");
        }
    }

    #[test]
    fn only_the_refresh_runs_under_a_deadline() {
        // Stopping a refresh costs a re-download; stopping an install or an
        // upgrade mid-transaction costs a half-configured system. The
        // bounded list is exactly the list where a kill is safe.
        assert_eq!(timeout_for(&Txn::Refresh), Some(REFRESH_TIMEOUT));
        assert_eq!(REFRESH_TIMEOUT, Duration::from_secs(60));
        for txn in [
            Txn::Install(vec!["htop".into()]),
            Txn::InstallAsDeps(vec!["cmake".into()]),
            Txn::Remove(vec!["htop".into()]),
            Txn::Upgrade,
            Txn::InstallFiles(vec!["/t/x.pkg.tar.zst".into()]),
            Txn::InstallFilesAsDeps(vec!["/t/x.pkg.tar.zst".into()]),
            Txn::UpdateMirrors(Some("ua".into())),
            Txn::UpdateMirrors(None),
        ] {
            assert!(
                timeout_for(&txn).is_none(),
                "{txn:?} must never be stopped mid-run"
            );
        }
    }

    #[test]
    fn a_deadline_stops_a_run_that_never_answers() {
        // The shape of a dead mirror: a child that answers nothing. It gets
        // a process group of its own exactly like `spawn` does — otherwise
        // the negative-pid kill would sweep the test harness with it.
        use futures::StreamExt;
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let deadline = Duration::from_millis(200);
        let code = wait_or_stop(&mut child, deadline, &tx).unwrap();
        assert_ne!(code, 0, "a stopped run is not a success");
        // And it says what happened, before the status: that line is what
        // the user reads in the log dialog the failure opens.
        let line = futures::executor::block_on(rx.next());
        match line {
            Some(TxnEvent::Line(text)) => assert_eq!(
                text,
                i18n::t_fmt("txn.timedOut", &[("seconds", &deadline.as_secs().to_string())]),
            ),
            other => panic!("the timeout must speak before it reports, got {other:?}"),
        }
    }

    #[test]
    fn a_run_that_answers_in_time_is_left_alone() {
        let mut child = Command::new("true").spawn().unwrap();
        let (tx, _rx) = futures::channel::mpsc::unbounded();
        let code = wait_or_stop(&mut child, Duration::from_secs(60), &tx).unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn the_helper_is_annotated_by_the_policy() {
        // The pattern that makes pkexec resolve `pkexec <helper>` to the
        // dev.aelum.package.manage action — the aelum-snapshot-restore
        // shape: the policy names the exact program, and it is the same
        // path this file execs.
        let policy = include_str!("../data/polkit/dev.aelum.package.manage.policy");
        assert!(policy.contains(&format!("exec.path\">{HELPER}<")));
        let helper = include_str!("bin/aelum-package-installer-helper.rs");
        for verb in PACMAN_VERBS {
            assert!(
                helper.contains(&format!("\"{verb}\" =>")),
                "helper lacks {verb}"
            );
            assert!(helper.contains("\"--noconfirm\""));
        }
        for verb in ALL_VERBS {
            assert!(
                helper.contains(&format!("\"{verb}\"")),
                "helper lacks {verb}"
            );
        }
        // Line-buffered: with stdout a pipe glibc holds pacman's output in
        // 4 KiB blocks, and the app would watch a frozen log through a
        // whole download (measured: 53 s of silence for ~200 MiB).
        assert!(helper.contains("\"-oL\""), "helper must line-buffer");
        // The shape that replaced the shell script: both programs by
        // absolute path — a bare name under pkexec resolves through the
        // caller's PATH — and an environment rebuilt from the allowlist.
        assert!(helper.contains("\"/usr/bin/stdbuf\""), "stdbuf by path");
        assert!(helper.contains("\"/usr/bin/pacman\""), "pacman by path");
        assert!(
            helper.contains(".env_clear()"),
            "pkexec passes the caller's environment; pacman must not inherit it"
        );
    }

    #[test]
    fn every_verb_clears_a_stale_db_lock_but_never_a_live_one() {
        let helper = include_str!("bin/aelum-package-installer-helper.rs");
        // The lock lives in the DB root this app reads its databases from:
        // one path, not a second copy of it.
        assert!(
            helper.contains(&format!("\"{}/db.lck\"", crate::model::PACMAN_ROOT)),
            "the lock must be the one in the DB root the model reads"
        );
        // One clear, reached only for a verb the helper knows and before any
        // pacman runs. alpm never drops a leftover db.lck itself — measured
        // on pacman 7.1: an empty O_EXCL file, no PID written, and no
        // dead-lock check anywhere — so the lock a killed transaction leaves
        // behind is every later transaction's "unable to lock database",
        // whichever verb it asked for.
        let refused = helper.find("usage() }").unwrap();
        let cleared = helper.find("clear_stale_lock();").unwrap();
        let execed = helper.find(".exec()").unwrap();
        assert!(refused < cleared, "a refused verb must not touch the lock");
        assert!(
            cleared < execed,
            "the lock must be cleared before pacman runs"
        );
        // A live pacman is a transaction in progress: dropping its lock would
        // let two of them write the same databases.
        assert!(helper.contains("pacman_running"), "must keep a live lock");
        // And the one verb that runs no pacman is dispatched before any of
        // this exists: reflector neither takes the database lock nor needs a
        // leftover one cleared — it fetches a JSON file and writes the mirror
        // list, it never opens a pacman database.
        let mirrored = helper.find("verb == \"update-mirrors\"").unwrap();
        assert!(
            mirrored < cleared,
            "the mirror update must not touch the database lock"
        );
    }

    #[test]
    fn the_mirror_update_replaces_the_mirrorlist_atomically() {
        let helper = include_str!("bin/aelum-package-installer-helper.rs");
        // The same path the app reads its country default from: one file,
        // named once per binary, pinned equal here across the crate boundary.
        assert!(
            helper.contains(&format!(
                "const MIRRORLIST: &str = \"{}\"",
                crate::model::MIRRORLIST
            )),
            "the mirrorlist must be the one path this app shares"
        );
        // reflector by absolute path and an environment rebuilt from the
        // allowlist — the verb added to this helper holds the same rule as
        // the seven pacman verbs it was built for.
        assert!(
            helper.contains("\"/usr/bin/reflector\""),
            "reflector by path"
        );
        assert!(helper.contains(".env_clear()"));
        // The new list lands by rename, never by a truncating write onto the
        // live file: a run killed mid-write leaves the old list whole.
        assert!(
            helper.contains("std::fs::rename(MIRRORLIST_TMP, MIRRORLIST)"),
            "the mirrorlist must be replaced atomically"
        );
        // And the country reaches reflector's argv only after a shape check,
        // so nothing on argv can read as a reflector option.
        assert!(
            helper.contains("is_country_code(one)"),
            "the country argument must be validated"
        );
    }
}
