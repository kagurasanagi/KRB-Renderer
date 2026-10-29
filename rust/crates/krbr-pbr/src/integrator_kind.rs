//! 積分器（レンダリングの方式）の種類と名前

/// 積分器（レンダリングの方式）。名前（as_str）は設定ファイルの [render] integrator、コマンドラインの --integrator、ビューアの選択と同じ
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IntegratorKind {
    /// パストレーサー
    #[default]
    Pt,
    /// 双方向パストレーシング
    Bdpt,
    /// 頂点の接続と併合（透明な物体越しの集光模様も扱う）
    Vcm,
}

impl IntegratorKind {
    /// すべての種類（ビューアの選択肢の並び）
    pub const ALL: [IntegratorKind; 3] = [IntegratorKind::Pt, IntegratorKind::Bdpt, IntegratorKind::Vcm];

    /// 設定ファイル・コマンドライン・覚えた設定での名前
    pub fn as_str(self) -> &'static str {
        match self {
            IntegratorKind::Pt => "pt",
            IntegratorKind::Bdpt => "bdpt",
            IntegratorKind::Vcm => "vcm",
        }
    }
}

/// as_str の名前で出す（幅の指定も効く）
impl std::fmt::Display for IntegratorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.as_str())
    }
}

impl std::str::FromStr for IntegratorKind {
    type Err = String;
    /// 名前（pt / bdpt / vcm。"path" は "pt" の別名）から。知らない名前は Err
    fn from_str(s: &str) -> Result<Self, String> {
        if s == "path" {
            return Ok(IntegratorKind::Pt);
        }
        Self::ALL.into_iter().find(|k| k.as_str() == s).ok_or_else(|| format!("unknown integrator '{s}' (pt / bdpt / vcm)"))
    }
}
