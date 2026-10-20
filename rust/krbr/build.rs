//! exe にアイコン（assets/krbr.ico。scripts/make_icon.py で作る）を埋め込む。リソースを作れなくてもビルドは続ける
fn main() {
    println!("cargo:rerun-if-changed=assets/krbr.ico");
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/krbr.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}
