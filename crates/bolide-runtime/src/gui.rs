//! Cross-platform egui runtime for the Bolide GUI standard library.

use crate::BolideString;
use eframe::egui;
use egui::{FontData, FontDefinitions, FontFamily};

type GuiView = unsafe extern "C" fn(*mut u8);

#[repr(C)]
pub struct BolideGuiUi {
    ui: *mut egui::Ui,
}

struct BolideEguiApp {
    view: GuiView,
}

fn bstr(ptr: *const BolideString) -> String {
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { (&*ptr).as_str().to_string() }
    }
}

fn ui_from_handle<'a>(handle: *mut BolideGuiUi) -> Option<&'a mut egui::Ui> {
    if handle.is_null() {
        return None;
    }
    let ui_ptr = unsafe { (*handle).ui };
    if ui_ptr.is_null() {
        None
    } else {
        Some(unsafe { &mut *ui_ptr })
    }
}

fn with_ui<R>(handle: *mut BolideGuiUi, fallback: R, f: impl FnOnce(&mut egui::Ui) -> R) -> R {
    match ui_from_handle(handle) {
        Some(ui) => f(ui),
        None => fallback,
    }
}

fn call_view(ui: &mut egui::Ui, view: GuiView) {
    let mut handle = BolideGuiUi {
        ui: ui as *mut egui::Ui,
    };
    let obj = crate::object_alloc(std::mem::size_of::<usize>());
    unsafe {
        (obj as *mut *mut BolideGuiUi).write(&mut handle as *mut BolideGuiUi);
        view(obj);
    }
    crate::object_release(obj);
}

fn load_system_cjk_font() -> Option<Vec<u8>> {
    let candidates: &[&str] = &[
        #[cfg(target_os = "windows")]
        r"C:\Windows\Fonts\simhei.ttf",
        #[cfg(target_os = "windows")]
        r"C:\Windows\Fonts\msyh.ttc",
        #[cfg(target_os = "windows")]
        r"C:\Windows\Fonts\simsun.ttc",
        #[cfg(target_os = "macos")]
        "/System/Library/Fonts/PingFang.ttc",
        #[cfg(target_os = "macos")]
        "/System/Library/Fonts/STHeiti Light.ttc",
        #[cfg(target_os = "linux")]
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        #[cfg(target_os = "linux")]
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        #[cfg(target_os = "linux")]
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        #[cfg(target_os = "linux")]
        "/usr/share/fonts/truetype/arphic/uming.ttc",
    ];

    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(bytes);
        }
    }
    None
}

fn install_system_fonts(ctx: &egui::Context) {
    let Some(font) = load_system_cjk_font() else {
        return;
    };

    let mut fonts = FontDefinitions::default();
    let name = "bolide-system-cjk".to_string();
    fonts.font_data.insert(name.clone(), FontData::from_owned(font));
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .push(name.clone());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push(name);
    ctx.set_fonts(fonts);
}

impl eframe::App for BolideEguiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            call_view(ui, self.view);
        });
    }
}

#[no_mangle]
pub extern "C" fn bolide_gui_backend() -> *mut BolideString {
    BolideString::new("egui/eframe")
}

#[no_mangle]
pub extern "C" fn bolide_gui_run(
    title: *const BolideString,
    width: i64,
    height: i64,
    view: Option<GuiView>,
) -> i64 {
    let Some(view) = view else {
        return 0;
    };

    let title = bstr(title);
    let width = width.max(240) as f32;
    let height = height.max(160) as f32;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size(egui::vec2(width, height)),
        run_and_return: false,
        event_loop_builder: Some(Box::new(|builder| {
            #[cfg(target_os = "windows")]
            {
                use winit::platform::windows::EventLoopBuilderExtWindows;
                builder.with_any_thread(true);
            }
        })),
        ..Default::default()
    };

    match eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            install_system_fonts(&cc.egui_ctx);
            cc.egui_ctx.set_visuals(egui::Visuals::light());
            Ok(Box::new(BolideEguiApp { view }))
        }),
    ) {
        Ok(()) => 1,
        Err(err) => {
            eprintln!("bolide gui error: {err}");
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn bolide_gui_label(ui: *mut BolideGuiUi, text: *const BolideString) {
    let text = bstr(text);
    with_ui(ui, (), |ui| {
        ui.label(text);
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_heading(ui: *mut BolideGuiUi, text: *const BolideString) {
    let text = bstr(text);
    with_ui(ui, (), |ui| {
        ui.heading(text);
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_small(ui: *mut BolideGuiUi, text: *const BolideString) {
    let text = bstr(text);
    with_ui(ui, (), |ui| {
        ui.small(text);
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_strong(ui: *mut BolideGuiUi, text: *const BolideString) {
    let text = bstr(text);
    with_ui(ui, (), |ui| {
        ui.label(egui::RichText::new(text).strong());
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_separator(ui: *mut BolideGuiUi) {
    with_ui(ui, (), |ui| {
        ui.separator();
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_space(ui: *mut BolideGuiUi, points: i64) {
    with_ui(ui, (), |ui| {
        ui.add_space(points.max(0) as f32);
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_button(ui: *mut BolideGuiUi, text: *const BolideString) -> i64 {
    let text = bstr(text);
    with_ui(ui, 0, |ui| if ui.button(text).clicked() { 1 } else { 0 })
}

#[no_mangle]
pub extern "C" fn bolide_gui_link(
    ui: *mut BolideGuiUi,
    label: *const BolideString,
    url: *const BolideString,
) -> i64 {
    let label = bstr(label);
    let url = bstr(url);
    with_ui(ui, 0, |ui| {
        if ui.hyperlink_to(label, url).clicked() {
            1
        } else {
            0
        }
    })
}

#[no_mangle]
pub extern "C" fn bolide_gui_text_input(
    ui: *mut BolideGuiUi,
    id: *const BolideString,
    value: *const BolideString,
) -> *mut BolideString {
    let id = bstr(id);
    let mut value = bstr(value);
    with_ui(ui, (), |ui| {
        ui.add(egui::TextEdit::singleline(&mut value).id_source(id));
    });
    BolideString::new(&value)
}

#[no_mangle]
pub extern "C" fn bolide_gui_password_input(
    ui: *mut BolideGuiUi,
    id: *const BolideString,
    value: *const BolideString,
) -> *mut BolideString {
    let id = bstr(id);
    let mut value = bstr(value);
    with_ui(ui, (), |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut value)
                .id_source(id)
                .password(true),
        );
    });
    BolideString::new(&value)
}

#[no_mangle]
pub extern "C" fn bolide_gui_multiline_input(
    ui: *mut BolideGuiUi,
    id: *const BolideString,
    value: *const BolideString,
    rows: i64,
) -> *mut BolideString {
    let id = bstr(id);
    let mut value = bstr(value);
    with_ui(ui, (), |ui| {
        ui.add(
            egui::TextEdit::multiline(&mut value)
                .id_source(id)
                .desired_rows(rows.clamp(1, 64) as usize),
        );
    });
    BolideString::new(&value)
}

#[no_mangle]
pub extern "C" fn bolide_gui_checkbox(
    ui: *mut BolideGuiUi,
    label: *const BolideString,
    checked: i64,
) -> i64 {
    let label = bstr(label);
    let mut checked = checked != 0;
    with_ui(ui, (), |ui| {
        ui.checkbox(&mut checked, label);
    });
    if checked {
        1
    } else {
        0
    }
}

#[no_mangle]
pub extern "C" fn bolide_gui_slider(
    ui: *mut BolideGuiUi,
    label: *const BolideString,
    value: i64,
    min: i64,
    max: i64,
) -> i64 {
    let label = bstr(label);
    let (min, max) = if min <= max { (min, max) } else { (max, min) };
    let mut value = value.clamp(min, max);
    with_ui(ui, (), |ui| {
        ui.add(egui::Slider::new(&mut value, min..=max).text(label));
    });
    value
}

#[no_mangle]
pub extern "C" fn bolide_gui_progress(ui: *mut BolideGuiUi, value: i64, max: i64) {
    let fraction = if max <= 0 {
        0.0
    } else {
        (value as f32 / max as f32).clamp(0.0, 1.0)
    };
    with_ui(ui, (), |ui| {
        ui.add(egui::ProgressBar::new(fraction).show_percentage());
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_pack(
    ui: *mut BolideGuiUi,
    side: *const BolideString,
    spacing: i64,
    child: Option<GuiView>,
) {
    let Some(child) = child else {
        return;
    };
    let side = bstr(side).to_ascii_lowercase();
    let spacing = spacing.max(0) as f32;
    with_ui(ui, (), |ui| {
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(spacing, spacing);
            match side.as_str() {
                "left" | "horizontal" | "x" => {
                    ui.horizontal(|ui| call_view(ui, child));
                }
                "right" => {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        call_view(ui, child)
                    });
                }
                "bottom" => {
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                        call_view(ui, child)
                    });
                }
                _ => {
                    ui.vertical(|ui| call_view(ui, child));
                }
            }
        });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_row(ui: *mut BolideGuiUi, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    with_ui(ui, (), |ui| {
        ui.horizontal(|ui| call_view(ui, child));
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_column(ui: *mut BolideGuiUi, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    with_ui(ui, (), |ui| {
        ui.vertical(|ui| call_view(ui, child));
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_group(ui: *mut BolideGuiUi, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    with_ui(ui, (), |ui| {
        ui.group(|ui| call_view(ui, child));
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_grid(
    ui: *mut BolideGuiUi,
    id: *const BolideString,
    columns: i64,
    striped: i64,
    child: Option<GuiView>,
) {
    let Some(child) = child else {
        return;
    };
    let id = bstr(id);
    let columns = columns.max(1) as usize;
    with_ui(ui, (), |ui| {
        egui::Grid::new(id)
            .num_columns(columns)
            .striped(striped != 0)
            .show(ui, |ui| call_view(ui, child));
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_end_row(ui: *mut BolideGuiUi) {
    with_ui(ui, (), |ui| {
        ui.end_row();
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_frame(
    ui: *mut BolideGuiUi,
    title: *const BolideString,
    child: Option<GuiView>,
) {
    let Some(child) = child else {
        return;
    };
    let title = bstr(title);
    with_ui(ui, (), |ui| {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            if !title.is_empty() {
                ui.strong(title);
                ui.separator();
            }
            call_view(ui, child);
        });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_scroll(
    ui: *mut BolideGuiUi,
    id: *const BolideString,
    max_height: i64,
    child: Option<GuiView>,
) {
    let Some(child) = child else {
        return;
    };
    let id = bstr(id);
    with_ui(ui, (), |ui| {
        let area = egui::ScrollArea::vertical().id_salt(id);
        if max_height > 0 {
            area.max_height(max_height as f32)
                .show(ui, |ui| call_view(ui, child));
        } else {
            area.show(ui, |ui| call_view(ui, child));
        }
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_indent(
    ui: *mut BolideGuiUi,
    id: *const BolideString,
    child: Option<GuiView>,
) {
    let Some(child) = child else {
        return;
    };
    let id = bstr(id);
    with_ui(ui, (), |ui| {
        ui.indent(id, |ui| call_view(ui, child));
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_centered(ui: *mut BolideGuiUi, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    with_ui(ui, (), |ui| {
        ui.vertical_centered(|ui| call_view(ui, child));
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_align(
    ui: *mut BolideGuiUi,
    mode: *const BolideString,
    child: Option<GuiView>,
) {
    let Some(child) = child else {
        return;
    };
    let mode = bstr(mode).to_ascii_lowercase();
    with_ui(ui, (), |ui| {
        let align = match mode.as_str() {
            "center" | "middle" => egui::Align::Center,
            "right" | "bottom" | "end" => egui::Align::Max,
            _ => egui::Align::Min,
        };
        ui.with_layout(egui::Layout::top_down(align), |ui| call_view(ui, child));
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_pad(
    ui: *mut BolideGuiUi,
    x: i64,
    y: i64,
    child: Option<GuiView>,
) {
    let Some(child) = child else {
        return;
    };
    let x = x.max(0) as f32;
    let y = y.max(0) as f32;
    with_ui(ui, (), |ui| {
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(x, y))
            .show(ui, |ui| call_view(ui, child));
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_width(ui: *mut BolideGuiUi, points: i64, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    let points = points.max(1) as f32;
    with_ui(ui, (), |ui| {
        ui.scope(|ui| {
            ui.set_min_width(points);
            ui.set_max_width(points);
            call_view(ui, child);
        });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_height(ui: *mut BolideGuiUi, points: i64, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    let points = points.max(1) as f32;
    with_ui(ui, (), |ui| {
        ui.scope(|ui| {
            ui.set_min_height(points);
            ui.set_max_height(points);
            call_view(ui, child);
        });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_size(
    ui: *mut BolideGuiUi,
    width: i64,
    height: i64,
    child: Option<GuiView>,
) {
    let Some(child) = child else {
        return;
    };
    let size = egui::vec2(width.max(1) as f32, height.max(1) as f32);
    with_ui(ui, (), |ui| {
        ui.scope(|ui| {
            ui.set_min_size(size);
            ui.set_max_size(size);
            call_view(ui, child);
        });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_fill_width(ui: *mut BolideGuiUi, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    with_ui(ui, (), |ui| {
        let width = ui.available_width().max(1.0);
        ui.scope(|ui| {
            ui.set_min_width(width);
            call_view(ui, child);
        });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_fill_height(ui: *mut BolideGuiUi, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    with_ui(ui, (), |ui| {
        let height = ui.available_height().max(1.0);
        ui.scope(|ui| {
            ui.set_min_height(height);
            call_view(ui, child);
        });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_fill(ui: *mut BolideGuiUi, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    with_ui(ui, (), |ui| {
        let size = egui::vec2(ui.available_width().max(1.0), ui.available_height().max(1.0));
        ui.scope(|ui| {
            ui.set_min_size(size);
            call_view(ui, child);
        });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_place(
    ui: *mut BolideGuiUi,
    x: i64,
    y: i64,
    width: i64,
    height: i64,
    child: Option<GuiView>,
) {
    let Some(child) = child else {
        return;
    };
    let x = x.max(0) as f32;
    let y = y.max(0) as f32;
    let width = width.max(1) as f32;
    let height = height.max(1) as f32;
    with_ui(ui, (), |ui| {
        let origin = ui.min_rect().min;
        let rect = egui::Rect::from_min_size(origin + egui::vec2(x, y), egui::vec2(width, height));
        ui.allocate_space(egui::vec2(x + width, y + height));
        #[allow(deprecated)]
        {
            ui.allocate_ui_at_rect(rect, |ui| {
                ui.set_min_size(egui::vec2(width, height));
                call_view(ui, child);
            });
        }
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_collapsing(
    ui: *mut BolideGuiUi,
    title: *const BolideString,
    child: Option<GuiView>,
) {
    let Some(child) = child else {
        return;
    };
    let title = bstr(title);
    with_ui(ui, (), |ui| {
        ui.collapsing(title, |ui| call_view(ui, child));
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_available_width(ui: *mut BolideGuiUi) -> i64 {
    with_ui(ui, 0, |ui| ui.available_width().round() as i64)
}

#[no_mangle]
pub extern "C" fn bolide_gui_available_height(ui: *mut BolideGuiUi) -> i64 {
    with_ui(ui, 0, |ui| ui.available_height().round() as i64)
}

#[no_mangle]
pub extern "C" fn bolide_gui_request_repaint(ui: *mut BolideGuiUi) {
    with_ui(ui, (), |ui| {
        ui.ctx().request_repaint();
    });
}
