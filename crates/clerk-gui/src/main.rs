// 在 Windows 发布模式下不显示控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    clerk_gui::run();
}
