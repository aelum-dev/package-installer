//! Package transactions: the one place this app asks for root.
//!
//! Every privileged action runs through the package's own helper under
//! pkexec — the polkit action's `exec.path` annotation authenticates the
//! *program*, exactly like `aelum-snapshot-restore`. The helper is the only
//! place pacman is invoked with privileges, so one action + one rule cover
//! install, removal, refresh, upgrade, and local files.
//!
//! `--noconfirm` everywhere: this dialog owns the confirmation, and a
//! conflict must exit non-zero with pacman's real output rather than wait
//! for a TTY that does not exist.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;

use futures::channel::mpsc::UnboundedSender;

use crate::i18n;

/// The helper shipped by the package; pkexec resolves it to the
/// `dev.aelum.package.manage` action through the policy's exec.path.
pub const HELPER: &str = "/usr/bin/aelum-package-installer-helper";

#[derive(Clone, Debug)]
pub enum Txn {
    Refresh,
    Install(Vec<String>),
    Remove(Vec<String>),
    Upgrade,
    /// Local package files — an opened `.pkg.tar.zst`, or everything one AUR
    /// build produced. Several files are one transaction: pacman resolves the
    /// order between them, which is what a split PKGBUILD needs.
    InstallFiles(Vec<PathBuf>),
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
            Txn::Remove(pkgs) => {
                let mut a = vec!["remove".into()];
                a.extend(pkgs.iter().cloned());
                a
            }
            Txn::Upgrade => vec!["upgrade".into()],
            Txn::InstallFiles(paths) => {
                let mut a = vec!["file".into()];
                a.extend(paths.iter().map(|p| p.display().to_string()));
                a
            }
        }
    }

    /// One line shown in the log while it runs.
    pub fn summary(&self) -> String {
        match self {
            Txn::Refresh => i18n::t("txn.summaryRefresh"),
            Txn::Install(pkgs) => {
                i18n::t_fmt("txn.summaryInstall", &[("names", &pkgs.join(", "))])
            }
            Txn::Remove(pkgs) => i18n::t_fmt("txn.summaryRemove", &[("names", &pkgs.join(", "))]),
            Txn::Upgrade => i18n::t("txn.summaryUpgrade"),
            Txn::InstallFiles(paths) => i18n::t_fmt(
                "txn.summaryFile",
                &[(
                    "file",
                    &paths
                        .iter()
                        .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(", "),
                )],
            ),
        }
    }
}

/// Progress of a running transaction, streamed to the UI.
#[derive(Clone, Debug)]
pub enum TxnEvent {
    Line(String),
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
    thread::spawn(move || {
        let status = child.wait().map(|s| s.code().unwrap_or(-1));
        let _ = tx.unbounded_send(match status {
            Ok(0) => TxnEvent::Finished(Ok(())),
            Ok(code) => TxnEvent::Finished(Err(code)),
            Err(_) => TxnEvent::Finished(Err(-1)),
        });
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
    Command::new("pkexec")
        .arg(HELPER)
        .args(txn.argv())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
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
        for line in std::io::BufReader::new(reader).lines().map_while(Result::ok) {
            let _ = tx.unbounded_send(TxnEvent::Line(line));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_matches_the_helper_subcommands() {
        assert_eq!(Txn::Refresh.argv(), ["refresh"]);
        assert_eq!(Txn::Install(vec!["htop".into()]).argv(), ["install", "htop"]);
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
    }

    #[test]
    fn the_helper_is_annotated_by_the_policy() {
        // The pattern that makes pkexec resolve `pkexec <helper>` to the
        // dev.aelum.package.manage action — the aelum-snapshot-restore
        // shape: the policy names the exact program, and it is the same
        // path this file execs.
        let policy = include_str!("../data/polkit/dev.aelum.package.manage.policy");
        assert!(policy.contains(&format!("exec.path\">{HELPER}<")));
        let helper = include_str!("../data/helper/aelum-package-installer-helper");
        for verb in ["refresh", "install", "remove", "upgrade", "file"] {
            assert!(helper.contains(&format!("{verb})")), "helper lacks {verb}");
            assert!(helper.contains("--noconfirm"));
        }
    }
}
