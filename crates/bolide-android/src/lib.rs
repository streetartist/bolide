//! JNI bridge used by the Android IDE.
//!
//! The bridge intentionally reuses the parser, Cranelift JIT, and runtime
//! directly.  Android-specific UI and file management stay in the app module.

#![cfg(target_os = "android")]

use bolide_compiler::JitCompiler;
use bolide_parser::parse_source_with_diagnostics;
use bolide_runtime::{
    android_gui_activity_destroyed, android_gui_close, android_gui_main, android_gui_set_insets,
    clear_android_gui_hooks, clear_io_hooks, set_android_gui_hooks, set_io_hooks,
    AndroidGuiLaunchHook, AndroidGuiReturnHook, InputHook, OutputHook,
};
use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::sys::jstring;
use jni::{JNIEnv, JavaVM};
use std::cell::RefCell;
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};

#[no_mangle]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    android_gui_main(app);
}

struct ReplState {
    compiler: JitCompiler,
    input_counter: usize,
    base_dir: String,
}

impl ReplState {
    fn new(base_dir: &str) -> Self {
        let mut compiler = JitCompiler::new();
        compiler.set_repl_mode(true);
        compiler.set_base_dir(base_dir);
        Self {
            compiler,
            input_counter: 0,
            base_dir: base_dir.to_string(),
        }
    }
}

thread_local! {
    // Java dispatches all native work through one single-thread executor, so a
    // thread-local state avoids imposing Send on Cranelift's JIT module.
    static REPL: RefCell<Option<ReplState>> = const { RefCell::new(None) };
}

fn run_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct GuiActivityBridge {
    vm: Arc<JavaVM>,
    activity: GlobalRef,
}

fn gui_activity_bridge() -> &'static Mutex<Option<GuiActivityBridge>> {
    static BRIDGE: OnceLock<Mutex<Option<GuiActivityBridge>>> = OnceLock::new();
    BRIDGE.get_or_init(|| Mutex::new(None))
}

struct HookGuard;

impl Drop for HookGuard {
    fn drop(&mut self) {
        clear_android_gui_hooks();
        clear_io_hooks();
    }
}

fn call_output(vm: &JavaVM, bridge: &GlobalRef, text: &str) -> jni::errors::Result<()> {
    let mut env = vm.attach_current_thread()?;
    let text = env.new_string(text)?;
    let text_obj = JObject::from(text);
    env.call_method(
        bridge.as_obj(),
        "onNativeOutput",
        "(Ljava/lang/String;)V",
        &[JValue::Object(&text_obj)],
    )?;
    Ok(())
}

fn call_input(vm: &JavaVM, bridge: &GlobalRef, prompt: &str) -> jni::errors::Result<String> {
    let mut env = vm.attach_current_thread()?;
    let prompt = env.new_string(prompt)?;
    let prompt_obj = JObject::from(prompt);
    let result = env
        .call_method(
            bridge.as_obj(),
            "onNativeInput",
            "(Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(&prompt_obj)],
        )?
        .l()?;
    if result.is_null() {
        return Ok(String::new());
    }
    let result = JString::from(result);
    let result: String = env.get_string(&result)?.into();
    Ok(result)
}

fn call_gui_launch(vm: &JavaVM, bridge: &GlobalRef, title: &str) -> jni::errors::Result<bool> {
    let mut env = vm.attach_current_thread()?;
    let title = env.new_string(title)?;
    let title_obj = JObject::from(title);
    env.call_method(
        bridge.as_obj(),
        "onNativeGuiRequest",
        "(Ljava/lang/String;)Z",
        &[JValue::Object(&title_obj)],
    )?
    .z()
}

fn call_gui_closed(vm: &JavaVM, bridge: &GlobalRef) -> jni::errors::Result<()> {
    let mut env = vm.attach_current_thread()?;
    env.call_method(bridge.as_obj(), "onNativeGuiClosed", "()V", &[])?;
    Ok(())
}

fn call_gui_activity_return() -> jni::errors::Result<()> {
    let (vm, activity) = {
        let bridge = gui_activity_bridge()
            .lock()
            .expect("GUI Activity bridge lock poisoned");
        let bridge = bridge
            .as_ref()
            .ok_or(jni::errors::Error::NullPtr("BolideGuiActivity"))?;
        (bridge.vm.clone(), bridge.activity.clone())
    };
    let mut env = vm.attach_current_thread()?;
    env.call_method(activity.as_obj(), "returnToIdeFromNative", "()V", &[])?;
    Ok(())
}

fn install_hooks(env: &mut JNIEnv, bridge: JObject) -> jni::errors::Result<HookGuard> {
    let vm = Arc::new(env.get_java_vm()?);
    let bridge = Arc::new(env.new_global_ref(bridge)?);

    let output_vm = vm.clone();
    let output_bridge = bridge.clone();
    let output: OutputHook = Arc::new(move |text| {
        let _ = call_output(&output_vm, &output_bridge, text);
    });

    let input_vm = vm.clone();
    let input_bridge = bridge.clone();
    let input: InputHook =
        Arc::new(move |prompt| call_input(&input_vm, &input_bridge, prompt).unwrap_or_default());

    let launch_vm = vm.clone();
    let launch_bridge = bridge.clone();
    let launch: AndroidGuiLaunchHook =
        Arc::new(move |title| call_gui_launch(&launch_vm, &launch_bridge, title).unwrap_or(false));

    let close_vm = vm;
    let close_bridge = bridge;
    let return_to_ide: AndroidGuiReturnHook = Arc::new(move || {
        if call_gui_activity_return().is_err() {
            let _ = call_gui_closed(&close_vm, &close_bridge);
        }
    });

    set_io_hooks(Some(output), Some(input));
    set_android_gui_hooks(Some(launch), Some(return_to_ide));
    Ok(HookGuard)
}

fn configure_bolide_home(home: &str) -> Result<(), String> {
    let std_dir = Path::new(home).join("std");
    if !std_dir.is_dir() {
        return Err(format!("标准库目录不存在: {}", std_dir.display()));
    }
    std::env::set_var("BOLIDE_HOME", home);
    Ok(())
}

fn run_file(path: &str, bolide_home: &str) -> Result<String, String> {
    configure_bolide_home(bolide_home)?;
    let source = fs::read_to_string(path).map_err(|e| format!("读取文件失败: {e}"))?;
    let ast =
        parse_source_with_diagnostics(&source).map_err(|e| format!("语法错误: {}", e.message))?;
    let mut compiler = JitCompiler::new();
    if let Some(parent) = Path::new(path).parent() {
        compiler.set_base_dir(&parent.to_string_lossy());
    }
    let main_ptr = compiler
        .compile(&ast)
        .map_err(|e| format!("编译错误: {e}"))?;
    let main_fn: fn() -> i64 = unsafe { std::mem::transmute(main_ptr) };
    let result = main_fn();
    Ok(format!("运行结束，返回值 {result}"))
}

fn eval_repl(code: &str, base_dir: &str, bolide_home: &str) -> Result<String, String> {
    configure_bolide_home(bolide_home)?;
    let ast =
        parse_source_with_diagnostics(code).map_err(|e| format!("语法错误: {}", e.message))?;
    REPL.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot
            .as_ref()
            .map(|state| state.base_dir != base_dir)
            .unwrap_or(true)
        {
            *slot = Some(ReplState::new(base_dir));
        }
        let state = slot.as_mut().expect("REPL state initialized");
        let main_name = format!("__android_repl_{}", state.input_counter);
        state.input_counter += 1;
        let main_ptr = state
            .compiler
            .compile_with_main(&ast, &main_name)
            .map_err(|e| format!("编译错误: {e}"))?;
        let main_fn: fn() -> i64 = unsafe { std::mem::transmute(main_ptr) };
        let result = main_fn();
        if result == 0 {
            Ok(String::new())
        } else {
            Ok(result.to_string())
        }
    })
}

fn to_java_string(env: &mut JNIEnv, value: String) -> jstring {
    env.new_string(value)
        .map(JString::into_raw)
        .unwrap_or(ptr::null_mut())
}

#[no_mangle]
pub extern "system" fn Java_dev_bolide_ide_BolideNative_runFile(
    mut env: JNIEnv,
    _class: JClass,
    path: JString,
    bolide_home: JString,
    bridge: JObject,
) -> jstring {
    let path: String = match env.get_string(&path) {
        Ok(value) => value.into(),
        Err(err) => return to_java_string(&mut env, format!("JNI 错误: {err}")),
    };
    let bolide_home: String = match env.get_string(&bolide_home) {
        Ok(value) => value.into(),
        Err(err) => return to_java_string(&mut env, format!("JNI 错误: {err}")),
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _run = run_lock().lock().map_err(|_| "运行锁已损坏".to_string())?;
        let _hooks = install_hooks(&mut env, bridge).map_err(|e| format!("JNI 错误: {e}"))?;
        run_file(&path, &bolide_home)
    }))
    .unwrap_or_else(|_| Err("Bolide 运行时发生 panic".to_string()));

    to_java_string(
        &mut env,
        result.unwrap_or_else(|error| format!("错误：{error}")),
    )
}

#[no_mangle]
pub extern "system" fn Java_dev_bolide_ide_BolideNative_evalRepl(
    mut env: JNIEnv,
    _class: JClass,
    code: JString,
    base_dir: JString,
    bolide_home: JString,
    bridge: JObject,
) -> jstring {
    let code: String = match env.get_string(&code) {
        Ok(value) => value.into(),
        Err(err) => return to_java_string(&mut env, format!("JNI 错误: {err}")),
    };
    let base_dir: String = match env.get_string(&base_dir) {
        Ok(value) => value.into(),
        Err(err) => return to_java_string(&mut env, format!("JNI 错误: {err}")),
    };
    let bolide_home: String = match env.get_string(&bolide_home) {
        Ok(value) => value.into(),
        Err(err) => return to_java_string(&mut env, format!("JNI 错误: {err}")),
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        let _run = run_lock().lock().map_err(|_| "运行锁已损坏".to_string())?;
        let _hooks = install_hooks(&mut env, bridge).map_err(|e| format!("JNI 错误: {e}"))?;
        eval_repl(&code, &base_dir, &bolide_home)
    }))
    .unwrap_or_else(|_| Err("Bolide REPL 发生 panic".to_string()));

    to_java_string(
        &mut env,
        result.unwrap_or_else(|error| format!("错误：{error}")),
    )
}

#[no_mangle]
pub extern "system" fn Java_dev_bolide_ide_BolideNative_resetRepl(_env: JNIEnv, _class: JClass) {
    REPL.with(|slot| *slot.borrow_mut() = None);
}

#[no_mangle]
pub extern "system" fn Java_dev_bolide_ide_BolideNative_closeGui(
    _env: JNIEnv,
    _class: JClass,
) -> jni::sys::jboolean {
    android_gui_close() as jni::sys::jboolean
}

#[no_mangle]
pub extern "system" fn Java_dev_bolide_ide_BolideNative_registerGuiActivity(
    env: JNIEnv,
    _class: JClass,
    activity: JObject,
) {
    let bridge = (|| -> jni::errors::Result<GuiActivityBridge> {
        Ok(GuiActivityBridge {
            vm: Arc::new(env.get_java_vm()?),
            activity: env.new_global_ref(activity)?,
        })
    })();
    if let Ok(bridge) = bridge {
        *gui_activity_bridge()
            .lock()
            .expect("GUI Activity bridge lock poisoned") = Some(bridge);
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_bolide_ide_BolideNative_setGuiInsets(
    _env: JNIEnv,
    _class: JClass,
    left: jni::sys::jint,
    top: jni::sys::jint,
    right: jni::sys::jint,
    bottom: jni::sys::jint,
) {
    android_gui_set_insets(left, top, right, bottom);
}

#[no_mangle]
pub extern "system" fn Java_dev_bolide_ide_BolideNative_guiActivityDestroyed(
    _env: JNIEnv,
    _class: JClass,
) {
    android_gui_set_insets(0, 0, 0, 0);
    *gui_activity_bridge()
        .lock()
        .expect("GUI Activity bridge lock poisoned") = None;
    android_gui_activity_destroyed();
}
