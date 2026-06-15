//! Cross-platform egui runtime for the Bolide GUI standard library.

use crate::BolideString;
use eframe::egui;
use egui::{FontData, FontDefinitions, FontFamily};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

type GuiView = unsafe extern "C" fn(*mut u8);

static GUI_GRID_ROWS: Lazy<Mutex<HashMap<String, usize>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy)]
struct GuiLayout {
    in_grid: bool,
    fill_height: bool,
    compact_width: bool,
    align: egui::Align,
    grid_column: usize,
    grid_rows: usize,
    grid_cell_width: f32,
    grid_cell_height: f32,
}

impl Default for GuiLayout {
    fn default() -> Self {
        Self {
            in_grid: false,
            fill_height: false,
            compact_width: false,
            align: egui::Align::Min,
            grid_column: 0,
            grid_rows: 0,
            grid_cell_width: 0.0,
            grid_cell_height: 0.0,
        }
    }
}

#[repr(C)]
pub struct BolideGuiUi {
    ui: *mut egui::Ui,
    layout: GuiLayout,
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

fn gui_from_handle<'a>(handle: *mut BolideGuiUi) -> Option<&'a mut BolideGuiUi> {
    if handle.is_null() {
        return None;
    }
    Some(unsafe { &mut *handle })
}

fn ui_from_handle<'a>(handle: *mut BolideGuiUi) -> Option<&'a mut egui::Ui> {
    let gui = gui_from_handle(handle)?;
    if gui.ui.is_null() {
        None
    } else {
        Some(unsafe { &mut *gui.ui })
    }
}

fn with_ui<R>(handle: *mut BolideGuiUi, fallback: R, f: impl FnOnce(&mut egui::Ui) -> R) -> R {
    match ui_from_handle(handle) {
        Some(ui) => f(ui),
        None => fallback,
    }
}

fn with_gui<R>(
    handle: *mut BolideGuiUi,
    fallback: R,
    f: impl FnOnce(&mut BolideGuiUi, &mut egui::Ui) -> R,
) -> R {
    let Some(gui) = gui_from_handle(handle) else {
        return fallback;
    };
    if gui.ui.is_null() {
        return fallback;
    }
    let ui = unsafe { &mut *gui.ui };
    f(gui, ui)
}

fn call_view_with_layout(ui: &mut egui::Ui, view: GuiView, layout: GuiLayout) -> GuiLayout {
    let mut handle = BolideGuiUi {
        ui: ui as *mut egui::Ui,
        layout,
    };
    let obj = crate::object_alloc(std::mem::size_of::<usize>());
    unsafe {
        (obj as *mut *mut BolideGuiUi).write(&mut handle as *mut BolideGuiUi);
        view(obj);
    }
    crate::object_release(obj);
    handle.layout
}

fn call_view(ui: &mut egui::Ui, view: GuiView) {
    let _ = call_view_with_layout(ui, view, GuiLayout::default());
}

fn top_down_layout(align: egui::Align) -> egui::Layout {
    egui::Layout::top_down(align)
}

fn advance_grid_cell(gui: &mut BolideGuiUi) {
    if gui.layout.in_grid {
        gui.layout.grid_column += 1;
    }
}

fn auto_widget_size(gui: &BolideGuiUi, ui: &egui::Ui, min_height: f32) -> egui::Vec2 {
    if gui.layout.in_grid {
        return egui::vec2(
            gui.layout.grid_cell_width.max(1.0),
            gui.layout.grid_cell_height.max(min_height),
        );
    }
    egui::vec2(ui.available_width().max(1.0), min_height)
}

fn text_units(text: &str) -> f32 {
    text.chars()
        .map(|ch| if ch.is_ascii() { 0.58 } else { 1.0 })
        .sum()
}

fn available_compact_width(ui: &egui::Ui, desired_width: f32) -> f32 {
    desired_width.max(1.0).min(ui.available_width().max(1.0))
}

fn compact_text_width(
    ui: &egui::Ui,
    text: &str,
    font_size: f32,
    padding: f32,
    min_width: f32,
) -> f32 {
    let desired = text_units(text) * font_size + padding;
    available_compact_width(ui, desired.max(min_width))
}

fn text_widget_size(
    gui: &BolideGuiUi,
    ui: &egui::Ui,
    text: &str,
    font_size: f32,
    min_height: f32,
    min_width: f32,
    padding: f32,
) -> egui::Vec2 {
    if gui.layout.in_grid {
        auto_widget_size(gui, ui, min_height)
    } else if gui.layout.compact_width {
        egui::vec2(
            compact_text_width(ui, text, font_size, padding, min_width),
            min_height,
        )
    } else {
        auto_widget_size(gui, ui, min_height)
    }
}

fn compact_control_size(
    gui: &BolideGuiUi,
    ui: &egui::Ui,
    desired_width: f32,
    min_height: f32,
) -> egui::Vec2 {
    if gui.layout.in_grid {
        auto_widget_size(gui, ui, min_height)
    } else if gui.layout.compact_width {
        egui::vec2(available_compact_width(ui, desired_width), min_height)
    } else {
        auto_widget_size(gui, ui, min_height)
    }
}

fn button_font_size(height: f32) -> f32 {
    (height * 0.36).clamp(16.0, 38.0)
}

fn add_text(ui: &mut egui::Ui, size: egui::Vec2, text: egui::RichText, align: egui::Align) {
    ui.allocate_ui_with_layout(size, egui::Layout::top_down(align), |ui| {
        ui.label(text);
    });
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
    fonts
        .font_data
        .insert(name.clone(), FontData::from_owned(font));
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
    with_gui(ui, (), |gui, ui| {
        let size = text_widget_size(gui, ui, &text, 16.0, 24.0, 24.0, 8.0);
        add_text(ui, size, egui::RichText::new(text), gui.layout.align);
        advance_grid_cell(gui);
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_heading(ui: *mut BolideGuiUi, text: *const BolideString) {
    let text = bstr(text);
    with_gui(ui, (), |gui, ui| {
        let size = if gui.layout.compact_width && !gui.layout.in_grid {
            text_widget_size(gui, ui, &text, 36.0, 44.0, 80.0, 12.0)
        } else if gui.layout.in_grid {
            auto_widget_size(gui, ui, 36.0)
        } else {
            egui::vec2(ui.available_width().max(1.0), 44.0)
        };
        add_text(
            ui,
            size,
            egui::RichText::new(text).size(36.0).strong(),
            gui.layout.align,
        );
        advance_grid_cell(gui);
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_small(ui: *mut BolideGuiUi, text: *const BolideString) {
    let text = bstr(text);
    with_gui(ui, (), |gui, ui| {
        let size = text_widget_size(gui, ui, &text, 14.0, 22.0, 20.0, 6.0);
        add_text(
            ui,
            size,
            egui::RichText::new(text).size(14.0),
            gui.layout.align,
        );
        advance_grid_cell(gui);
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_strong(ui: *mut BolideGuiUi, text: *const BolideString) {
    let text = bstr(text);
    with_gui(ui, (), |gui, ui| {
        let size = text_widget_size(gui, ui, &text, 18.0, 28.0, 28.0, 8.0);
        add_text(
            ui,
            size,
            egui::RichText::new(text).size(18.0).strong(),
            gui.layout.align,
        );
        advance_grid_cell(gui);
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
    with_gui(ui, 0, |gui, ui| {
        let size = if gui.layout.compact_width && !gui.layout.in_grid {
            egui::vec2(compact_text_width(ui, &text, 18.0, 34.0, 64.0), 36.0)
        } else {
            auto_widget_size(gui, ui, 36.0)
        };
        let font_size = button_font_size(size.y);
        let text = egui::RichText::new(text).size(font_size).strong();
        let clicked = ui.add_sized(size, egui::Button::new(text)).clicked();
        advance_grid_cell(gui);
        if clicked {
            1
        } else {
            0
        }
    })
}

#[no_mangle]
pub extern "C" fn bolide_gui_selectable(
    ui: *mut BolideGuiUi,
    text: *const BolideString,
    selected: i64,
) -> i64 {
    let text = bstr(text);
    with_gui(ui, 0, |gui, ui| {
        let size = if gui.layout.compact_width && !gui.layout.in_grid {
            egui::vec2(compact_text_width(ui, &text, 16.0, 28.0, 52.0), 30.0)
        } else {
            auto_widget_size(gui, ui, 30.0)
        };
        let text = egui::RichText::new(text).size(15.0);
        let clicked = ui
            .add_sized(size, egui::SelectableLabel::new(selected != 0, text))
            .clicked();
        advance_grid_cell(gui);
        if clicked {
            1
        } else {
            0
        }
    })
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
    with_gui(ui, (), |gui, ui| {
        let size = compact_control_size(gui, ui, 220.0, 32.0);
        ui.add_sized(size, egui::TextEdit::singleline(&mut value).id_source(id));
        advance_grid_cell(gui);
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
    with_gui(ui, (), |gui, ui| {
        let size = compact_control_size(gui, ui, 220.0, 32.0);
        ui.add_sized(
            size,
            egui::TextEdit::singleline(&mut value)
                .id_source(id)
                .password(true),
        );
        advance_grid_cell(gui);
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
    with_gui(ui, (), |gui, ui| {
        let rows = rows.clamp(1, 64) as f32;
        let size = if gui.layout.in_grid {
            auto_widget_size(gui, ui, 32.0)
        } else {
            egui::vec2(ui.available_width().max(1.0), (rows * 24.0).max(56.0))
        };
        ui.add_sized(
            size,
            egui::TextEdit::multiline(&mut value)
                .id_source(id)
                .desired_rows(rows as usize),
        );
        advance_grid_cell(gui);
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
    with_gui(ui, (), |gui, ui| {
        let size = text_widget_size(gui, ui, &label, 16.0, 28.0, 72.0, 32.0);
        ui.add_sized(size, egui::Checkbox::new(&mut checked, label));
        advance_grid_cell(gui);
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
    with_gui(ui, (), |gui, ui| {
        let size = compact_control_size(gui, ui, 220.0, 32.0);
        ui.add_sized(size, egui::Slider::new(&mut value, min..=max).text(label));
        advance_grid_cell(gui);
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
    with_gui(ui, (), |gui, ui| {
        let size = compact_control_size(gui, ui, 160.0, 24.0);
        ui.add_sized(size, egui::ProgressBar::new(fraction).show_percentage());
        advance_grid_cell(gui);
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
    with_gui(ui, (), |parent, ui| {
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(spacing, spacing);
            let mut layout = parent.layout;
            match side.as_str() {
                "left" | "horizontal" | "x" => {
                    layout.compact_width = true;
                    if parent.layout.fill_height {
                        let size = egui::vec2(
                            ui.available_width().max(1.0),
                            ui.available_height().max(1.0),
                        );
                        ui.allocate_ui_with_layout(
                            size,
                            egui::Layout::left_to_right(egui::Align::Min),
                            |ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(spacing, spacing);
                                let _ = call_view_with_layout(ui, child, layout);
                            },
                        );
                    } else {
                        ui.horizontal(|ui| {
                            let _ = call_view_with_layout(ui, child, layout);
                        });
                    }
                }
                "right" => {
                    layout.compact_width = true;
                    layout.align = egui::Align::Max;
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let _ = call_view_with_layout(ui, child, layout);
                    });
                }
                "bottom" => {
                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                        let _ = call_view_with_layout(ui, child, layout);
                    });
                }
                _ => {
                    ui.vertical(|ui| {
                        let _ = call_view_with_layout(ui, child, layout);
                    });
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
    with_gui(ui, (), |parent, ui| {
        let mut layout = parent.layout;
        layout.compact_width = true;
        if parent.layout.fill_height {
            let size = egui::vec2(
                ui.available_width().max(1.0),
                ui.available_height().max(1.0),
            );
            ui.allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Min), |ui| {
                let _ = call_view_with_layout(ui, child, layout);
            });
        } else {
            ui.horizontal(|ui| {
                let _ = call_view_with_layout(ui, child, layout);
            });
        }
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_column(ui: *mut BolideGuiUi, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    with_gui(ui, (), |parent, ui| {
        let layout = parent.layout;
        ui.vertical(|ui| {
            let _ = call_view_with_layout(ui, child, layout);
        });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_group(ui: *mut BolideGuiUi, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    with_gui(ui, (), |parent, ui| {
        let layout = parent.layout;
        ui.group(|ui| {
            let _ = call_view_with_layout(ui, child, layout);
        });
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
    with_gui(ui, (), |parent, ui| {
        let spacing = ui.spacing().item_spacing;
        let available_width = ui.available_width().max(columns as f32);
        let cell_width = ((available_width - spacing.x * (columns.saturating_sub(1) as f32))
            / columns as f32)
            .max(1.0);

        let rows_hint = GUI_GRID_ROWS
            .lock()
            .ok()
            .and_then(|rows| rows.get(&id).copied())
            .unwrap_or(1)
            .max(1);
        let cell_height = if parent.layout.fill_height {
            let available_height = ui.available_height().max(rows_hint as f32);
            ((available_height - spacing.y * (rows_hint.saturating_sub(1) as f32))
                / rows_hint as f32)
                .max(32.0)
        } else {
            36.0
        };

        let mut layout = parent.layout;
        layout.in_grid = true;
        layout.grid_column = 0;
        layout.grid_rows = 0;
        layout.grid_cell_width = cell_width;
        layout.grid_cell_height = cell_height;

        let mut rows_seen = 0usize;
        egui::Grid::new(id.clone())
            .num_columns(columns)
            .striped(striped != 0)
            .min_col_width(cell_width)
            .spacing(spacing)
            .show(ui, |ui| {
                let after = call_view_with_layout(ui, child, layout);
                rows_seen = after.grid_rows + usize::from(after.grid_column > 0);
            });

        if rows_seen > 0 {
            let changed = GUI_GRID_ROWS
                .lock()
                .map(|mut rows| rows.insert(id, rows_seen) != Some(rows_seen))
                .unwrap_or(false);
            if changed && parent.layout.fill_height {
                ui.ctx().request_repaint();
            }
        }
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_end_row(ui: *mut BolideGuiUi) {
    with_gui(ui, (), |gui, ui| {
        if gui.layout.in_grid && gui.layout.grid_column > 0 {
            gui.layout.grid_rows += 1;
            gui.layout.grid_column = 0;
        }
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
    with_gui(ui, (), |parent, ui| {
        let layout = parent.layout;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            if layout.fill_height {
                ui.set_min_height(ui.available_height().max(1.0));
            }
            if !layout.compact_width {
                ui.set_min_width(ui.available_width().max(1.0));
            }
            if !title.is_empty() {
                ui.strong(title);
                ui.separator();
            }
            let _ = call_view_with_layout(ui, child, layout);
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
    with_gui(ui, (), |parent, ui| {
        let mut area = egui::ScrollArea::vertical()
            .id_salt(id)
            .auto_shrink([false, false]);
        let mut layout = parent.layout;
        layout.fill_height = false;
        layout.compact_width = false;
        if max_height > 0 {
            area = area.max_height(max_height as f32);
        } else if parent.layout.fill_height {
            area = area.max_height(ui.available_height().max(1.0));
        }
        area.show(ui, |ui| {
            ui.set_min_width(ui.available_width().max(1.0));
            let _ = call_view_with_layout(ui, child, layout);
        });
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
    with_gui(ui, (), |parent, ui| {
        let layout = parent.layout;
        ui.indent(id, |ui| {
            let _ = call_view_with_layout(ui, child, layout);
        });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_centered(ui: *mut BolideGuiUi, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    with_gui(ui, (), |parent, ui| {
        let mut layout = parent.layout;
        layout.align = egui::Align::Center;
        ui.vertical_centered(|ui| {
            let _ = call_view_with_layout(ui, child, layout);
        });
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
    with_gui(ui, (), |parent, ui| {
        let align = match mode.as_str() {
            "center" | "middle" => egui::Align::Center,
            "right" | "bottom" | "end" => egui::Align::Max,
            _ => egui::Align::Min,
        };
        let mut layout = parent.layout;
        layout.align = align;
        ui.with_layout(egui::Layout::top_down(align), |ui| {
            let _ = call_view_with_layout(ui, child, layout);
        });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_pad(ui: *mut BolideGuiUi, x: i64, y: i64, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    let x = x.max(0) as f32;
    let y = y.max(0) as f32;
    with_gui(ui, (), |parent, ui| {
        let layout = parent.layout;
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(x, y))
            .show(ui, |ui| {
                let _ = call_view_with_layout(ui, child, layout);
            });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_width(ui: *mut BolideGuiUi, points: i64, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    let points = points.max(1) as f32;
    with_gui(ui, (), |parent, ui| {
        let mut layout = parent.layout;
        layout.compact_width = false;
        if parent.layout.fill_height {
            let size = egui::vec2(points, ui.available_height().max(1.0));
            ui.allocate_ui_with_layout(size, top_down_layout(layout.align), |ui| {
                ui.set_min_width(points);
                ui.set_max_width(points);
                let _ = call_view_with_layout(ui, child, layout);
            });
        } else {
            ui.scope(|ui| {
                ui.set_min_width(points);
                ui.set_max_width(points);
                let _ = call_view_with_layout(ui, child, layout);
            });
        }
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_height(ui: *mut BolideGuiUi, points: i64, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    let points = points.max(1) as f32;
    with_gui(ui, (), |parent, ui| {
        let mut layout = parent.layout;
        layout.fill_height = false;
        layout.compact_width = false;
        let size = egui::vec2(ui.available_width().max(1.0), points);
        ui.allocate_ui_with_layout(size, top_down_layout(layout.align), |ui| {
            ui.set_min_height(points);
            ui.set_max_height(points);
            let _ = call_view_with_layout(ui, child, layout);
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
    with_gui(ui, (), |parent, ui| {
        let mut layout = parent.layout;
        layout.fill_height = false;
        layout.compact_width = false;
        ui.allocate_ui_with_layout(size, top_down_layout(layout.align), |ui| {
            ui.set_min_size(size);
            ui.set_max_size(size);
            let _ = call_view_with_layout(ui, child, layout);
        });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_fill_width(ui: *mut BolideGuiUi, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    with_gui(ui, (), |parent, ui| {
        let width = ui.available_width().max(1.0);
        let mut layout = parent.layout;
        layout.compact_width = false;
        if parent.layout.fill_height {
            let size = egui::vec2(width, ui.available_height().max(1.0));
            ui.allocate_ui_with_layout(size, top_down_layout(layout.align), |ui| {
                ui.set_min_width(width);
                let _ = call_view_with_layout(ui, child, layout);
            });
        } else {
            ui.scope(|ui| {
                ui.set_min_width(width);
                let _ = call_view_with_layout(ui, child, layout);
            });
        }
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_fill_height(ui: *mut BolideGuiUi, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    with_gui(ui, (), |parent, ui| {
        let height = ui.available_height().max(1.0);
        let mut layout = parent.layout;
        layout.fill_height = true;
        let size = egui::vec2(ui.available_width().max(1.0), height);
        ui.allocate_ui_with_layout(size, top_down_layout(layout.align), |ui| {
            ui.set_min_height(height);
            let _ = call_view_with_layout(ui, child, layout);
        });
    });
}

#[no_mangle]
pub extern "C" fn bolide_gui_fill(ui: *mut BolideGuiUi, child: Option<GuiView>) {
    let Some(child) = child else {
        return;
    };
    with_gui(ui, (), |parent, ui| {
        let size = egui::vec2(
            ui.available_width().max(1.0),
            ui.available_height().max(1.0),
        );
        let mut layout = parent.layout;
        layout.fill_height = true;
        layout.compact_width = false;
        ui.allocate_ui_with_layout(size, top_down_layout(layout.align), |ui| {
            ui.set_min_size(size);
            let _ = call_view_with_layout(ui, child, layout);
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
    with_gui(ui, (), |parent, ui| {
        let mut layout = parent.layout;
        layout.fill_height = false;
        layout.compact_width = false;
        let origin = ui.min_rect().min;
        let rect = egui::Rect::from_min_size(origin + egui::vec2(x, y), egui::vec2(width, height));
        ui.allocate_space(egui::vec2(x + width, y + height));
        #[allow(deprecated)]
        {
            ui.allocate_ui_at_rect(rect, |ui| {
                ui.set_min_size(egui::vec2(width, height));
                let _ = call_view_with_layout(ui, child, layout);
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
    with_gui(ui, (), |parent, ui| {
        let layout = parent.layout;
        ui.collapsing(title, |ui| {
            let _ = call_view_with_layout(ui, child, layout);
        });
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
