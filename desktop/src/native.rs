use crate::{
    runtime,
    task::{Result, Task},
};
use std::{
    ffi::c_void,
    path::Path,
    ptr::{copy_nonoverlapping, null, null_mut},
    sync::Arc,
    time::Duration,
};
use tauri::Emitter;
use windows_sys::Win32::{
    Foundation::{GlobalFree, *},
    Graphics::Gdi::*,
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        LibraryLoader::GetModuleHandleW,
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        Ole::CF_UNICODETEXT,
        Threading::CreateMutexW,
    },
    UI::{Input::KeyboardAndMouse::EnableWindow, Shell::ShellExecuteW, WindowsAndMessaging::*},
};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
pub fn message(s: &str) {
    message_title("BetterWX", s);
}

pub fn message_title(title: &str, message: &str) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(message).as_ptr(),
            wide(title).as_ptr(),
            MB_OK | MB_ICONWARNING,
        );
    }
}

struct DiagnosticWindow {
    text: String,
    app: tauri::AppHandle,
}

unsafe extern "system" fn diagnostic_procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DiagnosticWindow;
        if !state.is_null() {
            match message {
                WM_COMMAND => match wparam & 0xffff {
                    1 => {
                        if copy_to_clipboard(hwnd, &(*state).text).is_ok() {
                            MessageBoxW(
                                hwnd,
                                wide("诊断信息已复制到剪贴板").as_ptr(),
                                wide("BetterWX").as_ptr(),
                                MB_OK | MB_ICONINFORMATION,
                            );
                        } else {
                            MessageBoxW(
                                hwnd,
                                wide("无法访问剪贴板").as_ptr(),
                                wide("BetterWX").as_ptr(),
                                MB_OK | MB_ICONWARNING,
                            );
                        }
                        return 0;
                    }
                    2 => {
                        let _ = (*state).app.emit("update-runtime-requested", ());
                        return 0;
                    }
                    3 => {
                        DestroyWindow(hwnd);
                        return 0;
                    }
                    _ => {}
                },
                WM_CLOSE => {
                    DestroyWindow(hwnd);
                    return 0;
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    return 0;
                }
                WM_NCDESTROY => {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    drop(Box::from_raw(state));
                    return DefWindowProcW(hwnd, message, wparam, lparam);
                }
                _ => {}
            }
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

unsafe fn copy_to_clipboard(hwnd: HWND, text: &str) -> Result<()> {
    let text = wide(text);
    let bytes = text.len() * size_of::<u16>();
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        EmptyClipboard();
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if memory.is_null() {
            CloseClipboard();
            return Err(std::io::Error::last_os_error().to_string());
        }
        let target = GlobalLock(memory) as *mut u16;
        if target.is_null() {
            GlobalFree(memory);
            CloseClipboard();
            return Err(std::io::Error::last_os_error().to_string());
        }
        copy_nonoverlapping(text.as_ptr(), target, text.len());
        GlobalUnlock(memory);
        if SetClipboardData(CF_UNICODETEXT as u32, memory).is_null() {
            GlobalFree(memory);
            CloseClipboard();
            return Err(std::io::Error::last_os_error().to_string());
        }
        CloseClipboard();
        Ok(())
    }
}

pub fn diagnostics(text: String, app: tauri::AppHandle) -> Result<()> {
    let mut state = Box::new(DiagnosticWindow { text, app });
    unsafe {
        let module = GetModuleHandleW(null());
        let class = wide("BetterWX.Diagnostics");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(diagnostic_procedure),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let width = 560;
        let height = 420;
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("BetterWX · 诊断信息").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            (GetSystemMetrics(SM_CXSCREEN) - width) / 2,
            (GetSystemMetrics(SM_CYSCREEN) - height) / 2,
            width,
            height,
            null_mut(),
            null_mut(),
            module,
            &mut *state as *mut DiagnosticWindow as *const c_void,
        );
        if hwnd.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let state = Box::into_raw(state);
        let editor = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            wide("EDIT").as_ptr(),
            wide(&(*state).text).as_ptr(),
            WS_CHILD
                | WS_VISIBLE
                | WS_VSCROLL
                | ES_MULTILINE as u32
                | ES_AUTOVSCROLL as u32
                | ES_READONLY as u32,
            20,
            20,
            504,
            300,
            hwnd,
            null_mut(),
            module,
            null(),
        );
        SendMessageW(
            editor,
            WM_SETFONT,
            GetStockObject(DEFAULT_GUI_FONT) as usize,
            1,
        );
        for (id, label, x, width) in [
            (1, "复制诊断", 20, 112),
            (2, "更新 WebView2", 142, 142),
            (3, "关闭", 422, 102),
        ] {
            let control = CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide(label).as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                x,
                338,
                width,
                34,
                hwnd,
                id as HMENU,
                module,
                null(),
            );
            SendMessageW(
                control,
                WM_SETFONT,
                GetStockObject(DEFAULT_GUI_FONT) as usize,
                1,
            );
        }
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

pub fn elevate() -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            wide("runas").as_ptr(),
            wide(&exe.to_string_lossy()).as_ptr(),
            wide("--wait-for-instance").as_ptr(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    if result as isize <= 32 {
        return Err("未获得管理员授权，当前程序继续运行".into());
    }
    Ok(())
}

pub struct Instance(HANDLE);
impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
pub fn single_instance() -> Result<Instance> {
    unsafe {
        let h = CreateMutexW(null(), 0, wide("Local\\BetterWX.Desktop.2").as_ptr());
        if h.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(h);
            return Err("BetterWX 已在运行，请切换到已有窗口".into());
        }
        Ok(Instance(h))
    }
}

struct Window {
    label: HWND,
    button: HWND,
    pause: HWND,
    clear: HWND,
    task: Option<Arc<Task>>,
    logs: std::path::PathBuf,
    success: bool,
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Window;
        if !ptr.is_null() {
            let state = &mut *ptr;
            match message {
                WM_COMMAND => {
                    match wparam & 0xffff {
                        1 => {
                            if state
                                .task
                                .as_ref()
                                .is_some_and(|t| t.snapshot().status == "running")
                            {
                                return 0;
                            }
                            match Task::new("安装 / 更新 WebView2", &state.logs) {
                                Ok(task) => {
                                    let worker = task.clone();
                                    state.task = Some(task);
                                    std::thread::spawn(move || {
                                        let r = runtime::install(&worker);
                                        worker.finish(r);
                                    });
                                }
                                Err(e) => crate::native::message(&e),
                            }
                        }
                        2 => {
                            if let Some(task) = &state.task {
                                if task.cancellable.load(std::sync::atomic::Ordering::Relaxed) {
                                    task.progress.cancel();
                                }
                            }
                        }
                        3 => {
                            if !state
                                .task
                                .as_ref()
                                .is_some_and(|t| t.snapshot().status == "running")
                            {
                                if let Err(e) = crate::download::clear_cache() {
                                    crate::native::message(&e);
                                }
                            }
                        }
                        _ => {}
                    }
                    return 0;
                }
                WM_TIMER => {
                    if let Some(task) = &state.task {
                        let s = task.snapshot();
                        let text = if s.total > 0 {
                            format!(
                                "{}\r\n\r\n{:.1} / {:.1} MB  ·  {:.1} MB/s\r\n\r\n{}",
                                s.action,
                                s.bytes as f64 / 1048576.0,
                                s.total as f64 / 1048576.0,
                                s.speed / 1048576.0,
                                s.message
                            )
                        } else {
                            s.message.clone()
                        };
                        SetWindowTextW(state.label, wide(&text).as_ptr());
                        EnableWindow(state.button, (s.status != "running") as i32);
                        EnableWindow(state.clear, (s.status != "running") as i32);
                        EnableWindow(state.pause, (s.status == "running" && s.cancellable) as i32);
                        if s.status == "success" && runtime::status().compatible {
                            state.success = true;
                            DestroyWindow(hwnd);
                        }
                    }
                    return 0;
                }
                WM_CLOSE => {
                    if let Some(t) = &state.task {
                        if t.snapshot().status == "running" {
                            if t.cancellable.load(std::sync::atomic::Ordering::Relaxed) {
                                t.progress.cancel();
                            } else {
                                crate::native::message("安装程序正在运行，请等待完成后退出");
                                return 0;
                            }
                        }
                    }
                    DestroyWindow(hwnd);
                    return 0;
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    return 0;
                }
                _ => {}
            }
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

pub fn ensure_runtime(logs: &Path, force: bool) -> Result<bool> {
    if !force && runtime::status().compatible {
        return Ok(true);
    }
    let mut state = Box::new(Window {
        label: std::ptr::null_mut(),
        button: std::ptr::null_mut(),
        pause: std::ptr::null_mut(),
        clear: std::ptr::null_mut(),
        task: None,
        logs: logs.to_owned(),
        success: false,
    });
    unsafe {
        let module = GetModuleHandleW(null());
        let class = wide("BetterWX.Runtime");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let width = 560;
        let height = 320;
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("BetterWX · WebView2").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            (GetSystemMetrics(SM_CXSCREEN) - width) / 2,
            (GetSystemMetrics(SM_CYSCREEN) - height) / 2,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            &mut *state as *mut Window as *const c_void,
        );
        if hwnd.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let text = if force {
            "更新 Microsoft WebView2\r\n\r\n从微软官方下载完整安装包。支持暂停、续传和多连接下载。"
        } else {
            "BetterWX 需要 Microsoft WebView2 才能打开界面。\r\n\r\n点击下方按钮一键安装。下载支持断点续传，关闭后可继续。"
        };
        state.label = CreateWindowExW(
            0,
            wide("STATIC").as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE,
            28,
            28,
            490,
            170,
            hwnd,
            std::ptr::null_mut(),
            module,
            null(),
        );
        for (id, label, x, width) in [
            (1, "安装 / 更新并继续", 28, 200),
            (2, "暂停", 244, 100),
            (3, "清除下载", 360, 140),
        ] {
            let h = CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide(label).as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                x,
                210,
                width,
                40,
                hwnd,
                id as HMENU,
                module,
                null(),
            );
            match id {
                1 => state.button = h,
                2 => state.pause = h,
                _ => state.clear = h,
            };
            SendMessageW(h, WM_SETFONT, GetStockObject(DEFAULT_GUI_FONT) as usize, 1);
        }
        SendMessageW(
            state.label,
            WM_SETFONT,
            GetStockObject(DEFAULT_GUI_FONT) as usize,
            1,
        );
        EnableWindow(state.pause, 0);
        SetTimer(hwnd, 1, 250, None);
        ShowWindow(hwnd, SW_SHOW);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    if let Some(task) = state.task {
        while task.snapshot().status == "running" {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(state.success)
}

pub fn wait_until_unused_and_remove(path: &Path) {
    for _ in 0..50 {
        if std::fs::remove_dir_all(path).is_ok() || !path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
