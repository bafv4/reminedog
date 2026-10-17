//! Sees LWJGL's calls into the window library and OpenGL through JVMTI, patching nothing.
//!
//! The JVM reports each native method as it binds it (NativeMethodBind) and lets the agent
//! bind another function in its place. LWJGL looks up every GLFW and SDL function with
//! `WinBase.nGetProcAddress`, a JNI native in every version: in its place, the agent hands
//! LWJGL the hook for the functions of the hooked window library ([`crate::hook::Hook`]),
//! which calls the real one. LWJGL then calls the hook whichever way it calls: JNI natives up
//! to LWJGL 3.3 (`JavaCritical_` entries on Java 8 with 3.2), FFM downcalls in LWJGL 3.4 on
//! Java 25.
//!
//! Nothing is patched, so other systems can do the same (with `DynamicLinkLoader.ndlsym`).
//! Only natives bound after `Agent_OnLoad` are seen, which are all of LWJGL's.

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::HMODULE;

use crate::ffi;
use crate::hook::Hook;

const JNI_OK: i32 = 0;
const JVMTI_VERSION_1_0: i32 = 0x3001_0000;
const JVMTI_ENABLE: i32 = 1;
const JVMTI_EVENT_NATIVE_METHOD_BIND: i32 = 67;
const JVMTI_PHASE_LIVE: i32 = 4;
const JVMTI_PHASE_START: i32 = 6;
/// `can_generate_native_method_bind_events`, the 31st flag of `jvmtiCapabilities`.
const CAN_GENERATE_NATIVE_METHOD_BIND_EVENTS: u32 = 1 << 30;

// Slots of jvmtiInterface_1 (jvmti.h numbers them from 1).
const SET_EVENT_NOTIFICATION_MODE: usize = 2 - 1;
const DEALLOCATE: usize = 47 - 1;
const GET_CLASS_SIGNATURE: usize = 48 - 1;
const GET_METHOD_NAME: usize = 64 - 1;
const GET_METHOD_DECLARING_CLASS: usize = 65 - 1;
const SET_EVENT_CALLBACKS: usize = 122 - 1;
const GET_PHASE: usize = 133 - 1;
const ADD_CAPABILITIES: usize = 142 - 1;
/// Slot of GetEnv in JNIInvokeInterface.
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
/// `WinBase.nGetProcAddress(module, name)` (LWJGL 3.2 and 3.3).
type GetProcAddressFn = unsafe extern "system" fn(Ptr, Ptr, i64, i64) -> i64;
/// `WinBase.nGetProcAddress(lastError, module, name)` (LWJGL 3.4).
type GetProcAddressLastErrorFn = unsafe extern "system" fn(Ptr, Ptr, i64, i64, i64) -> i64;

/// jvmtiEventCallbacks up to NativeMethodBind (event 67; the first, VMInit, is 50). The JVM
/// copies the size it is given and clears the rest.
#[repr(C)]
struct EventCallbacks {
    before: [Ptr; 17],
    native_method_bind: NativeMethodBindFn,
}

/// The JVM's own `nGetProcAddress`, by signature.
static GET_PROC_ADDRESS: AtomicUsize = AtomicUsize::new(0);
static GET_PROC_ADDRESS_LAST_ERROR: AtomicUsize = AtomicUsize::new(0);

/// Whether LWJGL's lookups go through the agent: functions LWJGL looks up from now on are
/// handed their hooks.
pub fn handing_out() -> bool {
    GET_PROC_ADDRESS.load(Ordering::Acquire) != 0
        || GET_PROC_ADDRESS_LAST_ERROR.load(Ordering::Acquire) != 0
}

/// The hook of `name` if `module` is the hooked window library (the first GLFW or SDL3 that
/// LWJGL looks a function up in).
fn hook_for(module: usize, name: &[u8]) -> Option<&'static Hook> {
    crate::library::identify(module as HMODULE);
    let hooks: &[&[Hook]] = if crate::glfw::is_hooked(module) {
        &[crate::glfw::hooks(), crate::glfw_input::hooks()]
    } else if crate::sdl::is_hooked(module) {
        &[crate::sdl::hooks(), crate::sdl_input::hooks()]
    } else {
        return None;
    };
    hooks
        .iter()
        .flat_map(|hooks| hooks.iter())
        .find(|hook| hook.name.to_bytes() == name)
}

/// Has the JVM report native methods as it binds them.
///
/// # Safety
/// `vm` is the `JavaVM*` the JVM passed `Agent_OnLoad`.
pub unsafe fn start(vm: *mut c_void) -> Result<(), String> {
    if vm.is_null() {
        return Err("no JavaVM".into());
    }
    let vm = vm as *mut *const Ptr;
    let mut env: Env = std::ptr::null_mut();
    // SAFETY: GetEnv of the JavaVM's function table, with its C signature.
    let status = unsafe {
        let get_env: unsafe extern "system" fn(*mut *const Ptr, *mut Env, i32) -> i32 =
            std::mem::transmute(*(*vm).add(GET_ENV));
        get_env(vm, &mut env, JVMTI_VERSION_1_0)
    };
    if status != JNI_OK || env.is_null() {
        return Err(format!("GetEnv(JVMTI) failed: {status}"));
    }
    // jvmtiCapabilities is 128 bits of flags.
    let capabilities = [CAN_GENERATE_NATIVE_METHOD_BIND_EVENTS, 0, 0, 0];
    let callbacks = EventCallbacks {
        before: [std::ptr::null_mut(); 17],
        native_method_bind,
    };
    // SAFETY: JVMTI functions with their signatures from jvmti.h, on the env just obtained.
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
/// `slot` holds a function of jvmtiInterface_1 whose type is `F`.
unsafe fn function<F: Copy>(env: Env, slot: usize) -> F {
    // SAFETY: the table holds function pointers.
    unsafe { std::mem::transmute_copy(&*(*env).add(slot)) }
}

/// A string the JVM allocated, released after reading.
///
/// # Safety
/// `s` is null or a string JVMTI returned on `env`.
unsafe fn take_string(env: Env, s: *mut c_char) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // SAFETY: the JVM's modified UTF-8, NUL-terminated; Deallocate frees what JVMTI gave.
    unsafe {
        let text = CStr::from_ptr(s).to_string_lossy().into_owned();
        let deallocate: unsafe extern "system" fn(Env, *mut c_char) -> i32 =
            function(env, DEALLOCATE);
        deallocate(env, s);
        Some(text)
    }
}

/// The class signature (`Lorg/lwjgl/...;`), name and signature of `method`; only once the VM
/// has started (natives bound earlier are the JVM's own).
///
/// # Safety
/// The env and method of a NativeMethodBind event.
unsafe fn describe(env: Env, method: Ptr) -> Option<(String, String, String)> {
    // SAFETY: JVMTI functions with their signatures from jvmti.h; the strings they return
    // are released by take_string.
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
    ffi::catch("NativeMethodBind", || {
        // SAFETY: the event's env and method.
        let Some((class, name, signature)) = (unsafe { describe(env, method) }) else {
            return;
        };
        if class.starts_with("Lorg/lwjgl/opengl/") {
            if let Some(ours) = crate::tall::wrap_native(&name, &signature, address) {
                // SAFETY: the JVM binds what the event leaves here.
                unsafe { *new_address = ours.cast_mut() };
            }
            return;
        }
        let (original, ours) = match (class.as_str(), name.as_str(), signature.as_str()) {
            ("Lorg/lwjgl/system/windows/WinBase;", "nGetProcAddress", "(JJ)J") => (
                &GET_PROC_ADDRESS,
                get_proc_address as GetProcAddressFn as Ptr,
            ),
            ("Lorg/lwjgl/system/windows/WinBase;", "nGetProcAddress", "(JJJ)J") => (
                &GET_PROC_ADDRESS_LAST_ERROR,
                get_proc_address_last_error as GetProcAddressLastErrorFn as Ptr,
            ),
            _ => return,
        };
        if address.is_null() || address == ours {
            return;
        }
        original.store(address as usize, Ordering::Release);
        // SAFETY: the JVM binds what the event leaves here.
        unsafe { *new_address = ours };
        log::info!("jvmti: LWJGL's {name}{signature} goes through the agent");
    });
}

/// In place of LWJGL 3.2's and 3.3's `WinBase.nGetProcAddress`.
unsafe extern "system" fn get_proc_address(env: Ptr, class: Ptr, module: i64, name: i64) -> i64 {
    // SAFETY: bound only after the JVM's own native was stored, with its signature.
    let address = unsafe {
        let original: GetProcAddressFn =
            std::mem::transmute(GET_PROC_ADDRESS.load(Ordering::Acquire));
        original(env, class, module, name)
    };
    // SAFETY: `name` is the NUL-terminated name LWJGL looked up.
    unsafe { handed(module, name, address) }
}

/// In place of LWJGL 3.4's `WinBase.nGetProcAddress`.
unsafe extern "system" fn get_proc_address_last_error(
    env: Ptr,
    class: Ptr,
    last_error: i64,
    module: i64,
    name: i64,
) -> i64 {
    // SAFETY: as above.
    let address = unsafe {
        let original: GetProcAddressLastErrorFn =
            std::mem::transmute(GET_PROC_ADDRESS_LAST_ERROR.load(Ordering::Acquire));
        original(env, class, last_error, module, name)
    };
    // SAFETY: as above.
    unsafe { handed(module, name, address) }
}

/// What LWJGL gets for the function it looked up in `module`: the hook in place of a hooked
/// function.
///
/// # Safety
/// `name` is 0 or a NUL-terminated string; `address` is that function of `module`.
unsafe fn handed(module: i64, name: i64, address: i64) -> i64 {
    if name == 0 || address == 0 {
        return address;
    }
    ffi::catch("LWJGL's lookup", || {
        // SAFETY: guaranteed by the caller.
        let name = unsafe { CStr::from_ptr(name as *const c_char) };
        let Some(hook) = hook_for(module as usize, name.to_bytes()) else {
            return address;
        };
        // SAFETY: the function of the hook's name, of the slot's type.
        unsafe { hook.original.set_looked_up(address as *const c_void) };
        log::info!(
            "jvmti: LWJGL looked up {}; handed the agent's hook",
            name.to_string_lossy()
        );
        hook.hook as i64
    })
    .unwrap_or(address)
}
