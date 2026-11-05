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

/// The helper shipped by the package; pkexec resolves it to the
/// `dev.aelum.package.manage` action through the policy's exec.path.
pub const HELPER: &str = "/usr/bin/aelum-package-installer-helper";

#[derive(Clone, Debug)]
pub enum Txn {
    Refresh,
    Install(Vec<String>),
    Remove(Vec<String>),
    Upgrade,
    InstallFile(PathBuf),
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
            Txn::InstallFile(path) => vec!["file".into(), path.display().to_string()],
        }
    }

    /// One line shown in the log while it runs.
    pub fn summary(&self) -> String {
        match self {
            Txn::Refresh => "Refreshing package databases…".into(),
            Txn::Install(pkgs) => format!("Installing {}…", pkgs.join(", ")),
            Txn::Remove(pkgs) => format!("Removing {}…", pkgs.join(", ")),
            Txn::Upgrade => "Updating all packages…".into(),
            Txn::InstallFile(path) => {
                format!("Installing {}…", path.file_name().unwrap_or_default().to_string_lossy())
            }
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
pub fn run(txn: &Txn, tx: UnboundedSender<TxnEvent>) -> std::io::Result<()> {
    let mut child = Command::new("pkexec")
        .arg(HELPER)
        .args(txn.argv())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Both pipes get their own reader: leaving one unread can deadlock a
    // long pacman run on a full pipe buffer.
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.unbounded_send(TxnEvent::Line(line));
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        thread::spawn(move || {
            for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
                let _ = tx.unbounded_send(TxnEvent::Line(line));
            }
        });
    }
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
            Txn::InstallFile("/home/u/x.pkg.tar.zst".into()).argv(),
            ["file", "/home/u/x.pkg.tar.zst"]
        );
    }
}
