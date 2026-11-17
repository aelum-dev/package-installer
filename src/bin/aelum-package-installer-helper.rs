//! aelum-package-installer-helper — the root half of every package-manager
//! operation. pkexec authenticates this program (the polkit action names it
//! through its exec.path annotation); it is the only place pacman runs with
//! privileges, so one action and one rule cover every verb.
//!
//! This is the crate's second binary, not a script, and that is the point:
//! pkexec hands the program it runs as root the *caller's* environment, and
//! a shell script inside that environment is run by an interpreter that
//! obeys `BASH_ENV` and imported shell functions, resolves every bare
//! command — its own `#!/usr/bin/env` line first of all — through the
//! caller's `PATH`, and passes `LD_*` on to what it execs. One
//! attacker-chosen variable there is code running as root through a
//! legitimately authorized action. So this program names no interpreter,
//! builds pacman's argv itself, execve's both programs by absolute path,
//! and gives them an environment that is empty apart from a fixed `PATH`
//! and the locale variables. It is the rule rEFInd's compiled pkexec helper
//! follows: never exec a bare name as root.
//!
//! `--noconfirm` everywhere: the app's dialog owns the confirmation, and a
//! conflict must exit non-zero with pacman's real output rather than wait
//! for a TTY that does not exist.
//! No `--noprogressbar`: measured against pacman 7.1, pacman draws bars and
//! transaction counters only on a TTY — through this app's pipe it prints
//! plain lines either way — and the flag would strip the ( i/n) counters a
//! TTY run does carry. progress.rs parses both shapes.
//! `stdbuf -oL`: with stdout a pipe, glibc buffers pacman's output in 4 KiB
//! blocks, and a download's lines stay in that buffer until it ends — the
//! app watched "Retrieving packages..." for a minute with 700 MiB moving.
//! Line-buffered, every line reaches the app as pacman prints it.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

/// The database lock, under the same pacman root the model reads its
/// databases from — one path, never a second copy of it.
const DB_LOCK: &str = "/var/lib/pacman/db.lck";

/// The only caller variables pacman is handed: they choose its language,
/// nothing else. Everything else is dropped by `env_clear` — `LD_*`,
/// `IFS`, shell variables, `TMPDIR` (a package's scriptlets mktemp through
/// it) — and `PATH` is re-added fixed from `exec_pacman`.
const ENV_KEEP: [&str; 5] = ["LANG", "LC_ALL", "LC_CTYPE", "LC_MESSAGES", "LANGUAGE"];

fn main() {
    let mut argv = std::env::args_os().skip(1);
    let Some(verb) = argv.next() else { usage() };
    let rest: Vec<OsString> = argv.collect();
    // The verbs are named here before anything is touched, so an unknown one
    // is refused instead of clearing a lock on a call that runs no pacman.
    let Some(flags) = pacman_args(&verb.to_string_lossy(), &rest) else {
        usage()
    };
    clear_stale_lock();
    exec_pacman(&flags);
}

/// How the arguments a verb takes reach pacman.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Take {
    /// names follow with no separator, as the dispatched script had them.
    Loose,
    /// names arrive behind a `--` so none can read as a pacman option.
    Dashed,
    /// the verb takes no names at all.
    None,
}

/// Which arguments a verb of the helper runs: pacman's flags, then whatever
/// arguments the verb takes. A pure function so every mapping is testable
/// without root.
fn pacman_args(verb: &str, rest: &[OsString]) -> Option<Vec<OsString>> {
    let (flags, take): (&[&str], Take) = match verb {
        "refresh" => (&["-Sy"], Take::Loose),
        "install" => (&["-S", "--needed"], Take::Dashed),
        "install-asdeps" => (&["-S", "--needed", "--asdeps"], Take::Dashed),
        "remove" => (&["-Rns"], Take::Dashed),
        "upgrade" => (&["-Syu"], Take::None),
        "file" => (&["-U"], Take::Dashed),
        "file-asdeps" => (&["-U", "--asdeps"], Take::Dashed),
        _ => return None,
    };
    let mut argv: Vec<OsString> = flags.iter().map(|flag| (*flag).into()).collect();
    argv.push("--noconfirm".into());
    if take == Take::Dashed {
        argv.push("--".into());
    }
    if take != Take::None {
        argv.extend_from_slice(rest);
    }
    Some(argv)
}

/// Whether any pacman is running right now. Its `comm` is the whole test:
/// the app runs one transaction at a time, so a live pacman is a live
/// transaction, whoever started it.
fn pacman_running() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        // Only PID directories: /proc holds plenty of names that never are.
        if !entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        // A process that exits mid-scan simply fails its read, which is
        // nobody's pacman.
        if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm"))
            && comm.trim() == "pacman"
        {
            return true;
        }
    }
    false
}

/// pacman's DB lock is an empty file it opens with O_EXCL — measured on
/// pacman 7.1: mode 0000, zero bytes, no PID written — and alpm has no
/// dead-lock check at all. A lock left by a killed or crashed pacman
/// therefore answers every later transaction "unable to lock database",
/// whatever verb it asked for, forever. So every verb below clears one
/// first — but only with no live pacman anywhere: a lock nothing is holding
/// is nobody's, while a live one is a transaction that must not be run
/// twice at once and is left for pacman to refuse honestly.
fn clear_stale_lock() {
    if !Path::new(DB_LOCK).exists() {
        return;
    }
    if pacman_running() {
        println!(":: another pacman is running — leaving its database lock");
        return;
    }
    println!(":: removing stale database lock: {DB_LOCK}");
    // If a pacman took the lock between the scan and here, this delete loses
    // the race; pacman then reports the taken database either way, which is
    // the honest refusal the check aims at.
    let _ = std::fs::remove_file(DB_LOCK);
}

fn usage() -> ! {
    eprintln!(
        "usage: aelum-package-installer-helper \
         {{refresh|install|install-asdeps|remove|upgrade|file|file-asdeps}} [args...]"
    );
    std::process::exit(2)
}

/// Run pacman, replacing this process — the same exec the script used, so
/// pkexec reports pacman's exit status and no parent is left behind.
fn exec_pacman(flags: &[OsString]) -> ! {
    let mut cmd = Command::new("/usr/bin/stdbuf");
    cmd.arg("-oL")
        .arg("/usr/bin/pacman")
        .args(flags)
        .env_clear()
        .env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin")
        .envs(
            ENV_KEEP
                .iter()
                .filter_map(|key| std::env::var_os(key).map(|value| (*key, value))),
        );
    let err = cmd.exec();
    eprintln!("aelum-package-installer-helper: cannot run pacman: {err}");
    std::process::exit(127)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(verb: &str, rest: &[&str]) -> Vec<String> {
        let rest: Vec<OsString> = rest.iter().map(|name| (*name).into()).collect();
        pacman_args(verb, &rest)
            .expect("a known verb")
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn every_verb_runs_pacmans_command_unchanged() {
        // The dispatched script's seven mappings, flag for flag: this binary
        // replaced that script and must not quietly change an invocation.
        assert_eq!(argv("refresh", &[]), ["-Sy", "--noconfirm"]);
        assert_eq!(
            argv("install", &["htop"]),
            ["-S", "--needed", "--noconfirm", "--", "htop"]
        );
        // The asdeps verbs carry the marking makepkg's -s uses: what the AUR
        // flow brings that the user did not name is a dependency, never an
        // Explicit package that outlives every -Rns and reads as installed.
        assert_eq!(
            argv("install-asdeps", &["cmake"]),
            ["-S", "--needed", "--asdeps", "--noconfirm", "--", "cmake"]
        );
        assert_eq!(
            argv("remove", &["a", "b"]),
            ["-Rns", "--noconfirm", "--", "a", "b"]
        );
        assert_eq!(argv("upgrade", &[]), ["-Syu", "--noconfirm"]);
        assert_eq!(
            argv("file", &["/t/x.pkg.tar.zst"]),
            ["-U", "--noconfirm", "--", "/t/x.pkg.tar.zst"]
        );
        assert_eq!(
            argv("file-asdeps", &["/t/d.pkg.tar.zst"]),
            ["-U", "--asdeps", "--noconfirm", "--", "/t/d.pkg.tar.zst"]
        );
    }

    #[test]
    fn no_verb_reaches_pacman_without_noconfirm() {
        // The dialog owns the confirmation; a verb that lost the flag would
        // hang waiting for input on a pipe that has none.
        for verb in [
            "refresh",
            "install",
            "install-asdeps",
            "remove",
            "upgrade",
            "file",
            "file-asdeps",
        ] {
            assert!(
                argv(verb, &[]).iter().any(|arg| arg == "--noconfirm"),
                "{verb} would prompt"
            );
        }
    }

    #[test]
    fn an_unknown_verb_gets_no_pacman_argv() {
        // Including words shaped like pacman options: the verb is the only
        // thing here that may choose anything, and it chooses from seven.
        assert!(pacman_args("-Sy", &[]).is_none());
        assert!(pacman_args("rm", &[]).is_none());
        assert!(pacman_args("", &[]).is_none());
    }

    #[test]
    fn names_arrive_behind_the_double_dash() {
        // A package name like "-S" must stay a name and never become a
        // pacman option.
        assert_eq!(
            argv("install", &["-S"]),
            ["-S", "--needed", "--noconfirm", "--", "-S"]
        );
    }

    #[test]
    fn upgrade_takes_no_names() {
        // The script dropped "$@" for this verb; a stray name here would
        // widen the transaction nobody asked for.
        assert_eq!(argv("upgrade", &["linux"]), ["-Syu", "--noconfirm"]);
    }

    #[test]
    fn the_handed_environment_is_the_allowlist_alone() {
        // A regression here reopens the hole this binary exists to close:
        // only variables that choose pacman's language survive, and the
        // ones that can change what executes — or where — never do.
        assert_eq!(
            ENV_KEEP,
            ["LANG", "LC_ALL", "LC_CTYPE", "LC_MESSAGES", "LANGUAGE"]
        );
        for danger in [
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "BASH_ENV",
            "ENV",
            "PATH",
            "IFS",
            "TMPDIR",
        ] {
            assert!(
                !ENV_KEEP.contains(&danger),
                "{danger} must never reach pacman or a package's scriptlets"
            );
        }
    }
}
