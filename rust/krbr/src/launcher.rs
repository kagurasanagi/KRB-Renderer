//! KRB-Renderer.exe：ビューアを開くための小さな起動役（Windows の GUI のプログラムなので、エクスプローラーから開いても
//! コンソールの窓が一瞬も出ない）。同じフォルダの krbr.exe を、コンソール無しで同じ引数のまま起動して、すぐ終わる。
//! 起動に失敗したときは、krbr.exe がログを窓で見せる（KRBR_DETACHED）
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::{Command, Stdio};

fn main() {
    let exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(if cfg!(windows) { "krbr.exe" } else { "krbr" })));
    let Some(exe) = exe.filter(|p| p.is_file()) else {
        krbr_core::platform::message_box("KRB Renderer", "krbr.exe is not found next to KRB-Renderer.exe.");
        std::process::exit(1)
    };
    let mut cmd = Command::new(&exe);
    cmd.args(std::env::args_os().skip(1)).env("KRBR_DETACHED", "1").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000 | 0x0000_0200); // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP
    }
    if let Err(e) = cmd.spawn() {
        krbr_core::platform::message_box("KRB Renderer", &format!("cannot start {}: {e}", exe.display()));
        std::process::exit(1)
    }
}
