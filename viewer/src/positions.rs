//! Where reading left off in each file, so a file opens where it was
//! closed.
//!
//! Kept in a small text file in the user's state folder
//! (`$XDG_STATE_HOME/k-pdf/positions`, `~/Library/Application Support/
//! k-pdf/positions`, `%LOCALAPPDATA%\k-pdf\positions`): one line a file,
//! `page<TAB>spot<TAB>path`, most recent first, at most [`MOST`] files.
//! It is read when a file opens and written when another file is opened
//! or the app closes; nothing is kept in memory meanwhile.

use std::path::{Path, PathBuf};

/// The files remembered (the least recently read are forgotten).
const MOST: usize = 500;

/// Where reading left off: the page (counted from 0, as shown) and the
/// spot on it (display points from its top edge).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Position {
    pub page: usize,
    pub spot: f32,
}

fn file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let dir = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).filter(|d| d.is_absolute()).or_else(|| home.map(|h| h.join(".local/state")))
    }?;
    Some(dir.join("k-pdf").join("positions"))
}

fn parse(line: &str) -> Option<(Position, &str)> {
    let mut parts = line.splitn(3, '\t');
    let page = parts.next()?.parse().ok()?;
    let spot: f32 = parts.next()?.parse().ok()?;
    let path = parts.next()?;
    Some((Position { page, spot: if spot.is_finite() { spot.max(0.0) } else { 0.0 } }, path))
}

fn read_all() -> String {
    file().and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_default()
}

/// Where reading left off in `path`, if it was read before.
pub fn get(path: &Path) -> Option<Position> {
    let key = path.to_str()?;
    read_all().lines().filter_map(parse).find(|(_, p)| *p == key).map(|(pos, _)| pos)
}

/// Remembers where reading left off in `path`.
pub fn set(path: &Path, position: Position) {
    let (Some(key), Some(file)) = (path.to_str(), file()) else { return };
    if key.contains(['\n', '\r']) {
        return;
    }
    let old = read_all();
    if old.lines().filter_map(parse).next() == Some((position, key)) {
        return; // unchanged
    }
    let mut out = format!("{}\t{:.1}\t{key}\n", position.page, position.spot);
    for line in old.lines().filter(|l| parse(l).is_some_and(|(_, p)| p != key)).take(MOST - 1) {
        out.push_str(line);
        out.push('\n');
    }
    // Written aside, then moved over the old file: never half written.
    let Some(dir) = file.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let temp = file.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&temp, out).is_ok() && std::fs::rename(&temp, &file).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_read_back() {
        assert_eq!(parse("12\t340.5\t/home/a b/c.pdf"), Some((Position { page: 12, spot: 340.5 }, "/home/a b/c.pdf")));
        assert_eq!(parse("x\t1\t/a.pdf"), None);
        assert_eq!(parse("3\t1"), None);
    }
}
