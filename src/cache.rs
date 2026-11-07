//! The app's own cache: where its state lives under `$XDG_CACHE_HOME`, and
//! when the sync databases are old enough to be worth asking the mirrors about
//! again. **No GTK, no network, no process.**
//!
//! The startup check is the only network call the app makes on its own, and
//! the databases it fetches stay the same bytes for minutes at a time. Their
//! mtimes cannot say when a check last ran — `pacman -Sy` sends a conditional
//! request and leaves the `.db` files untouched when the mirror answers
//! "not modified" — so the app keeps that record itself, and the record
//! outlives the run that wrote it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a successful check counts as current. Mirrors do not change on a
/// shorter clock than this, and a launch that re-fetches every database is a
/// window that opens slowly for nothing.
pub const MAX_AGE: Duration = Duration::from_secs(600);

/// The app's own cache directory. `$XDG_CACHE_HOME` first, like every other
/// cache on the desktop; everything under here is disposable — losing it costs
/// one network check, never information the user cannot get again.
pub fn app_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".cache"))
        })
        .ok_or_else(|| "no cache directory to write in".to_string())?;
    Ok(base.join("aelum").join("package-installer"))
}

/// Where the moment of the last successful database check is recorded.
pub fn stamp_path() -> Result<PathBuf, String> {
    Ok(app_dir()?.join("last-check"))
}

/// When an earlier run last left the databases current, if anything here says so.
pub fn read_stamp() -> Option<SystemTime> {
    read_at(&stamp_path().ok()?)
}

/// Records that the databases are current now. A failed write costs one extra
/// check on the next launch, which is what happens without this file at all —
/// not worth a dialog.
pub fn write_stamp(at: SystemTime) {
    if let Ok(path) = stamp_path() {
        write_at(&path, at);
    }
}

/// Is a database check owed? `recorded` is the newest evidence that the
/// databases were current — this run's successful check, or the record an
/// earlier one left. `databases` is the newest mtime among the databases that
/// exist, which proves someone fetched them at that moment. The newer of the
/// two is the honest age; no evidence at all, no databases to read, or evidence
/// dated ahead of the clock (a clock that can be behind its own records cannot
/// say how old anything is) all mean: check.
pub fn check_is_owed(
    recorded: Option<SystemTime>,
    databases: Option<SystemTime>,
    now: SystemTime,
) -> bool {
    // Databases that cannot be read at all are the one case no record can
    // answer: an empty page must never read as a result, so the mirrors get asked.
    let Some(databases) = databases else { return true };
    let evidence = recorded.unwrap_or(databases).max(databases);
    match now.duration_since(evidence) {
        Ok(age) => age >= MAX_AGE,
        Err(_) => true,
    }
}

/// The stamp is one number: seconds since the epoch. Anything that does not
/// parse as one is no evidence, not a zero.
fn read_at(path: &Path) -> Option<SystemTime> {
    let text = std::fs::read_to_string(path).ok()?;
    let secs: u64 = text.trim().parse().ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(secs))
}

fn write_at(path: &Path, at: SystemTime) {
    let Ok(secs) = at.duration_since(UNIX_EPOCH) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, secs.as_secs().to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed clock well clear of the epoch, so ages are exact and the test
    /// never sleeps.
    fn clock() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aelum-pkg-installer-cache-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn databases_younger_than_the_window_need_no_check() {
        let now = clock();
        assert!(!check_is_owed(
            Some(now - Duration::from_secs(1)),
            Some(now - Duration::from_secs(1)),
            now
        ));
        assert!(!check_is_owed(
            None,
            Some(now - (MAX_AGE - Duration::from_secs(1))),
            now
        ));
    }

    #[test]
    fn databases_older_than_the_window_owe_a_check() {
        let now = clock();
        assert!(check_is_owed(Some(now - MAX_AGE), Some(now - MAX_AGE), now));
        assert!(check_is_owed(None, Some(now - MAX_AGE * 3), now));
    }

    #[test]
    fn the_newer_piece_of_evidence_is_the_one_that_counts() {
        let now = clock();
        // A successful -Sy that the mirror answered "not modified" leaves the
        // files old: the record is what says a check ran.
        assert!(!check_is_owed(
            Some(now - Duration::from_secs(60)),
            Some(now - Duration::from_secs(86_400)),
            now
        ));
        // And the other way round: a pacman run outside this app fetched them,
        // so asking again would fetch the same bytes.
        assert!(!check_is_owed(
            Some(now - Duration::from_secs(86_400)),
            Some(now - Duration::from_secs(60)),
            now
        ));
    }

    #[test]
    fn no_evidence_owes_a_check() {
        let now = clock();
        assert!(check_is_owed(None, None, now));
        // Nothing readable in the sync dir: no record, however fresh, makes an
        // empty page honest.
        assert!(check_is_owed(Some(now - Duration::from_secs(1)), None, now));
        // A record dated ahead of the clock: the clock moved, so it cannot be
        // trusted to call anything recent.
        assert!(check_is_owed(
            Some(now + Duration::from_secs(60)),
            Some(now),
            now
        ));
    }

    #[test]
    fn the_stamp_survives_the_run_that_wrote_it() {
        let dir = test_dir("roundtrip");
        let path = dir.join("last-check");
        let at = clock();
        write_at(&path, at);
        assert_eq!(read_at(&path), Some(at));

        // The next launch, minutes later, reads the record and asks the mirrors
        // nothing; ten minutes and a second later it does.
        let record = read_at(&path);
        assert!(!check_is_owed(record, Some(at), at + Duration::from_secs(300)));
        assert!(check_is_owed(record, Some(at), at + MAX_AGE + Duration::from_secs(1)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_stamp_is_no_evidence() {
        let dir = test_dir("unreadable");
        let path = dir.join("last-check");
        // Missing, empty, someone else's text, a number that does not fit: all
        // mean "unknown", which means a check is owed — never "brand new".
        assert_eq!(read_at(&path), None);
        let huge = "9".repeat(30);
        for text in [
            "",
            "   ",
            "not a time",
            "2026-11-07 10:00",
            "18e9",
            huge.as_str(),
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(read_at(&path), None, "{text:?} read as a check time");
        }
        // A directory where the file belongs is an unreadable stamp too.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(read_at(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_stamp_lives_under_the_app_s_own_name() {
        // Whatever $XDG_CACHE_HOME resolves to (the app directory itself is
        // pinned in remote.rs, and the AUR tree is its `aur` sibling), the
        // record belongs to this app alone and nothing else reads it.
        let stamp = stamp_path().unwrap();
        let names: Vec<_> = stamp
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(n) => Some(n.to_os_string()),
                _ => None,
            })
            .collect();
        assert!(
            names.ends_with(&[
                std::ffi::OsString::from("aelum"),
                std::ffi::OsString::from("package-installer"),
                std::ffi::OsString::from("last-check"),
            ]),
            "the stamp is not under the app's own cache directory: {stamp:?}"
        );
    }
}
