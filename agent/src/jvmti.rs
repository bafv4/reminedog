//! LWJGL からウィンドウのライブラリと OpenGL への呼び出しを、JVMTI で見る。何も書き換えない。
//!
//! JVM は、ネイティブのメソッドを結び付けるたびに知らせ（NativeMethodBind）、エージェントが
//! 代わりに別の関数を結び付けられるようにする。LWJGL は、どの版でも、GLFW と SDL の関数を
//! JNI のネイティブで引く（Windows は `WinBase.nGetProcAddress`、Linux と macOS は
//! `DynamicLinkLoader.ndlsym`）：その代わりに、エージェントは LWJGL に関数のフック
//! （[`crate::library::hook`]、[`crate::hook::Hook`]）を渡し、フックが本物を呼ぶ。LWJGL は、
//! どの呼び方でもフックを呼ぶ：LWJGL 3.3 までは JNI のネイティブ（Java 8 の 3.2 では
//! `JavaCritical_` の入口）、Java 25 の LWJGL 3.4 では FFM の downcall。LWJGL のほかの
//! ネイティブ（ズームの GL の関数）は、結び付けるときに差し替える（[`crate::library::native`]）。
//!
//! 見えるのは `Agent_OnLoad` の後に結び付くネイティブだけで、LWJGL のものはすべてそう。

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::library;

const JNI_OK: i32 = 0;
const JVMTI_VERSION_1_0: i32 = 0x3001_0000;
const JVMTI_ENABLE: i32 = 1;
const JVMTI_EVENT_NATIVE_METHOD_BIND: i32 = 67;
const JVMTI_PHASE_LIVE: i32 = 4;
const JVMTI_PHASE_START: i32 = 6;
/// `can_generate_native_method_bind_events`：`jvmtiCapabilities` の 31 番目のフラグ。
const CAN_GENERATE_NATIVE_METHOD_BIND_EVENTS: u32 = 1 << 30;

// jvmtiInterface_1 の枠（jvmti.h は 1 から数える）。
const SET_EVENT_NOTIFICATION_MODE: usize = 2 - 1;
const DEALLOCATE: usize = 47 - 1;
const GET_CLASS_SIGNATURE: usize = 48 - 1;
const GET_METHOD_NAME: usize = 64 - 1;
const GET_METHOD_DECLARING_CLASS: usize = 65 - 1;
const SET_EVENT_CALLBACKS: usize = 122 - 1;
const GET_PHASE: usize = 133 - 1;
const ADD_CAPABILITIES: usize = 142 - 1;
/// JNIInvokeInterface の GetEnv の枠。
const GET_ENV: usize = 6;

type Env = *mut *const *const c_void;
type Ptr = *mut c_void;

type NativeMethodBindFn = unsafe extern "system" fn(
    jvmti: Env,
    jni: Ptr,
    thread: Ptr,
    method: Ptr,
    address: Ptr,
    new_address: *mut Ptr,
);
/// `WinBase.nGetProcAddress(module, name)`（LWJGL 3.2 と 3.3）と、
/// `DynamicLinkLoader.ndlsym(handle, name)`。
type GetProcAddressFn = unsafe extern "system" fn(Ptr, Ptr, i64, i64) -> i64;
/// `WinBase.nGetProcAddress(lastError, module, name)`（LWJGL 3.4）。
type GetProcAddressLastErrorFn = unsafe extern "system" fn(Ptr, Ptr, i64, i64, i64) -> i64;

/// NativeMethodBind までの jvmtiEventCallbacks（イベント 67。最初の VMInit は 50）。JVM は
/// 渡された大きさだけを写し、残りは空にする。
#[repr(C)]
struct EventCallbacks {
    before: [Ptr; 17],
    native_method_bind: NativeMethodBindFn,
}

/// LWJGL が関数を引くのに使う JVM 自身のネイティブ（シグネチャごと）：LWJGL 3.3 までの
/// どの OS と、Linux・macOS では (handle, name)、LWJGL 3.4 の Windows では
/// (last error, handle, name)。
static GET_PROC_ADDRESS: AtomicUsize = AtomicUsize::new(0);
static GET_PROC_ADDRESS_LAST_ERROR: AtomicUsize = AtomicUsize::new(0);

/// JVM に、ネイティブのメソッドを結び付けるたびに知らせてもらう。
///
/// # Safety
/// `vm` は、JVM が `Agent_OnLoad` に渡した `JavaVM*`。
pub unsafe fn start(vm: *mut c_void) -> Result<(), String> {
    if vm.is_null() {
        return Err("no JavaVM".into());
    }
    let vm = vm as *mut *const Ptr;
    let mut env: Env = std::ptr::null_mut();
    // SAFETY: JavaVM の関数表の GetEnv（C のシグネチャで）。
    let status = unsafe {
        let get_env: unsafe extern "system" fn(*mut *const Ptr, *mut Env, i32) -> i32 =
            std::mem::transmute(*(*vm).add(GET_ENV));
        get_env(vm, &mut env, JVMTI_VERSION_1_0)
    };
    if status != JNI_OK || env.is_null() {
        return Err(format!("GetEnv(JVMTI) failed: {status}"));
    }
    // jvmtiCapabilities は 128 ビットのフラグ。
    let capabilities = [CAN_GENERATE_NATIVE_METHOD_BIND_EVENTS, 0, 0, 0];
    let callbacks = EventCallbacks {
        before: [std::ptr::null_mut(); 17],
        native_method_bind,
    };
    // SAFETY: jvmti.h のシグネチャの JVMTI の関数を、いま得た env で呼ぶ。
    unsafe {
        let add_capabilities: unsafe extern "system" fn(Env, *const u32) -> i32 =
            function(env, ADD_CAPABILITIES);
        check(
            "AddCapabilities",
            add_capabilities(env, capabilities.as_ptr()),
        )?;
        let set_callbacks: unsafe extern "system" fn(Env, *const EventCallbacks, i32) -> i32 =
            function(env, SET_EVENT_CALLBACKS);
        check(
            "SetEventCallbacks",
            set_callbacks(env, &callbacks, size_of::<EventCallbacks>() as i32),
        )?;
        let set_mode: unsafe extern "system" fn(Env, i32, i32, Ptr) -> i32 =
            function(env, SET_EVENT_NOTIFICATION_MODE);
        check(
            "SetEventNotificationMode",
            set_mode(
                env,
                JVMTI_ENABLE,
                JVMTI_EVENT_NATIVE_METHOD_BIND,
                std::ptr::null_mut(),
            ),
        )
    }
}

fn check(what: &str, error: i32) -> Result<(), String> {
    if error == 0 {
        Ok(())
    } else {
        Err(format!("{what} failed: JVMTI error {error}"))
    }
}

/// # Safety
/// `slot` は、型が `F` の jvmtiInterface_1 の関数を持つ。
unsafe fn function<F: Copy>(env: Env, slot: usize) -> F {
    // SAFETY: 表は関数ポインタを持つ。
    unsafe { std::mem::transmute_copy(&*(*env).add(slot)) }
}

/// JVM が確保した文字列。読んだ後に解放する。
///
/// # Safety
/// `s` は null か、`env` で JVMTI が返した文字列。
unsafe fn take_string(env: Env, s: *mut c_char) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // SAFETY: JVM の modified UTF-8 で NUL で終わる。JVMTI が渡したものは Deallocate で解放する。
    unsafe {
        let text = CStr::from_ptr(s).to_string_lossy().into_owned();
        let deallocate: unsafe extern "system" fn(Env, *mut c_char) -> i32 =
            function(env, DEALLOCATE);
        deallocate(env, s);
        Some(text)
    }
}

/// `method` のクラスのシグネチャ（`Lorg/lwjgl/...;`）、名前、シグネチャ。VM が始まった後だけ
/// （それより前に結び付くネイティブは JVM 自身のもの）。
///
/// # Safety
/// NativeMethodBind のイベントの env と method。
unsafe fn describe(env: Env, method: Ptr) -> Option<(String, String, String)> {
    // SAFETY: jvmti.h のシグネチャの JVMTI の関数。返った文字列は take_string が解放する。
    unsafe {
        let mut phase = 0;
        let get_phase: unsafe extern "system" fn(Env, *mut i32) -> i32 = function(env, GET_PHASE);
        if get_phase(env, &mut phase) != 0
            || (phase != JVMTI_PHASE_LIVE && phase != JVMTI_PHASE_START)
        {
            return None;
        }
        let mut class: Ptr = std::ptr::null_mut();
        let declaring_class: unsafe extern "system" fn(Env, Ptr, *mut Ptr) -> i32 =
            function(env, GET_METHOD_DECLARING_CLASS);
        if declaring_class(env, method, &mut class) != 0 {
            return None;
        }
        let mut class_signature = std::ptr::null_mut();
        let get_class_signature: unsafe extern "system" fn(
            Env,
            Ptr,
            *mut *mut c_char,
            *mut *mut c_char,
        ) -> i32 = function(env, GET_CLASS_SIGNATURE);
        if get_class_signature(env, class, &mut class_signature, std::ptr::null_mut()) != 0 {
            return None;
        }
        let class = take_string(env, class_signature)?;
        if !class.starts_with("Lorg/lwjgl/") {
            return None;
        }
        let (mut name, mut signature) = (std::ptr::null_mut(), std::ptr::null_mut());
        let get_method_name: unsafe extern "system" fn(
            Env,
            Ptr,
            *mut *mut c_char,
            *mut *mut c_char,
            *mut *mut c_char,
        ) -> i32 = function(env, GET_METHOD_NAME);
        if get_method_name(env, method, &mut name, &mut signature, std::ptr::null_mut()) != 0 {
            return None;
        }
        Some((class, take_string(env, name)?, take_string(env, signature)?))
    }
}

unsafe extern "system" fn native_method_bind(
    env: Env,
    _jni: Ptr,
    _thread: Ptr,
    method: Ptr,
    address: Ptr,
    new_address: *mut Ptr,
) {
    crate::catch("NativeMethodBind", || {
        // SAFETY: イベントの env と method。
        let Some((class, name, signature)) = (unsafe { describe(env, method) }) else {
            return;
        };
        let (original, ours) = match (class.as_str(), name.as_str(), signature.as_str()) {
            ("Lorg/lwjgl/system/windows/WinBase;", "nGetProcAddress", "(JJ)J")
            | (
                "Lorg/lwjgl/system/linux/DynamicLinkLoader;"
                | "Lorg/lwjgl/system/macosx/DynamicLinkLoader;",
                "ndlsym",
                "(JJ)J",
            ) => (
                &GET_PROC_ADDRESS,
                get_proc_address as GetProcAddressFn as Ptr,
            ),
            ("Lorg/lwjgl/system/windows/WinBase;", "nGetProcAddress", "(JJJ)J") => (
                &GET_PROC_ADDRESS_LAST_ERROR,
                get_proc_address_last_error as GetProcAddressLastErrorFn as Ptr,
            ),
            _ => {
                if let Some(ours) = library::native(&class, &name, &signature, address) {
                    // SAFETY: JVM は、イベントがここに残したものを結び付ける。
                    unsafe { *new_address = ours.cast_mut() };
                }
                return;
            }
        };
        if address.is_null() || address == ours {
            return;
        }
        original.store(address as usize, Ordering::Release);
        // SAFETY: JVM は、イベントがここに残したものを結び付ける。
        unsafe { *new_address = ours };
        log::info!("jvmti: LWJGL's {name}{signature} goes through the agent");
    });
}

/// LWJGL 3.2 と 3.3 の `WinBase.nGetProcAddress`、および `DynamicLinkLoader.ndlsym`
/// （Linux、macOS）の代わり。
unsafe extern "system" fn get_proc_address(env: Ptr, class: Ptr, module: i64, name: i64) -> i64 {
    // SAFETY: JVM 自身のネイティブを覚えた後にだけ、そのシグネチャで結び付く。
    let address = unsafe {
        let original: GetProcAddressFn =
            std::mem::transmute(GET_PROC_ADDRESS.load(Ordering::Acquire));
        original(env, class, module, name)
    };
    // SAFETY: `name` は、LWJGL が引いた NUL で終わる名前。
    unsafe { handed(module, name, address) }
}

/// LWJGL 3.4 の `WinBase.nGetProcAddress` の代わり。
unsafe extern "system" fn get_proc_address_last_error(
    env: Ptr,
    class: Ptr,
    last_error: i64,
    module: i64,
    name: i64,
) -> i64 {
    // SAFETY: 上と同じ。
    let address = unsafe {
        let original: GetProcAddressLastErrorFn =
            std::mem::transmute(GET_PROC_ADDRESS_LAST_ERROR.load(Ordering::Acquire));
        original(env, class, last_error, module, name)
    };
    // SAFETY: 上と同じ。
    unsafe { handed(module, name, address) }
}

/// LWJGL が `module` で引いた関数の代わりに渡すもの：フックする関数ならフック。
///
/// # Safety
/// `name` は 0 か NUL で終わる文字列。`address` は `module` のその関数。
unsafe fn handed(module: i64, name: i64, address: i64) -> i64 {
    if name == 0 || address == 0 {
        return address;
    }
    crate::catch("LWJGL's lookup", || {
        // SAFETY: 呼び出し側が保証する。
        let name = unsafe { CStr::from_ptr(name as *const c_char) };
        let Some(hook) = library::hook(module as usize, name) else {
            return Some(address);
        };
        // SAFETY: フックの名前の関数で、置き場所の型。
        unsafe { hook.original.set_looked_up(address as *const c_void) };
        log::info!(
            "jvmti: LWJGL looked up {}; handed the agent's hook",
            name.to_string_lossy()
        );
        Some(hook.hook as i64)
    })
    .flatten()
    .unwrap_or(address)
}
