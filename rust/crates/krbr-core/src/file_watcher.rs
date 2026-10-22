//! ファイルの保存の検出。更新時刻と大きさを一定の間隔で見て、
//! 変化が止まってから（書き込みの途中や、いったん消して置き換える保存を避けて）保存とみなす

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    time: Option<SystemTime>,
    size: u64,
    exists: bool,
}

pub struct FileWatcher {
    file: PathBuf,
    interval: Duration,
    settle: Duration,
    committed: Stamp,
    candidate: Option<(Stamp, Instant)>,
    last_check: Option<Instant>,
    saved_at: Option<SystemTime>,
}

impl FileWatcher {
    /// interval ごとに調べ、変化が settle のあいだ止まったら保存とみなす（既定は with_defaults の 100 ms / 200 ms）
    pub fn new(file: &Path, interval: Duration, settle: Duration) -> Self {
        let file = file.to_path_buf();
        let committed = stamp(&file);
        Self { file, interval, settle, committed, candidate: None, last_check: None, saved_at: None }
    }

    /// 100 ms ごとに調べ、200 ms 止まったら保存とみなす（保存から読み直しまでの遅れを 0.3 秒程度に抑えつつ、書きかけを読まないための値）
    pub fn with_defaults(file: &Path) -> Self {
        Self::new(file, Duration::from_millis(100), Duration::from_millis(200))
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// 保存されたら一度だけ true
    pub fn poll(&mut self) -> bool {
        let now = Instant::now();
        if self.last_check.is_some_and(|t| now - t < self.interval) {
            return false;
        }
        self.last_check = Some(now);
        let s = stamp(&self.file);
        if !s.exists {
            return false; // 置き換えの途中（いったん消える）
        }
        if s == self.committed {
            self.candidate = None;
            return false;
        }
        match self.candidate {
            Some((c, since)) if c == s => {
                if now - since < self.settle {
                    return false;
                }
            }
            _ => {
                self.candidate = Some((s, now)); // まだ書き込み中かもしれないので、変化が止まるのを待つ
                return false;
            }
        }
        self.committed = s;
        self.candidate = None;
        self.saved_at = s.time;
        true
    }

    /// 直前に検出した保存の、ファイルの更新時刻（保存から画像の更新までの時間を測るのに使う）
    pub fn saved_at(&self) -> Option<SystemTime> {
        self.saved_at
    }
}

fn stamp(p: &Path) -> Stamp {
    match std::fs::metadata(p) {
        Ok(m) => Stamp { time: m.modified().ok(), size: m.len(), exists: true },
        Err(_) => Stamp { time: None, size: 0, exists: false },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_save_after_it_settles() {
        let dir = std::env::temp_dir().join(format!("krbr_watch_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.txt");
        std::fs::write(&f, "one").unwrap();
        let mut w = FileWatcher::new(&f, Duration::ZERO, Duration::from_millis(30));
        assert!(!w.poll()); // 変化なし
        std::fs::write(&f, "two!").unwrap();
        assert!(!w.poll()); // 変化を見つけたが、まだ落ち着いていない
        std::thread::sleep(Duration::from_millis(50));
        assert!(w.poll());
        assert!(w.saved_at().is_some());
        assert!(!w.poll()); // 一度だけ
        std::fs::remove_file(&f).unwrap();
        assert!(!w.poll()); // 消えている間は保存とみなさない
        let _ = std::fs::remove_dir_all(&dir);
    }
}
