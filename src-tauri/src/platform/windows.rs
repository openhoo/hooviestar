use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use hooviestar_engine::{NativeSurfaceKind, NativeSurfaces, SourceEnumeration};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewWindow, Window};
use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
        Graphics::Gdi::{
            AC_SRC_ALPHA, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
            CreateDIBSection, CreatePen, CreateSolidBrush, DIB_RGB_COLORS, DeleteDC, DeleteObject,
            GdiFlush, HBRUSH, LineTo, MoveToEx, PS_DASH, PS_SOLID, RoundRect, SelectObject,
            ValidateRect,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{
                GetCapture, GetKeyState, ReleaseCapture, SetCapture, VK_CONTROL, VK_MENU, VK_SHIFT,
            },
            WindowsAndMessaging::{
                CREATESTRUCTW, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW,
                DestroyWindow, GW_HWNDPREV, GWLP_USERDATA, GetClientRect, GetSystemMetrics,
                GetWindow, GetWindowLongPtrW, GetWindowRect, HTCLIENT, HTNOWHERE, HWND_BOTTOM,
                HWND_TOP, IsIconic, IsWindowVisible, MA_ACTIVATE, RegisterClassExW,
                SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
                SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
                SWP_SHOWWINDOW, SetWindowLongPtrW, SetWindowPos, ULW_ALPHA, UpdateLayeredWindow,
                WINDOW_EX_STYLE, WM_CANCELMODE, WM_CAPTURECHANGED, WM_ERASEBKGND, WM_LBUTTONDOWN,
                WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY,
                WM_NCHITTEST, WM_PAINT, WM_SIZE, WNDCLASSEXW, WS_CHILD, WS_CLIPCHILDREN,
                WS_CLIPSIBLINGS, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_VISIBLE,
            },
        },
    },
    core::w,
};

use super::{PreviewOverlayPayload, PreviewTransform};

pub struct OutputVisibility;

pub fn configure_graphics_backend() {}

impl OutputVisibility {
    pub fn prepare() -> Result<Self, String> {
        Ok(Self)
    }

    pub fn show_program(&self, window: &Window) -> Result<(), String> {
        let hwnd = window.hwnd().map_err(|error| error.to_string())?;
        let size = window.outer_size().map_err(|error| error.to_string())?;
        let virtual_left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
        let virtual_top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
        let virtual_width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
        let virtual_height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
        if virtual_width <= 0 || virtual_height <= 0 {
            return Err("Windows meldet keinen virtuellen Desktop".into());
        }
        let virtual_right = virtual_left
            .checked_add(virtual_width)
            .ok_or_else(|| "Virtueller Desktop überschreitet Win32-Koordinaten".to_string())?;
        let virtual_bottom = virtual_top
            .checked_add(virtual_height)
            .ok_or_else(|| "Virtueller Desktop überschreitet Win32-Koordinaten".to_string())?;
        let width = i32::try_from(size.width).map_err(|_| "Programmbreite ist zu groß")?;
        let height = i32::try_from(size.height).map_err(|_| "Programmhöhe ist zu groß")?;
        let (x, y) = offscreen_program_position(virtual_left, virtual_top, virtual_width, width)?;
        // Sichtbar und nicht minimiert lassen: Nur so bietet Discord das
        // Fenster als App an und Windows Graphics Capture liefert weiter
        // Frames. HWND_BOTTOM plus Offscreen-Position verhindert Aktivierung
        // und sichtbares Aufblitzen.
        unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_BOTTOM),
                x,
                y,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        }
        .map_err(|error| {
            format!("Program-Ausgabe konnte nicht offscreen platziert werden: {error}")
        })?;
        assert_mapped_offscreen(
            hwnd,
            RECT {
                left: virtual_left,
                top: virtual_top,
                right: virtual_right,
                bottom: virtual_bottom,
            },
        )
    }

    pub fn initially_visible(&self) -> bool {
        false
    }

    pub fn cleanup(self) {}
}

/// Places the mapped Program window wholly outside every virtual-desktop
/// monitor. If the left side cannot be represented, use the right side;
/// impossible coordinate ranges fail closed.
fn offscreen_program_position(
    virtual_left: i32,
    virtual_top: i32,
    virtual_width: i32,
    program_width: i32,
) -> Result<(i32, i32), String> {
    const MARGIN: i32 = 128;
    let virtual_right = virtual_left
        .checked_add(virtual_width)
        .ok_or_else(|| "Virtueller Desktop überschreitet Win32-Koordinaten".to_string())?;
    if let Some(x) = virtual_left
        .checked_sub(program_width)
        .and_then(|value| value.checked_sub(MARGIN))
    {
        return Ok((x, virtual_top));
    }
    let x = virtual_right.checked_add(MARGIN).ok_or_else(|| {
        "Program-Ausgabe kann nicht sicher offscreen platziert werden".to_string()
    })?;
    Ok((x, virtual_top))
}

fn assert_mapped_offscreen(hwnd: HWND, desktop: RECT) -> Result<(), String> {
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return Err("Program-Ausgabe ist nicht für Capture-APIs gemappt".into());
    }
    if unsafe { IsIconic(hwnd) }.as_bool() {
        return Err("Program-Ausgabe wurde unerwartet minimiert".into());
    }
    let mut program = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut program) }
        .map_err(|error| format!("Program-Position konnte nicht geprüft werden: {error}"))?;
    if rectangles_intersect(program, desktop) {
        return Err(format!(
            "Program-Ausgabe schneidet sichtbaren Desktop: program={program:?}, desktop={desktop:?}"
        ));
    }
    Ok(())
}

pub fn stage_program_for_picker(program: usize, cover: usize) -> Result<(), String> {
    let program = HWND(program as *mut _);
    let cover = HWND(cover as *mut _);
    let mut program_rectangle = RECT::default();
    let mut cover_rectangle = RECT::default();
    unsafe { GetWindowRect(program, &mut program_rectangle) }
        .map_err(|error| format!("Program-Geometrie konnte nicht gelesen werden: {error}"))?;
    unsafe { GetWindowRect(cover, &mut cover_rectangle) }
        .map_err(|error| format!("Studio-Geometrie konnte nicht gelesen werden: {error}"))?;
    let width = program_rectangle
        .right
        .saturating_sub(program_rectangle.left);
    let height = program_rectangle
        .bottom
        .saturating_sub(program_rectangle.top);
    let target = RECT {
        left: cover_rectangle.left,
        top: cover_rectangle.top,
        right: cover_rectangle.left.saturating_add(width),
        bottom: cover_rectangle.top.saturating_add(height),
    };
    if width <= 0 || height <= 0 || !rectangles_contain(cover_rectangle, target) {
        return Err(format!(
            "Studio deckt Program für sichere Auswahl nicht vollständig ab: program={program_rectangle:?}, studio={cover_rectangle:?}"
        ));
    }
    if !unsafe { IsWindowVisible(cover) }.as_bool() || unsafe { IsIconic(cover) }.as_bool() {
        return Err("Studio muss sichtbar und maximiert bleiben".into());
    }
    unsafe {
        SetWindowPos(
            program,
            Some(cover),
            target.left,
            target.top,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    }
    .map_err(|error| {
        format!("Program konnte nicht sicher für Auswahl vorbereitet werden: {error}")
    })?;
    assert_program_picker_covered(program.0 as usize, cover.0 as usize)
}

pub fn assert_program_picker_covered(program: usize, cover: usize) -> Result<(), String> {
    let program = HWND(program as *mut _);
    let cover = HWND(cover as *mut _);
    if !unsafe { IsWindowVisible(program) }.as_bool() || unsafe { IsIconic(program) }.as_bool() {
        return Err("Program ist nicht capture-fähig gemappt".into());
    }
    if !unsafe { IsWindowVisible(cover) }.as_bool() || unsafe { IsIconic(cover) }.as_bool() {
        return Err("Studio-Cover ist nicht mehr sichtbar".into());
    }
    let mut program_rectangle = RECT::default();
    let mut cover_rectangle = RECT::default();
    unsafe { GetWindowRect(program, &mut program_rectangle) }
        .map_err(|error| format!("Program-Geometrie konnte nicht geprüft werden: {error}"))?;
    unsafe { GetWindowRect(cover, &mut cover_rectangle) }
        .map_err(|error| format!("Studio-Geometrie konnte nicht geprüft werden: {error}"))?;
    if !rectangles_contain(cover_rectangle, program_rectangle) {
        return Err(format!(
            "Studio deckt Program nicht mehr vollständig ab: program={program_rectangle:?}, studio={cover_rectangle:?}"
        ));
    }
    if !window_is_above(cover, program) {
        return Err("Studio-Cover liegt nicht mehr über Program".into());
    }
    Ok(())
}

pub fn restore_program_offscreen(program: usize) -> Result<(), String> {
    let program = HWND(program as *mut _);
    let mut rectangle = RECT::default();
    unsafe { GetWindowRect(program, &mut rectangle) }
        .map_err(|error| format!("Program-Geometrie konnte nicht gelesen werden: {error}"))?;
    let width = rectangle.right.saturating_sub(rectangle.left);
    let height = rectangle.bottom.saturating_sub(rectangle.top);
    let virtual_left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let virtual_top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let virtual_width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let virtual_height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    if width <= 0 || height <= 0 || virtual_width <= 0 || virtual_height <= 0 {
        return Err("Ungültige Geometrie beim Offscreen-Wiederherstellen".into());
    }
    let (x, y) = offscreen_program_position(virtual_left, virtual_top, virtual_width, width)?;
    unsafe {
        SetWindowPos(
            program,
            Some(HWND_BOTTOM),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    }
    .map_err(|error| format!("Program konnte nicht offscreen wiederhergestellt werden: {error}"))?;
    assert_mapped_offscreen(
        program,
        RECT {
            left: virtual_left,
            top: virtual_top,
            right: virtual_left.saturating_add(virtual_width),
            bottom: virtual_top.saturating_add(virtual_height),
        },
    )
}

fn rectangles_intersect(left: RECT, right: RECT) -> bool {
    left.left < right.right
        && left.right > right.left
        && left.top < right.bottom
        && left.bottom > right.top
}

fn rectangles_contain(outer: RECT, inner: RECT) -> bool {
    outer.left <= inner.left
        && outer.top <= inner.top
        && outer.right >= inner.right
        && outer.bottom >= inner.bottom
}

fn window_is_above(upper: HWND, lower: HWND) -> bool {
    let mut current = lower;
    for _ in 0..4096 {
        let Ok(next) = (unsafe { GetWindow(current, GW_HWNDPREV) }) else {
            return false;
        };
        if next == upper {
            return true;
        }
        current = next;
    }
    false
}

const PREVIEW_INITIAL_WIDTH: i32 = 16;
const PREVIEW_INITIAL_HEIGHT: i32 = 9;
const OVERLAY_CLASS_NAME: windows::core::PCWSTR = w!("HooviestarNativePreviewOverlay");

#[derive(Clone)]
struct OverlayModel {
    visible: bool,
    output_width: f64,
    output_height: f64,
    selection: Option<super::PreviewOverlaySelection>,
}

struct PointerTracker {
    active: bool,
    x: f64,
    y: f64,
}

struct OverlayState {
    app: AppHandle,
    native_visible: AtomicBool,
    model: Mutex<OverlayModel>,
    pointer: Mutex<PointerTracker>,
}

#[derive(Clone)]
struct PreviewEntry {
    overlay: usize,
    state: Arc<OverlayState>,
}

static OVERLAY_CLASS: OnceLock<Result<(), String>> = OnceLock::new();
static PREVIEW_REGISTRY: OnceLock<Mutex<HashMap<usize, PreviewEntry>>> = OnceLock::new();

fn preview_registry() -> &'static Mutex<HashMap<usize, PreviewEntry>> {
    PREVIEW_REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn ensure_overlay_class() -> Result<HINSTANCE, String> {
    let instance = unsafe {
        GetModuleHandleW(None)
            .map_err(|error| format!("Win32-Modul für Preview-Overlay fehlt: {error}"))?
    };
    OVERLAY_CLASS
        .get_or_init(|| {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(preview_overlay_wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance.into(),
                hIcon: Default::default(),
                hCursor: Default::default(),
                hbrBackground: HBRUSH::default(),
                lpszMenuName: windows::core::PCWSTR::null(),
                lpszClassName: OVERLAY_CLASS_NAME,
                hIconSm: Default::default(),
            };
            let atom = unsafe { RegisterClassExW(&class) };
            if atom == 0 {
                Err("Win32-Klasse für Preview-Overlay konnte nicht registriert werden".into())
            } else {
                Ok(())
            }
        })
        .clone()?;
    Ok(instance.into())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewPointerEvent {
    phase: &'static str,
    pointer_id: u8,
    x: f64,
    y: f64,
    shift_key: bool,
    alt_key: bool,
    ctrl_key: bool,
    button: u8,
}

fn emit_pointer_event(state: &OverlayState, phase: &'static str, x: f64, y: f64, wparam: WPARAM) {
    let flags = wparam.0 as u32;
    let shift_key = flags & 0x0004 != 0 || unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    let alt_key = unsafe { GetKeyState(VK_MENU.0 as i32) } < 0;
    let ctrl_key = flags & 0x0008 != 0 || unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
    let payload = PreviewPointerEvent {
        phase,
        pointer_id: 1,
        x,
        y,
        shift_key,
        alt_key,
        ctrl_key,
        button: 0,
    };
    if let Err(error) = state.app.emit("preview-pointer", payload) {
        eprintln!("[hooviestar] Preview-Pointer-Ereignis konnte nicht zugestellt werden: {error}");
    }
}

fn signed_word(value: isize, shift: u32) -> i32 {
    ((value as u32).wrapping_shr(shift) as u16 as i16) as i32
}

fn normalized_pointer(hwnd: HWND, lparam: LPARAM, state: &OverlayState) -> (f64, f64) {
    let client_x = signed_word(lparam.0, 0);
    let client_y = signed_word(lparam.0, 16);
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let client_width = client.right.saturating_sub(client.left).max(1) as f64;
    let client_height = client.bottom.saturating_sub(client.top).max(1) as f64;
    let model = state.model.lock().expect("preview model mutex poisoned");
    let x = client_x as f64 / client_width * model.output_width;
    let y = client_y as f64 / client_height * model.output_height;
    (x, y)
}

fn pointer_cancel(state: &OverlayState) -> Option<(f64, f64)> {
    let mut pointer = state
        .pointer
        .lock()
        .expect("preview pointer mutex poisoned");
    if !pointer.active {
        return None;
    }
    pointer.active = false;
    Some((pointer.x, pointer.y))
}

fn cancel_pointer_capture(hwnd: HWND, state: &OverlayState, wparam: WPARAM) {
    if let Some((x, y)) = pointer_cancel(state) {
        emit_pointer_event(state, "cancel", x, y, wparam);
        unsafe {
            if GetCapture() == hwnd {
                let _ = ReleaseCapture();
            }
        }
    }
}

fn rgb(red: u8, green: u8, blue: u8) -> COLORREF {
    COLORREF(red as u32 | ((green as u32) << 8) | ((blue as u32) << 16))
}

fn rounded_i32(value: f64) -> i32 {
    if value <= i32::MIN as f64 {
        i32::MIN
    } else if value >= i32::MAX as f64 {
        i32::MAX
    } else {
        value.round() as i32
    }
}

fn selection_points(
    transform: &PreviewTransform,
    output_width: f64,
    output_height: f64,
    client_width: f64,
    client_height: f64,
) -> [(i32, i32); 8] {
    let scale_x = client_width / output_width;
    let scale_y = client_height / output_height;
    let center_x = (transform.x + transform.width / 2.0) * scale_x;
    let center_y = (transform.y + transform.height / 2.0) * scale_y;
    let half_width = transform.width * scale_x / 2.0;
    let half_height = transform.height * scale_y / 2.0;
    let angle = transform.rotation_degrees.to_radians();
    let cosine = angle.cos();
    let sine = angle.sin();
    let local = [
        (-half_width, -half_height),
        (0.0, -half_height),
        (half_width, -half_height),
        (half_width, 0.0),
        (half_width, half_height),
        (0.0, half_height),
        (-half_width, half_height),
        (-half_width, 0.0),
    ];
    local.map(|(x, y)| {
        (
            rounded_i32(center_x + x * cosine - y * sine),
            rounded_i32(center_y + x * sine + y * cosine),
        )
    })
}
fn update_layered_overlay(hwnd: HWND, state: &OverlayState) -> Result<(), String> {
    let mut client = RECT::default();
    unsafe {
        GetClientRect(hwnd, &mut client).map_err(|error| {
            format!("Preview-Overlay-Geometrie konnte nicht gelesen werden: {error}")
        })?;
    }
    let width = client.right.saturating_sub(client.left).max(1);
    let height = client.bottom.saturating_sub(client.top).max(1);
    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        bmiColors: [Default::default()],
    };
    let hdc = unsafe { CreateCompatibleDC(None) };
    if hdc.is_invalid() {
        return Err("Preview-Overlay-DIB-Kontext konnte nicht erstellt werden".into());
    }
    let mut bits: *mut c_void = std::ptr::null_mut();
    let bitmap = match unsafe {
        CreateDIBSection(Some(hdc), &bitmap_info, DIB_RGB_COLORS, &mut bits, None, 0)
    } {
        Ok(bitmap) => bitmap,
        Err(error) => {
            unsafe {
                let _ = DeleteDC(hdc);
            }
            return Err(format!(
                "Preview-Overlay-DIB konnte nicht erstellt werden: {error}"
            ));
        }
    };
    let previous_bitmap = unsafe { SelectObject(hdc, bitmap.into()) };
    let pixel_count = (width as usize).saturating_mul(height as usize);
    if bits.is_null() || pixel_count == 0 {
        unsafe {
            let _ = SelectObject(hdc, previous_bitmap);
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(hdc);
        }
        return Err("Preview-Overlay-DIB lieferte keinen Pixelpuffer".into());
    }
    let pixels = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u32>(), pixel_count) };
    pixels.fill(0x0100_0000);
    let model = state
        .model
        .lock()
        .expect("preview model mutex poisoned")
        .clone();
    if model.visible
        && let Some(selection) = model.selection
    {
        let points = selection_points(
            &selection.transform,
            model.output_width,
            model.output_height,
            width as f64,
            height as f64,
        );
        let line_color = if selection.locked {
            rgb(255, 178, 46)
        } else {
            rgb(127, 118, 255)
        };
        let handle_fill = if selection.locked {
            rgb(111, 72, 20)
        } else {
            rgb(35, 29, 92)
        };
        let pen_style = if selection.locked { PS_DASH } else { PS_SOLID };
        let pen = unsafe { CreatePen(pen_style, 2, line_color) };
        let brush = unsafe { CreateSolidBrush(handle_fill) };
        let previous_pen = unsafe { SelectObject(hdc, pen.into()) };
        let previous_brush = unsafe { SelectObject(hdc, brush.into()) };
        unsafe {
            let _ = MoveToEx(hdc, points[0].0, points[0].1, None);
            for point in points.iter().skip(1).chain(std::iter::once(&points[0])) {
                let _ = LineTo(hdc, point.0, point.1);
            }
            for (x, y) in points {
                let half = 4;
                let _ = RoundRect(
                    hdc,
                    x.saturating_sub(half),
                    y.saturating_sub(half),
                    x.saturating_add(half),
                    y.saturating_add(half),
                    3,
                    3,
                );
            }
            let _ = SelectObject(hdc, previous_pen);
            let _ = SelectObject(hdc, previous_brush);
            let _ = DeleteObject(pen.into());
            let _ = DeleteObject(brush.into());
        }
    }
    unsafe {
        let _ = GdiFlush();
    }
    for pixel in pixels {
        if *pixel & 0x00ff_ffff == 0 {
            *pixel = 0x0100_0000;
        } else {
            *pixel = (*pixel & 0x00ff_ffff) | 0xff00_0000;
        }
    }
    let size = SIZE {
        cx: width,
        cy: height,
    };
    let source = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: 0,
        BlendFlags: 0,
        SourceConstantAlpha: u8::MAX,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    let result = unsafe {
        UpdateLayeredWindow(
            hwnd,
            None,
            None,
            Some(std::ptr::addr_of!(size)),
            Some(hdc),
            Some(std::ptr::addr_of!(source)),
            COLORREF(0),
            Some(std::ptr::addr_of!(blend)),
            ULW_ALPHA,
        )
    };
    unsafe {
        let _ = SelectObject(hdc, previous_bitmap);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(hdc);
    }
    result.map_err(|error| format!("Preview-Overlay konnte nicht gezeichnet werden: {error}"))
}

unsafe extern "system" fn preview_overlay_wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let mut state_ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut OverlayState;
    if message == WM_NCCREATE && state_ptr.is_null() {
        let create = lparam.0 as *const CREATESTRUCTW;
        if !create.is_null() {
            state_ptr = unsafe { (*create).lpCreateParams } as *mut OverlayState;
            if !state_ptr.is_null() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
                }
                return LRESULT(1);
            }
        }
        return LRESULT(0);
    }
    if state_ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let state = unsafe { &*state_ptr };
    match message {
        WM_NCHITTEST => {
            let model = state.model.lock().expect("preview model mutex poisoned");
            let hit_test = if model.visible && state.native_visible.load(Ordering::Acquire) {
                HTCLIENT
            } else {
                HTNOWHERE
            };
            LRESULT(hit_test as isize)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_ACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            unsafe {
                let _ = ValidateRect(Some(hwnd), None);
            }
            if let Err(error) = update_layered_overlay(hwnd, state) {
                eprintln!("[hooviestar] Preview-Overlay konnte nicht aktualisiert werden: {error}");
            }
            LRESULT(0)
        }
        WM_SIZE => {
            if let Err(error) = update_layered_overlay(hwnd, state) {
                eprintln!("[hooviestar] Preview-Overlay konnte nicht aktualisiert werden: {error}");
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = normalized_pointer(hwnd, lparam, state);
            {
                let mut pointer = state
                    .pointer
                    .lock()
                    .expect("preview pointer mutex poisoned");
                pointer.active = true;
                pointer.x = x;
                pointer.y = y;
            }
            unsafe {
                let _ = SetCapture(hwnd);
            }
            emit_pointer_event(state, "down", x, y, wparam);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let event = {
                let mut pointer = state
                    .pointer
                    .lock()
                    .expect("preview pointer mutex poisoned");
                if !pointer.active {
                    None
                } else {
                    let (x, y) = normalized_pointer(hwnd, lparam, state);
                    pointer.x = x;
                    pointer.y = y;
                    Some((x, y))
                }
            };
            if let Some((x, y)) = event {
                emit_pointer_event(state, "move", x, y, wparam);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let event = {
                let mut pointer = state
                    .pointer
                    .lock()
                    .expect("preview pointer mutex poisoned");
                if pointer.active {
                    let (x, y) = normalized_pointer(hwnd, lparam, state);
                    pointer.x = x;
                    pointer.y = y;
                    pointer.active = false;
                    Some((x, y))
                } else {
                    None
                }
            };
            if let Some((x, y)) = event {
                emit_pointer_event(state, "up", x, y, wparam);
                unsafe {
                    let _ = ReleaseCapture();
                }
            }
            LRESULT(0)
        }
        WM_CANCELMODE | WM_CAPTURECHANGED => {
            cancel_pointer_capture(hwnd, state, wparam);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            cancel_pointer_capture(hwnd, state, wparam);
            let result = unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            result
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

pub struct NativePreview {
    hwnd: usize,
    overlay: usize,
    _overlay_state: Arc<OverlayState>,
}

impl NativePreview {
    pub fn create(
        studio: &WebviewWindow,
        program: &Window,
        _output_visibility: &OutputVisibility,
    ) -> Result<(Self, NativeSurfaces), String> {
        let studio_hwnd = studio.hwnd().map_err(|error| error.to_string())?;
        let program_hwnd = program.hwnd().map_err(|error| error.to_string())?;
        let program_size = program.inner_size().map_err(|error| error.to_string())?;
        let instance = ensure_overlay_class()?;
        let preview_hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("STATIC"),
                w!("Hooviestar Preview"),
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                PREVIEW_INITIAL_WIDTH,
                PREVIEW_INITIAL_HEIGHT,
                Some(studio_hwnd),
                None,
                Some(instance),
                None,
            )
        }
        .map_err(|error| format!("Native D3D11-Preview konnte nicht erstellt werden: {error}"))?;
        let state = Arc::new(OverlayState {
            app: studio.app_handle().clone(),
            native_visible: AtomicBool::new(true),
            model: Mutex::new(OverlayModel {
                visible: true,
                output_width: PREVIEW_INITIAL_WIDTH as f64,
                output_height: PREVIEW_INITIAL_HEIGHT as f64,
                selection: None,
            }),
            pointer: Mutex::new(PointerTracker {
                active: false,
                x: 0.0,
                y: 0.0,
            }),
        });
        let state_ptr = Arc::as_ptr(&state) as *const c_void;
        let overlay_hwnd = match unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE,
                OVERLAY_CLASS_NAME,
                w!("Hooviestar Preview Overlay"),
                WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
                0,
                0,
                PREVIEW_INITIAL_WIDTH,
                PREVIEW_INITIAL_HEIGHT,
                Some(studio_hwnd),
                None,
                Some(instance),
                Some(state_ptr),
            )
        } {
            Ok(hwnd) => hwnd,
            Err(error) => {
                unsafe {
                    let _ = DestroyWindow(preview_hwnd);
                }
                return Err(format!(
                    "Transparentes Preview-Overlay konnte nicht erstellt werden: {error}"
                ));
            }
        };
        if let Err(error) = unsafe {
            SetWindowPos(
                overlay_hwnd,
                Some(HWND_TOP),
                0,
                0,
                PREVIEW_INITIAL_WIDTH,
                PREVIEW_INITIAL_HEIGHT,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        } {
            unsafe {
                let _ = DestroyWindow(overlay_hwnd);
                let _ = DestroyWindow(preview_hwnd);
            }
            return Err(format!(
                "Preview-Overlay konnte nicht über der D3D11-Fläche platziert werden: {error}"
            ));
        }
        if let Err(error) = update_layered_overlay(overlay_hwnd, &state) {
            unsafe {
                let _ = DestroyWindow(overlay_hwnd);
                let _ = DestroyWindow(preview_hwnd);
            }
            return Err(error);
        }
        let preview = Self {
            hwnd: preview_hwnd.0 as usize,
            overlay: overlay_hwnd.0 as usize,
            _overlay_state: state.clone(),
        };
        preview_registry()
            .lock()
            .expect("preview registry mutex poisoned")
            .insert(
                preview.hwnd,
                PreviewEntry {
                    overlay: preview.overlay,
                    state,
                },
            );
        Ok((
            preview,
            NativeSurfaces {
                studio: studio_hwnd.0 as usize,
                program: program_hwnd.0 as usize,
                preview: preview_hwnd.0 as usize,
                display: 0,
                kind: NativeSurfaceKind::Win32,
                program_width: program_size.width.max(1),
                program_height: program_size.height.max(1),
                preview_width: PREVIEW_INITIAL_WIDTH as u32,
                preview_height: PREVIEW_INITIAL_HEIGHT as u32,
            },
        ))
    }

    pub fn native_handle(&self) -> usize {
        self.hwnd
    }

    fn destroy_inner(&mut self) -> Result<(), String> {
        if self.hwnd == 0 {
            return Ok(());
        }
        preview_registry()
            .lock()
            .expect("preview registry mutex poisoned")
            .remove(&self.hwnd);
        let mut errors = Vec::new();
        if self.overlay != 0 {
            match unsafe { DestroyWindow(HWND(self.overlay as *mut _)) } {
                Ok(()) => self.overlay = 0,
                Err(error) => errors.push(format!("Preview-Overlay: {error}")),
            }
        }
        if self.hwnd != 0 {
            match unsafe { DestroyWindow(HWND(self.hwnd as *mut _)) } {
                Ok(()) => self.hwnd = 0,
                Err(error) => errors.push(format!("D3D11-Preview: {error}")),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    pub fn destroy(mut self) -> Result<(), String> {
        self.destroy_inner()
    }
}

impl Drop for NativePreview {
    fn drop(&mut self) {
        let _ = self.destroy_inner();
    }
}

pub async fn enumerate_sources(surfaces: NativeSurfaces) -> Result<SourceEnumeration, String> {
    let excluded = [surfaces.studio, surfaces.program, surfaces.preview];
    let (candidates, message) = tauri::async_runtime::spawn_blocking(move || {
        let mut candidates =
            hooviestar_engine::discovery::windows::enumerate_visible_windows(&excluded)?;
        candidates.extend(hooviestar_engine::discovery::windows::enumerate_displays()?);
        let message = match hooviestar_engine::discovery::windows::enumerate_audio_sessions() {
            Ok(audio) => {
                candidates.extend(audio);
                None
            }
            Err(error) => Some(format!("Anwendungs-Audio nicht verfügbar: {error}")),
        };
        Ok::<_, String>((candidates, message))
    })
    .await
    .map_err(|error| format!("Quellenauflösung fehlgeschlagen: {error}"))??;
    Ok(SourceEnumeration {
        candidates,
        portal_selection_required: false,
        message,
    })
}

pub struct PortalResources;

impl PortalResources {
    pub fn new() -> Self {
        Self
    }

    pub async fn select(&self) -> Result<SourceEnumeration, String> {
        Err("Desktop-Portal-Auswahl ist nur unter Linux verfügbar".into())
    }

    pub fn clear(&self) {}
}

pub fn set_preview_bounds(
    hwnd: usize,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> Result<(), String> {
    if width <= 0 || height <= 0 {
        return Err("Vorschauabmessungen müssen positiv sein".into());
    }
    let entry = preview_registry()
        .lock()
        .expect("preview registry mutex poisoned")
        .get(&hwnd)
        .cloned()
        .ok_or_else(|| "Native Preview-Overlay ist nicht verfügbar".to_string())?;
    let preview_hwnd = HWND(hwnd as *mut _);
    let overlay_hwnd = HWND(entry.overlay as *mut _);
    unsafe {
        SetWindowPos(
            preview_hwnd,
            Some(HWND_TOP),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE,
        )
        .map_err(|error| format!("D3D11-Preview konnte nicht positioniert werden: {error}"))?;
        SetWindowPos(
            overlay_hwnd,
            Some(HWND_TOP),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE,
        )
        .map_err(|error| {
            format!(
                "Preview-Overlay konnte nicht über der D3D11-Fläche positioniert werden: {error}"
            )
        })?;
    }
    update_layered_overlay(overlay_hwnd, &entry.state)
}
pub fn set_preview_overlay(hwnd: usize, payload: PreviewOverlayPayload) -> Result<(), String> {
    payload.validate()?;
    let entry = preview_registry()
        .lock()
        .expect("preview registry mutex poisoned")
        .get(&hwnd)
        .cloned()
        .ok_or_else(|| "Native Preview-Overlay ist nicht verfügbar".to_string())?;
    if !payload.visible {
        cancel_pointer_capture(HWND(entry.overlay as *mut _), &entry.state, WPARAM(0));
    }
    {
        let mut model = entry
            .state
            .model
            .lock()
            .expect("preview model mutex poisoned");
        *model = OverlayModel {
            visible: payload.visible,
            output_width: payload.output_width,
            output_height: payload.output_height,
            selection: payload.selection,
        };
    }
    let overlay_visible = payload.visible && entry.state.native_visible.load(Ordering::Acquire);
    let visibility = if overlay_visible {
        SWP_SHOWWINDOW
    } else {
        SWP_HIDEWINDOW
    };
    unsafe {
        SetWindowPos(
            HWND(entry.overlay as *mut _),
            None,
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | visibility,
        )
        .map_err(|error| {
            format!("Preview-Overlay-Sichtbarkeit konnte nicht geändert werden: {error}")
        })?;
    }
    update_layered_overlay(HWND(entry.overlay as *mut _), &entry.state)
}
pub fn set_preview_visible(hwnd: usize, visible: bool) -> Result<(), String> {
    let visibility = if visible {
        SWP_SHOWWINDOW
    } else {
        SWP_HIDEWINDOW
    };
    let entry = preview_registry()
        .lock()
        .expect("preview registry mutex poisoned")
        .get(&hwnd)
        .cloned()
        .ok_or_else(|| "Native Preview-Overlay ist nicht verfügbar".to_string())?;
    if !visible {
        cancel_pointer_capture(HWND(entry.overlay as *mut _), &entry.state, WPARAM(0));
    }
    entry.state.native_visible.store(false, Ordering::Release);
    let model_visible = entry
        .state
        .model
        .lock()
        .expect("preview model mutex poisoned")
        .visible;
    let overlay_visibility = if visible && model_visible {
        SWP_SHOWWINDOW
    } else {
        SWP_HIDEWINDOW
    };
    unsafe {
        SetWindowPos(
            HWND(hwnd as *mut _),
            None,
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | visibility,
        )
        .map_err(|error| {
            format!("D3D11-Preview-Sichtbarkeit konnte nicht geändert werden: {error}")
        })?;
        SetWindowPos(
            HWND(entry.overlay as *mut _),
            None,
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | overlay_visibility,
        )
        .map_err(|error| {
            format!("Preview-Overlay-Sichtbarkeit konnte nicht geändert werden: {error}")
        })?;
    }
    if visible {
        entry.state.native_visible.store(true, Ordering::Release);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{offscreen_program_position, rectangles_contain};
    use windows::Win32::Foundation::RECT;

    #[test]
    fn program_is_placed_left_of_single_monitor_desktop() {
        assert_eq!(offscreen_program_position(0, 0, 1920, 1280), Ok((-1408, 0)));
    }

    #[test]
    fn program_is_placed_left_of_negative_origin_multi_monitor_desktop() {
        assert_eq!(
            offscreen_program_position(-1920, -120, 3840, 1920),
            Ok((-3968, -120))
        );
    }

    #[test]
    fn extreme_left_coordinate_falls_back_right_of_desktop() {
        assert_eq!(
            offscreen_program_position(i32::MIN + 64, 42, 3840, 1920),
            Ok((i32::MIN + 4032, 42))
        );
    }

    #[test]
    fn impossible_coordinate_range_fails_closed() {
        assert!(offscreen_program_position(i32::MAX - 4, 0, 8, 1920).is_err());
    }

    #[test]
    fn picker_cover_must_contain_every_program_edge() {
        let cover = RECT {
            left: -8,
            top: -8,
            right: 1928,
            bottom: 1088,
        };
        assert!(rectangles_contain(
            cover,
            RECT {
                left: 0,
                top: 0,
                right: 1280,
                bottom: 720,
            }
        ));
        assert!(!rectangles_contain(
            cover,
            RECT {
                left: 0,
                top: 0,
                right: 1930,
                bottom: 720,
            }
        ));
    }
}
