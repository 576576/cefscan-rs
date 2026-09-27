// 发布构建下隐藏控制台窗口：`cefscanw` 是 GUI 程序，不该弹出黑框。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    cefscanw_lib::run()
}
