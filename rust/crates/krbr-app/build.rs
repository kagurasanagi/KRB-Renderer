//! 「KRB Renderer について」に出す、ビルドしたコミット（短いハッシュ）とその日付を環境変数で渡す。
//! git が無い・リポジトリの外でビルドしたときは空
use std::process::Command;

fn git(args: &[&str]) -> String {
    Command::new("git").args(args).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

fn main() {
    // コミットが変わったら（HEAD・ブランチの先が動いたら）作り直す
    let dir = git(&["rev-parse", "--git-dir"]);
    if !dir.is_empty() {
        println!("cargo:rerun-if-changed={dir}/HEAD");
        println!("cargo:rerun-if-changed={dir}/refs/heads");
        println!("cargo:rerun-if-changed={dir}/packed-refs");
    }
    println!("cargo:rustc-env=KRBR_GIT_HASH={}", git(&["rev-parse", "--short=9", "HEAD"]));
    println!("cargo:rustc-env=KRBR_GIT_DATE={}", git(&["log", "-1", "--format=%cs"]));
}
