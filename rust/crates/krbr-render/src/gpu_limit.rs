//! GPU の使用率の上限（目安）。使ってよい GPU 時間が、経った時間 × 上限の割合ずつたまり（50 ms まで）、
//! 終わった処理の GPU 時間をそこから引く。残りがあるときだけ次を流す（トークンバケット）。
//! 流せる時刻が表示のフレームの区切りに限られても、待った分はたまるので、長い目で見た割合は上限に合う。
//! タスクマネージャーや nvidia-smi の使用率とは計り方が違い（表示の処理も入らない）、数 % ずれる

use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct GpuLimit {
    percent: u32,
    /// 使ってよい GPU 時間の残り（秒。負は使いすぎ）と、最後に足した時刻
    budget: f64,
    refilled_at: Instant,
}

/// たまる上限（休んだ後に、まとめて流しすぎない）
const MAX_BUDGET: Duration = Duration::from_millis(50);

impl Default for GpuLimit {
    fn default() -> Self {
        Self::unlimited()
    }
}

impl GpuLimit {
    pub const MIN_PERCENT: u32 = 10;
    pub const MAX_PERCENT: u32 = 100;

    /// percent は MIN_PERCENT..=MAX_PERCENT に収める
    pub fn new(percent: u32) -> Self {
        Self { percent: percent.clamp(Self::MIN_PERCENT, Self::MAX_PERCENT), budget: 0.0, refilled_at: Instant::now() }
    }

    pub fn unlimited() -> Self {
        Self::new(Self::MAX_PERCENT)
    }

    pub fn percent(&self) -> u32 {
        self.percent
    }

    pub fn set_percent(&mut self, percent: u32) {
        *self = Self::new(percent);
    }

    /// 上限がかかっているか（100 % 未満）
    pub fn limited(&self) -> bool {
        self.percent < Self::MAX_PERCENT
    }

    fn share(&self) -> f64 {
        self.percent as f64 / Self::MAX_PERCENT as f64
    }

    /// 今までに経った時間の分を足す
    fn refill(&mut self, now: Instant) {
        let dt = now.saturating_duration_since(self.refilled_at).as_secs_f64();
        self.budget = (self.budget + dt * self.share()).min(MAX_BUDGET.as_secs_f64());
        self.refilled_at = now;
    }

    /// GPU を busy の間使った（終わった処理の分を引く）
    pub fn used(&mut self, busy: Duration) {
        if !self.limited() {
            return;
        }
        self.refill(Instant::now());
        self.budget -= busy.as_secs_f64();
    }

    /// 次を流してよいか
    pub fn ready(&mut self) -> bool {
        if !self.limited() {
            return true;
        }
        self.refill(Instant::now());
        self.budget > 0.0
    }

    /// 次を流してよくなるまで待つ
    pub fn wait(&mut self) {
        while !self.ready() {
            std::thread::sleep(Duration::from_secs_f64(-self.budget / self.share()).max(Duration::from_micros(100)));
        }
    }

    /// f を流して完了を待つ処理として呼び、かかった時間を GPU を使った時間とみなす（オフラインの描画用）
    pub fn run<T>(&mut self, f: impl FnOnce() -> T) -> T {
        self.wait();
        let t0 = Instant::now();
        let r = f();
        self.used(t0.elapsed());
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_follows_the_share() {
        let t = Instant::now();
        let mut l = GpuLimit::new(50);
        l.refilled_at = t;
        l.refill(t + Duration::from_millis(20));
        assert!((l.budget - 0.010).abs() < 1e-9); // 20 ms の 50 %
        l.budget -= 0.030;
        l.refill(t + Duration::from_millis(40));
        assert!((l.budget - -0.010).abs() < 1e-9);
        // 長く休んでもたまるのは上限まで
        l.refill(t + Duration::from_secs(10));
        assert_eq!(l.budget, MAX_BUDGET.as_secs_f64());
    }

    #[test]
    fn run_keeps_the_share() {
        let mut l = GpuLimit::new(50);
        let t0 = Instant::now();
        for _ in 0..10 {
            l.run(|| std::thread::sleep(Duration::from_millis(5)));
        }
        // 50 ms 使ったので、全体は 100 ms くらい（初めの残りが 0 なので最初の 1 回も待つ）
        let total = t0.elapsed();
        assert!(total >= Duration::from_millis(90) && total < Duration::from_millis(200), "{total:?}");
    }

    #[test]
    fn unlimited_never_waits_and_percent_is_clamped() {
        let mut l = GpuLimit::unlimited();
        l.used(Duration::from_secs(1));
        assert!(l.ready());
        assert_eq!(GpuLimit::new(0).percent(), GpuLimit::MIN_PERCENT);
        assert_eq!(GpuLimit::new(500).percent(), GpuLimit::MAX_PERCENT);
    }
}
