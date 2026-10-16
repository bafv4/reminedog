//! Spike: can the agent see LWJGL's calls into GLFW, SDL3 and OpenGL without detouring the
//! libraries' functions? Nothing is patched, so it would work alike on Windows, Linux and
//! macOS. JVMTI NativeMethodBind replaces LWJGL's JNI natives as the JVM binds them:
//!
//! - the symbol lookups (`WinBase.nGetProcAddress`, `DynamicLinkLoader.ndlsym`): when LWJGL
//!   looks up `glfwSwapBuffers`, `SDL_GL_SwapWindow` or `SDL_PollEvent`, it gets this
//!   agent's function, which calls the real one. LWJGL calls GLFW and SDL through
//!   `JNI.invoke*`, which are JNI natives up to LWJGL 3.3 (on Java 8, `JavaCritical_`
//!   entries in 3.2) but FFM downcalls in LWJGL 3.4 on Java 25: only the address it is given
//!   is common to all of them
//! - `org.lwjgl.opengl.GL30C.glBindFramebuffer(II)V`: GL functions are JNI natives of their
//!   own in every version
//!
//! Everything goes to `reminedog-nmb.log` in the working folder (`log=<path>` for another):
//! every LWJGL native bound, and the counts every 300 swaps and at exit. On Windows, a small
//! magenta square is drawn in the bottom-left corner before each swap, with the game's
//! context (`nomarker` turns it off).

use std::ffi::{CStr, c_char, c_void};
use std::fs::File;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const JNI_OK: i32 = 0;
const JVMTI_VERSION_1_0: i32 = 0x3001_0000;
const JVMTI_ENABLE: i32 = 1;
const JVMTI_EVENT_NATIVE_METHOD_BIND: i32 = 67;
const JVMTI_PHASE_LIVE: i32 = 4;
const JVMTI_PHASE_START: i32 = 6;
/// Bit of `can_generate_native_method_bind_events` in `jvmtiCapabilities`.
const CAN_GENERATE_NATIVE_METHOD_BIND_EVENTS: u32 = 1 << 30;

// Slots in jvmtiInterface_1 (jvmti.h numbers them from 1).
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

const REPORT_EVERY: u64 = 300;

type JvmtiEnv = *mut *const *const c_void;
type Ptr = *mut c_void;

/// jvmtiEventCallbacks up to NativeMethodBind (event 67; VMInit is 50). The JVM copies only
/// the given size and clears the rest.
#[repr(C)]
struct EventCallbacks {
    before: [Ptr; 17],
    native_method_bind: NativeMethodBindFn,
}

type NativeMethodBindFn = unsafe extern "system" fn(
    jvmti: JvmtiEnv,
    jni: Ptr,
    thread: Ptr,
    method: Ptr,
    address: Ptr,
    new_address: *mut Ptr,
);

/// The natives this spike replaces.
#[derive(Clone, Copy)]
enum Target {
    BindFramebuffer,
    GetProcAddress,
    GetProcAddressLastError,
    Dlsym,
}

const TARGETS: usize = 4;

impl Target {
    fn find(class: &str, name: &str, signature: &str) -> Option<Self> {
        Some(match (class, name, signature) {
            ("Lorg/lwjgl/opengl/GL30C;", "glBindFramebuffer", "(II)V") => Self::BindFramebuffer,
            // LWJGL 3.2 and 3.3: (module, name).
            ("Lorg/lwjgl/system/windows/WinBase;", "nGetProcAddress", "(JJ)J") => {
                Self::GetProcAddress
            }
            // LWJGL 3.4: (last error, module, name).
            ("Lorg/lwjgl/system/windows/WinBase;", "nGetProcAddress", "(JJJ)J") => {
                Self::GetProcAddressLastError
            }
            (
                "Lorg/lwjgl/system/linux/DynamicLinkLoader;"
                | "Lorg/lwjgl/system/macosx/DynamicLinkLoader;",
                "ndlsym",
                "(JJ)J",
            ) => Self::Dlsym,
            _ => return None,
        })
    }

    fn wrapper(self) -> Ptr {
        match self {
            Self::BindFramebuffer => bind_framebuffer as BindFramebufferFn as Ptr,
            Self::GetProcAddress | Self::Dlsym => lookup as LookupFn as Ptr,
            Self::GetProcAddressLastError => lookup_last_error as LookupLastErrorFn as Ptr,
        }
    }
}

/// The JVM's own address of each replaced native.
static ORIGINAL: [AtomicUsize; TARGETS] = [const { AtomicUsize::new(0) }; TARGETS];

/// The real functions behind the ones LWJGL was given.
static GLFW_SWAP_BUFFERS: AtomicUsize = AtomicUsize::new(0);
static SDL_GL_SWAP_WINDOW: AtomicUsize = AtomicUsize::new(0);
static SDL_POLL_EVENT: AtomicUsize = AtomicUsize::new(0);

static SWAPS: AtomicU64 = AtomicU64::new(0);
static POLLS: AtomicU64 = AtomicU64::new(0);
static POLLED_EVENTS: AtomicU64 = AtomicU64::new(0);
static BIND_FRAMEBUFFER: AtomicU64 = AtomicU64::new(0);
static BIND_FRAMEBUFFER_0: AtomicU64 = AtomicU64::new(0);
static LOOKUPS: AtomicU64 = AtomicU64::new(0);
static LWJGL_BINDS: AtomicU64 = AtomicU64::new(0);

static MARKER: AtomicBool = AtomicBool::new(true);
static LOG: Mutex<Option<File>> = Mutex::new(None);
static START: OnceLock<Instant> = OnceLock::new();

fn log(line: &str) {
    let seconds = START.get().map_or(0.0, |s| s.elapsed().as_secs_f64());
    if let Ok(mut file) = LOG.lock()
        && let Some(file) = file.as_mut()
    {
        let _ = writeln!(file, "{seconds:9.3} {line}");
    }
}

fn report(when: &str) {
    let load = |c: &AtomicU64| c.load(Ordering::Relaxed);
    log(&format!(
        "{when}: swaps={} sdl_polls={} (events {}) glBindFramebuffer={} (to 0: {}) lookups={} lwjgl_binds={}",
        load(&SWAPS),
        load(&POLLS),
        load(&POLLED_EVENTS),
        load(&BIND_FRAMEBUFFER),
        load(&BIND_FRAMEBUFFER_0),
        load(&LOOKUPS),
        load(&LWJGL_BINDS),
    ));
}

/// JVMTI entry point.
///
/// # Safety
/// Called by the JVM; `options` is null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Agent_OnLoad(vm: Ptr, options: *const c_char, _: Ptr) -> i32 {
    START.get_or_init(Instant::now);
    let options = if options.is_null() {
        String::new()
    } else {
        // SAFETY: the JVM passes a NUL-terminated string.
        unsafe { CStr::from_ptr(options) }
            .to_string_lossy()
            .into_owned()
    };
    let mut path = "reminedog-nmb.log".to_owned();
    for option in options.split(',') {
        if option == "nomarker" {
            MARKER.store(false, Ordering::Relaxed);
        } else if let Some(p) = option.strip_prefix("log=") {
            path = p.to_owned();
        }
    }
    match File::create(&path) {
        Ok(file) => *LOG.lock().unwrap() = Some(file),
        Err(e) => eprintln!("reminedog-nmb: cannot write {path}: {e}"),
    }
    // SAFETY: the JVM's own pointer.
    match unsafe { start(vm) } {
        Ok(()) => log("NativeMethodBind enabled"),
        Err(e) => log(&format!("not enabled: {e}")),
    }
    // Never keep the program from starting.
    JNI_OK
}

/// # Safety
/// Called by the JVM at shutdown.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Agent_OnUnload(_vm: Ptr) {
    report("exit");
}

/// # Safety
/// `vm` is the JVM's JavaVM.
unsafe fn start(vm: Ptr) -> Result<(), String> {
    let vm = vm as *mut *const Ptr;
    let mut jvmti: JvmtiEnv = std::ptr::null_mut();
    // SAFETY: GetEnv of the JavaVM's function table.
    let get_env: unsafe extern "system" fn(*mut *const Ptr, *mut JvmtiEnv, i32) -> i32 =
        unsafe { std::mem::transmute(*(*vm).add(GET_ENV)) };
    let status = unsafe { get_env(vm, &mut jvmti, JVMTI_VERSION_1_0) };
    if status != JNI_OK || jvmti.is_null() {
        return Err(format!("GetEnv(JVMTI) failed: {status}"));
    }
    // jvmtiCapabilities is 128 bits of flags.
    let capabilities = [CAN_GENERATE_NATIVE_METHOD_BIND_EVENTS, 0, 0, 0];
    let add: unsafe extern "system" fn(JvmtiEnv, *const u32) -> i32 =
        unsafe { function(jvmti, ADD_CAPABILITIES) };
    check("AddCapabilities", unsafe {
        add(jvmti, capabilities.as_ptr())
    })?;
    let callbacks = EventCallbacks {
        before: [std::ptr::null_mut(); 17],
        native_method_bind,
    };
    let set_callbacks: unsafe extern "system" fn(JvmtiEnv, *const EventCallbacks, i32) -> i32 =
        unsafe { function(jvmti, SET_EVENT_CALLBACKS) };
    check("SetEventCallbacks", unsafe {
        set_callbacks(jvmti, &callbacks, size_of::<EventCallbacks>() as i32)
    })?;
    let set_mode: unsafe extern "system" fn(JvmtiEnv, i32, i32, Ptr) -> i32 =
        unsafe { function(jvmti, SET_EVENT_NOTIFICATION_MODE) };
    check("SetEventNotificationMode", unsafe {
        set_mode(
            jvmti,
            JVMTI_ENABLE,
            JVMTI_EVENT_NATIVE_METHOD_BIND,
            std::ptr::null_mut(),
        )
    })
}

fn check(what: &str, error: i32) -> Result<(), String> {
    if error == 0 {
        Ok(())
    } else {
        Err(format!("{what} failed: JVMTI error {error}"))
    }
}

/// # Safety
/// `slot` is a function of jvmtiInterface_1 whose type is `F`.
unsafe fn function<F: Copy>(jvmti: JvmtiEnv, slot: usize) -> F {
    // SAFETY: the table holds function pointers.
    unsafe { std::mem::transmute_copy(&*(*jvmti).add(slot)) }
}

/// A string the JVM allocated, released after reading.
unsafe fn take_string(jvmti: JvmtiEnv, s: *mut c_char) -> String {
    if s.is_null() {
        return String::new();
    }
    // SAFETY: the JVM's modified UTF-8, NUL-terminated.
    let text = unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned();
    let deallocate: unsafe extern "system" fn(JvmtiEnv, *mut c_char) -> i32 =
        unsafe { function(jvmti, DEALLOCATE) };
    unsafe { deallocate(jvmti, s) };
    text
}

unsafe extern "system" fn native_method_bind(
    jvmti: JvmtiEnv,
    _jni: Ptr,
    _thread: Ptr,
    method: Ptr,
    address: Ptr,
    new_address: *mut Ptr,
) {
    // SAFETY: JVMTI calls on the env the event came with; names are only there once the VM
    // has started.
    unsafe {
        let mut phase = 0;
        let get_phase: unsafe extern "system" fn(JvmtiEnv, *mut i32) -> i32 =
            function(jvmti, GET_PHASE);
        if get_phase(jvmti, &mut phase) != 0
            || (phase != JVMTI_PHASE_LIVE && phase != JVMTI_PHASE_START)
        {
            return;
        }
        let mut class: Ptr = std::ptr::null_mut();
        let declaring: unsafe extern "system" fn(JvmtiEnv, Ptr, *mut Ptr) -> i32 =
            function(jvmti, GET_METHOD_DECLARING_CLASS);
        if declaring(jvmti, method, &mut class) != 0 {
            return;
        }
        let mut class_signature: *mut c_char = std::ptr::null_mut();
        let class_sig: unsafe extern "system" fn(
            JvmtiEnv,
            Ptr,
            *mut *mut c_char,
            *mut *mut c_char,
        ) -> i32 = function(jvmti, GET_CLASS_SIGNATURE);
        if class_sig(jvmti, class, &mut class_signature, std::ptr::null_mut()) != 0 {
            return;
        }
        let class = take_string(jvmti, class_signature);
        if !class.starts_with("Lorg/lwjgl/") {
            return;
        }
        let (mut name, mut signature): (*mut c_char, *mut c_char) =
            (std::ptr::null_mut(), std::ptr::null_mut());
        let method_name: unsafe extern "system" fn(
            JvmtiEnv,
            Ptr,
            *mut *mut c_char,
            *mut *mut c_char,
            *mut *mut c_char,
        ) -> i32 = function(jvmti, GET_METHOD_NAME);
        if method_name(
            jvmti,
            method,
            &mut name,
            &mut signature,
            std::ptr::null_mut(),
        ) != 0
        {
            return;
        }
        let (name, signature) = (take_string(jvmti, name), take_string(jvmti, signature));
        LWJGL_BINDS.fetch_add(1, Ordering::Relaxed);
        let Some(target) = Target::find(&class, &name, &signature) else {
            log(&format!("bind {class} {name}{signature}"));
            return;
        };
        let wrapper = target.wrapper();
        if address == wrapper {
            return;
        }
        let previous = ORIGINAL[target as usize].swap(address as usize, Ordering::AcqRel);
        if previous != 0 && previous != address as usize {
            log(&format!(
                "bind {class} {name}{signature}: bound again elsewhere"
            ));
        }
        *new_address = wrapper;
        log(&format!("bind {class} {name}{signature}: wrapped"));
    }
}

fn original(target: Target) -> usize {
    ORIGINAL[target as usize].load(Ordering::Acquire)
}

type BindFramebufferFn = unsafe extern "system" fn(Ptr, Ptr, i32, i32);
type LookupFn = unsafe extern "system" fn(Ptr, Ptr, i64, i64) -> i64;
type LookupLastErrorFn = unsafe extern "system" fn(Ptr, Ptr, i64, i64, i64) -> i64;

type GlfwSwapBuffersFn = unsafe extern "C" fn(Ptr);
type SdlGlSwapWindowFn = unsafe extern "C" fn(Ptr) -> bool;
type SdlPollEventFn = unsafe extern "C" fn(Ptr) -> bool;

/// What LWJGL calls as `glfwSwapBuffers`.
unsafe extern "C" fn glfw_swap_buffers(window: Ptr) {
    before_swap("glfwSwapBuffers");
    // SAFETY: the real function LWJGL looked up.
    let real: GlfwSwapBuffersFn =
        unsafe { std::mem::transmute(GLFW_SWAP_BUFFERS.load(Ordering::Relaxed)) };
    unsafe { real(window) }
}

/// What LWJGL calls as `SDL_GL_SwapWindow`.
unsafe extern "C" fn sdl_gl_swap_window(window: Ptr) -> bool {
    before_swap("SDL_GL_SwapWindow");
    // SAFETY: the real function LWJGL looked up.
    let real: SdlGlSwapWindowFn =
        unsafe { std::mem::transmute(SDL_GL_SWAP_WINDOW.load(Ordering::Relaxed)) };
    unsafe { real(window) }
}

/// What LWJGL calls as `SDL_PollEvent`.
unsafe extern "C" fn sdl_poll_event(event: Ptr) -> bool {
    // SAFETY: the real function LWJGL looked up.
    let real: SdlPollEventFn =
        unsafe { std::mem::transmute(SDL_POLL_EVENT.load(Ordering::Relaxed)) };
    let polled = unsafe { real(event) };
    POLLS.fetch_add(1, Ordering::Relaxed);
    if polled {
        POLLED_EVENTS.fetch_add(1, Ordering::Relaxed);
    }
    polled
}

unsafe extern "system" fn bind_framebuffer(env: Ptr, class: Ptr, target: i32, framebuffer: i32) {
    BIND_FRAMEBUFFER.fetch_add(1, Ordering::Relaxed);
    if framebuffer == 0 {
        BIND_FRAMEBUFFER_0.fetch_add(1, Ordering::Relaxed);
    }
    // SAFETY: the JVM's native for this method.
    let original: BindFramebufferFn =
        unsafe { std::mem::transmute(original(Target::BindFramebuffer)) };
    unsafe { original(env, class, target, framebuffer) }
}

unsafe extern "system" fn lookup(env: Ptr, class: Ptr, library: i64, name: i64) -> i64 {
    // Both lookups take (library, name); which one was bound is in ORIGINAL.
    let target = if original(Target::Dlsym) != 0 {
        Target::Dlsym
    } else {
        Target::GetProcAddress
    };
    // SAFETY: the JVM's native for this method.
    let original: LookupFn = unsafe { std::mem::transmute(original(target)) };
    let address = unsafe { original(env, class, library, name) };
    unsafe { substitute(name, address) }
}

unsafe extern "system" fn lookup_last_error(
    env: Ptr,
    class: Ptr,
    last_error: i64,
    library: i64,
    name: i64,
) -> i64 {
    // SAFETY: the JVM's native for this method.
    let original: LookupLastErrorFn =
        unsafe { std::mem::transmute(original(Target::GetProcAddressLastError)) };
    let address = unsafe { original(env, class, last_error, library, name) };
    unsafe { substitute(name, address) }
}

/// The address LWJGL gets for the symbol it looked up: this agent's function in place of
/// the ones it watches.
///
/// # Safety
/// `name` is LWJGL's NUL-terminated name of the symbol.
unsafe fn substitute(name: i64, address: i64) -> i64 {
    LOOKUPS.fetch_add(1, Ordering::Relaxed);
    if name == 0 || address == 0 {
        return address;
    }
    // SAFETY: the caller's string.
    let name = unsafe { CStr::from_ptr(name as *const c_char) };
    let (real, ours) = match name.to_bytes() {
        b"glfwSwapBuffers" => (
            &GLFW_SWAP_BUFFERS,
            glfw_swap_buffers as GlfwSwapBuffersFn as usize,
        ),
        b"SDL_GL_SwapWindow" => (
            &SDL_GL_SWAP_WINDOW,
            sdl_gl_swap_window as SdlGlSwapWindowFn as usize,
        ),
        b"SDL_PollEvent" => (&SDL_POLL_EVENT, sdl_poll_event as SdlPollEventFn as usize),
        _ => return address,
    };
    real.store(address as usize, Ordering::Relaxed);
    log(&format!(
        "looked up {}: substituted",
        name.to_string_lossy()
    ));
    ours as i64
}

fn before_swap(what: &str) {
    let swaps = SWAPS.fetch_add(1, Ordering::Relaxed) + 1;
    if swaps == 1 {
        log(&format!(
            "first {what} through the wrapper, on {:?}",
            std::thread::current().id()
        ));
    }
    if swaps.is_multiple_of(REPORT_EVERY) {
        report("swaps");
    }
    #[cfg(windows)]
    if MARKER.load(Ordering::Relaxed) {
        marker::draw();
    }
}

/// A magenta square in the bottom-left corner of what the game is about to show, drawn
/// with the game's own context (OpenGL 1.1 from opengl32.dll; its state put back).
#[cfg(windows)]
mod marker {
    use std::ffi::{c_char, c_void};
    use std::sync::OnceLock;

    const GL_SCISSOR_BOX: u32 = 0x0C10;
    const GL_SCISSOR_TEST: u32 = 0x0C11;
    const GL_COLOR_CLEAR_VALUE: u32 = 0x0C22;
    const GL_COLOR_WRITEMASK: u32 = 0x0C23;
    const GL_COLOR_BUFFER_BIT: u32 = 0x4000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    struct Gl {
        enable: unsafe extern "system" fn(u32),
        disable: unsafe extern "system" fn(u32),
        is_enabled: unsafe extern "system" fn(u32) -> u8,
        scissor: unsafe extern "system" fn(i32, i32, i32, i32),
        get_integerv: unsafe extern "system" fn(u32, *mut i32),
        get_floatv: unsafe extern "system" fn(u32, *mut f32),
        get_booleanv: unsafe extern "system" fn(u32, *mut u8),
        clear_color: unsafe extern "system" fn(f32, f32, f32, f32),
        color_mask: unsafe extern "system" fn(u8, u8, u8, u8),
        clear: unsafe extern "system" fn(u32),
    }

    static GL: OnceLock<Option<Gl>> = OnceLock::new();

    fn load() -> Option<Gl> {
        let name: Vec<u16> = "opengl32.dll\0".encode_utf16().collect();
        // SAFETY: the game has loaded opengl32 by its first swap; exported GL 1.1 functions.
        unsafe {
            let module = GetModuleHandleW(name.as_ptr());
            if module.is_null() {
                return None;
            }
            /// The export as the function type of the field it fills.
            unsafe fn f<F>(module: *mut c_void, name: &[u8]) -> Option<F> {
                let p = unsafe { GetProcAddress(module, name.as_ptr().cast()) };
                (!p.is_null()).then(|| unsafe { std::mem::transmute_copy(&p) })
            }
            Some(Gl {
                enable: f(module, b"glEnable\0")?,
                disable: f(module, b"glDisable\0")?,
                is_enabled: f(module, b"glIsEnabled\0")?,
                scissor: f(module, b"glScissor\0")?,
                get_integerv: f(module, b"glGetIntegerv\0")?,
                get_floatv: f(module, b"glGetFloatv\0")?,
                get_booleanv: f(module, b"glGetBooleanv\0")?,
                clear_color: f(module, b"glClearColor\0")?,
                color_mask: f(module, b"glColorMask\0")?,
                clear: f(module, b"glClear\0")?,
            })
        }
    }

    pub fn draw() {
        let Some(gl) = GL.get_or_init(load) else {
            return;
        };
        // SAFETY: on the thread whose context is about to be swapped.
        unsafe {
            let scissor_on = (gl.is_enabled)(GL_SCISSOR_TEST);
            let mut box_ = [0; 4];
            (gl.get_integerv)(GL_SCISSOR_BOX, box_.as_mut_ptr());
            let mut color = [0.0; 4];
            (gl.get_floatv)(GL_COLOR_CLEAR_VALUE, color.as_mut_ptr());
            let mut mask = [0; 4];
            (gl.get_booleanv)(GL_COLOR_WRITEMASK, mask.as_mut_ptr());

            (gl.enable)(GL_SCISSOR_TEST);
            (gl.scissor)(8, 8, 40, 40);
            (gl.color_mask)(1, 1, 1, 1);
            (gl.clear_color)(1.0, 0.0, 1.0, 1.0);
            (gl.clear)(GL_COLOR_BUFFER_BIT);

            (gl.color_mask)(mask[0], mask[1], mask[2], mask[3]);
            (gl.clear_color)(color[0], color[1], color[2], color[3]);
            (gl.scissor)(box_[0], box_[1], box_[2], box_[3]);
            if scissor_on == 0 {
                (gl.disable)(GL_SCISSOR_TEST);
            }
        }
    }
}
