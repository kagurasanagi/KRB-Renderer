//! ファイルの保存の検出。基板のフォルダの変更通知（notify）で変化をすぐ知り、更新時刻と大きさで確かめて保存とみなす。
//! - 一時ファイルに書いてから名前を変えて置き換えた保存（KiCad 10 以降）は、書き終わっているので待たずに保存とみなす
//! - そのまま書き込んだ保存（KiCad 9 まで）は、書き込みの途中を読まないよう、変化が止まってから保存とみなす
//! - 通知を取りこぼしたとき・使えない場所（ネットワークドライブなど）のために、一定の間隔でも更新時刻を調べる

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use notify::event::{EventKind, ModifyKind, RenameMode};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// 変更通知が使えるときの、通知とは別に更新時刻を調べる間隔（取りこぼしの備え）
const EVENT_FALLBACK_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    time: Option<SystemTime>,
    size: u64,
    exists: bool,
}

/// 見張るファイルあての通知が届いたときに呼ぶ関数（通知のスレッドから呼ぶ。待っているビューアを起こすのに使う）
type Waker = Box<dyn Fn() + Send>;

/// フォルダの変更通知（見張るファイルあての分だけを使う）
struct Events {
    /// 落とすと見張りが止まるので持っておく
    _watcher: RecommendedWatcher,
    rx: mpsc::Receiver<notify::Result<notify::Event>>,
    name: OsString,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl Events {
    fn start(file: &Path) -> Result<Self, String> {
        let name = file.file_name().ok_or("no file name")?.to_os_string();
        let dir = file.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let (tx, rx) = mpsc::channel();
        let waker: Arc<Mutex<Option<Waker>>> = Arc::default();
        let (w2, n2) = (waker.clone(), name.clone());
        let mut w = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
            let ours = ev.as_ref().map_or(true, |e| e.paths.iter().any(|p| p.file_name() == Some(n2.as_os_str())));
            let _ = tx.send(ev);
            if ours {
                if let Some(wake) = w2.lock().ok().as_deref().and_then(Option::as_ref) {
                    wake();
                }
            }
        })
        .map_err(|e| e.to_string())?;
        w.watch(dir, RecursiveMode::NonRecursive).map_err(|e| e.to_string())?;
        Ok(Self { _watcher: w, rx, name, waker })
    }

    /// 届いた通知のうち、見張るファイルあてのもの：(名前の変更で置き換わった, 中身などが変わった)
    fn drain(&self) -> (bool, bool) {
        let (mut replaced, mut touched) = (false, false);
        let is_target = |p: &PathBuf| p.file_name() == Some(self.name.as_os_str());
        while let Ok(ev) = self.rx.try_recv() {
            let Ok(ev) = ev else { continue };
            match ev.kind {
                // 名前の変更の後の名前が見張るファイル（Both は [前, 後]）。作られた（消してから書いている途中かもしれない）は待つ側
                EventKind::Modify(ModifyKind::Name(RenameMode::To)) if ev.paths.iter().any(is_target) => replaced = true,
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if ev.paths.last().is_some_and(is_target) => replaced = true,
                // 前の名前が見張るファイル（置き換えの途中で退かされた）・消えた：置き換わるのを待つ
                EventKind::Modify(ModifyKind::Name(_)) | EventKind::Remove(_) => {}
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any | EventKind::Other if ev.paths.iter().any(is_target) => touched = true,
                _ => {}
            }
        }
        (replaced, touched)
    }
}

pub struct FileWatcher {
    file: PathBuf,
    /// 更新時刻を調べる間隔（変更通知が使えれば EVENT_FALLBACK_INTERVAL）
    interval: Duration,
    settle: Duration,
    committed: Stamp,
    candidate: Option<(Stamp, Instant)>,
    last_check: Option<Instant>,
    saved_at: Option<SystemTime>,
    events: Option<Events>,
}

impl FileWatcher {
    /// フォルダの変更通知で見張る。そのまま書き込んだ保存は、変化が settle のあいだ止まったら保存とみなす。
    /// 通知が使えなければ interval ごとに更新時刻を調べる（既定は with_defaults の 100 ms / 200 ms）
    pub fn new(file: &Path, interval: Duration, settle: Duration) -> Self {
        let mut w = Self::polling(file, interval, settle);
        match Events::start(file) {
            Ok(e) => {
                w.events = Some(e);
                w.interval = EVENT_FALLBACK_INTERVAL.max(interval);
            }
            Err(e) => crate::debug!("watch {}: no change notifications ({e}); polling every {} ms", file.display(), interval.as_millis()),
        }
        w
    }

    /// 変更通知を使わず、interval ごとに更新時刻を調べるだけ
    pub fn polling(file: &Path, interval: Duration, settle: Duration) -> Self {
        let file = file.to_path_buf();
        let committed = stamp(&file);
        Self { file, interval, settle, committed, candidate: None, last_check: None, saved_at: None, events: None }
    }

    /// 通知が使えなければ 100 ms ごとに調べ、そのまま書き込んだ保存は 200 ms 止まったら保存とみなす
    /// （保存から読み直しまでの遅れを 0.3 秒程度に抑えつつ、書きかけを読まないための値）
    pub fn with_defaults(file: &Path) -> Self {
        Self::new(file, Duration::from_millis(100), Duration::from_millis(200))
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// フォルダの変更通知を使っているか
    pub fn uses_events(&self) -> bool {
        self.events.is_some()
    }

    /// 見張るファイルあての通知が届いたら wake を呼ぶ（通知のスレッドから。眠っているイベントループを起こして poll させる）。
    /// 変更通知を使っていなければ何もしない
    pub fn set_waker(&mut self, wake: impl Fn() + Send + 'static) {
        if let Some(e) = &self.events {
            if let Ok(mut w) = e.waker.lock() {
                *w = Some(Box::new(wake));
            }
        }
    }

    /// set_waker 済みか
    pub fn has_waker(&self) -> bool {
        self.events.as_ref().is_some_and(|e| e.waker.lock().is_ok_and(|w| w.is_some()))
    }

    /// 保存されたら一度だけ true
    pub fn poll(&mut self) -> bool {
        let now = Instant::now();
        let (replaced, touched) = self.events.as_ref().map(Events::drain).unwrap_or_default();
        let settled = self.candidate.is_some_and(|(_, since)| now - since >= self.settle);
        let due = self.last_check.is_none_or(|t| now - t >= self.interval);
        if !(replaced || touched || settled || due) {
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
        // 名前の変更で置き換わった：書き終えたファイルなので、待たずに保存とみなす
        if !replaced {
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

    /// テストごとの空のフォルダ
    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("krbr_watch_test_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// limit のあいだ poll し続け、保存とみなすまでの時間（みなさなければ None）
    fn wait_for_save(w: &mut FileWatcher, limit: Duration) -> Option<Duration> {
        let t0 = Instant::now();
        while t0.elapsed() < limit {
            if w.poll() {
                return Some(t0.elapsed());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    /// 更新時刻の分解能を超えて、変化が見えるように少し待つ
    fn tick() {
        std::thread::sleep(Duration::from_millis(20));
    }

    #[test]
    fn detects_a_save_after_it_settles() {
        let dir = test_dir("settle");
        let f = dir.join("a.txt");
        std::fs::write(&f, "one").unwrap();
        let mut w = FileWatcher::polling(&f, Duration::ZERO, Duration::from_millis(30));
        assert!(!w.poll()); // 変化なし
        tick();
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

    /// 一時ファイルに書いてから名前を変えて置き換える保存（KiCad 10）：変化が止まるのを待たずに、通知ですぐ保存とみなす
    #[test]
    fn a_replacing_rename_is_a_save_at_once() {
        let dir = test_dir("rename");
        let f = dir.join("board.kicad_pcb");
        std::fs::write(&f, "one").unwrap();
        // 待ちの時間と調べる間隔より十分短く見つかれば、通知で見つけている
        let mut w = FileWatcher::new(&f, Duration::from_secs(5), Duration::from_secs(5));
        assert!(w.uses_events());
        assert!(!w.poll());
        tick();
        let tmp = dir.join("board.kicad_pcb.tmp1234");
        std::fs::write(&tmp, "two, longer").unwrap();
        std::fs::rename(&tmp, &f).unwrap();
        let took = wait_for_save(&mut w, Duration::from_secs(2));
        assert!(took.is_some_and(|t| t < Duration::from_millis(500)), "{took:?}");
        assert!(!w.poll()); // 一度だけ
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 通知が届くと、set_waker で渡した関数が呼ばれる（ほかのファイルの通知では呼ばれない）
    #[test]
    fn the_waker_is_called_on_a_save() {
        let dir = test_dir("waker");
        let f = dir.join("board.kicad_pcb");
        std::fs::write(&f, "one").unwrap();
        let mut w = FileWatcher::new(&f, Duration::from_secs(5), Duration::from_secs(5));
        let (tx, rx) = mpsc::channel();
        w.set_waker(move || {
            let _ = tx.send(());
        });
        assert!(w.has_waker());
        std::fs::write(dir.join("other.txt"), "x").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        let tmp = dir.join("board.kicad_pcb.tmp");
        std::fs::write(&tmp, "two").unwrap();
        std::fs::rename(&tmp, &f).unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        assert!(w.poll());
        drop(w);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// そのまま書き込む保存（KiCad 9）：通知で変化を知っても、変化が止まるまで待つ
    #[test]
    fn an_in_place_write_waits_until_it_settles() {
        let dir = test_dir("inplace");
        let f = dir.join("board.kicad_pcb");
        std::fs::write(&f, "one").unwrap();
        let settle = Duration::from_millis(300);
        let mut w = FileWatcher::new(&f, Duration::from_secs(5), settle);
        assert!(w.uses_events());
        tick();
        std::fs::write(&f, "two, longer").unwrap();
        let took = wait_for_save(&mut w, Duration::from_secs(2));
        assert!(took.is_some_and(|t| t >= settle && t < Duration::from_millis(1000)), "{took:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 同じフォルダのほかのファイル（KiCad の一時ファイル・.kicad_prl など）が変わっても、保存とみなさない
    #[test]
    fn other_files_in_the_folder_are_ignored() {
        let dir = test_dir("others");
        let f = dir.join("board.kicad_pcb");
        std::fs::write(&f, "one").unwrap();
        let mut w = FileWatcher::new(&f, Duration::ZERO, Duration::from_millis(30));
        assert!(w.uses_events());
        tick();
        std::fs::write(dir.join("board.kicad_prl"), "x").unwrap();
        let tmp = dir.join("board.kicad_pcb.tmp1");
        std::fs::write(&tmp, "y").unwrap();
        std::fs::rename(&tmp, dir.join("board-backup.kicad_pcb")).unwrap();
        assert_eq!(wait_for_save(&mut w, Duration::from_millis(300)), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 通知が届かなくても（使えない場所・取りこぼし）、一定の間隔で調べて保存を見つける
    #[test]
    fn polling_alone_finds_a_save() {
        let dir = test_dir("polling");
        let f = dir.join("board.kicad_pcb");
        std::fs::write(&f, "one").unwrap();
        let mut w = FileWatcher::polling(&f, Duration::from_millis(50), Duration::from_millis(50));
        assert!(!w.uses_events());
        tick();
        let tmp = dir.join("board.kicad_pcb.tmp");
        std::fs::write(&tmp, "two, longer").unwrap();
        std::fs::rename(&tmp, &f).unwrap();
        let took = wait_for_save(&mut w, Duration::from_secs(2));
        assert!(took.is_some_and(|t| t >= Duration::from_millis(50)), "{took:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
