//! ゲームの描画先の上の、エージェント自前の OpenGL のコンテキストと、今のコンテキストへの
//! 問い合わせ。OS のコンテキストの関数（[`crate::os::Os`]）を通す。
//!
//! コンテキストはゲームと同じ描画先に作るので、どちらも同じ既定のフレームバッファに描くが、
//! GL の状態はそれぞれが持つ：スワップのフックの中でこちらに切り替え、後で戻しても、
//! Minecraft の状態には触れない（ゲームが core と compatibility のどちらのプロファイルでも）。

use std::ffi::{CStr, c_void};
use std::sync::OnceLock;

use crate::os::{self, Os};

const GL_VENDOR: u32 = 0x1F00;
const GL_RENDERER: u32 = 0x1F01;
const GL_VERSION: u32 = 0x1F02;
const GL_CONTEXT_PROFILE_MASK: u32 = 0x9126;

type GlGetStringFn = unsafe extern "system" fn(u32) -> *const u8;
type GlGetIntegervFn = unsafe extern "system" fn(u32, *mut i32);

/// 見つかった `glGetIntegerv`（GL 1.1 の関数：ドライバのどのコンテキストでも同じ）。
static GET_INTEGERV: OnceLock<GlGetIntegervFn> = OnceLock::new();

/// 今のコンテキスト。なければ 0。
pub fn current_context() -> usize {
    os::get().current_context()
}

/// 今のコンテキストの GL の関数を `F`（その関数ポインタの型）として。
///
/// # Safety
/// `F` は GL の関数 `name` の型。
pub unsafe fn function<F: Copy>(name: &CStr) -> Option<F> {
    const { assert!(size_of::<F>() == size_of::<*const c_void>()) };
    let p = os::get().gl_function(name);
    // SAFETY: 呼び出し側が保証する。
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy::<*const c_void, F>(&p) })
}

/// 今のコンテキストでの `glGetIntegerv(name)`（聞けなければ 0）。
pub fn integer(name: u32) -> i32 {
    let get = match GET_INTEGERV.get() {
        Some(&get) => Some(get),
        // SAFETY: glGetIntegerv の型。見つかってから覚える（今のコンテキストがなければ、まだ覚えない）。
        None => unsafe { function::<GlGetIntegervFn>(c"glGetIntegerv") }
            .map(|get| *GET_INTEGERV.get_or_init(|| get)),
    };
    let mut value = 0;
    if let Some(get) = get {
        // SAFETY: 値が 1 つの問い合わせは、整数を 1 つ書く。
        unsafe { get(name, &mut value) };
    }
    value
}

/// 今の GL のコンテキストの上の glow のコンテキスト。
///
/// # Safety
/// このスレッドでコンテキストが今のものであり、glow のコンテキストはその間だけ使う。
pub unsafe fn glow_context() -> glow::Context {
    let os = os::get();
    // SAFETY: 呼び出し側が保証する。ローダーは今のコンテキストの関数を引く。
    unsafe { glow::Context::from_loader_function_cstr(|name| os.gl_function(name)) }
}

/// 今のコンテキストの "version | renderer | vendor | profile"。GL 1.1 の入口だけを使うので、
/// 何も読み込まずにゲームのコンテキストで動く。
pub fn describe_current() -> String {
    // SAFETY: glGetString の型。
    let get_string = unsafe { function::<GlGetStringFn>(c"glGetString") };
    let string = |name| {
        let Some(get_string) = get_string else {
            return "?".to_owned();
        };
        // SAFETY: 今のコンテキストでの glGetString。null はエラー。
        let s = unsafe { get_string(name) };
        if s.is_null() {
            "?".to_owned()
        } else {
            // SAFETY: GL は静的な NUL で終わる文字列を返す。
            unsafe { CStr::from_ptr(s.cast()) }
                .to_string_lossy()
                .into_owned()
        }
    };
    let version = string(GL_VERSION);
    // プロファイルの問い合わせは GL 3.2 からしかない。古いコンテキストでは、ゲームの
    // コンテキストに GL のエラーを残してしまう。
    let profile = if parse_gl_version(&version) >= Some((3, 2)) {
        match integer(GL_CONTEXT_PROFILE_MASK) {
            m if m & 1 != 0 => "core",
            m if m & 2 != 0 => "compatibility",
            _ => "unknown profile",
        }
    } else {
        "legacy"
    };
    format!(
        "{version} | {} | {} | {profile}",
        string(GL_RENDERER),
        string(GL_VENDOR)
    )
}

/// "4.6.0 NVIDIA 555.85" のようなデスクトップの GL_VERSION の文字列から (major, minor)。
fn parse_gl_version(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// エージェントのコンテキスト。[`OwnContext::delete`] で明示的に消す。`Drop` では消さない
/// （スレッドの後始末の間に走りうるため）。
pub struct OwnContext {
    surface: usize,
    context: usize,
}

impl OwnContext {
    /// ウィンドウのライブラリが `surface` にもう設定したピクセルフォーマットで、コンテキストを作る。
    pub fn create(surface: usize) -> Result<Self, String> {
        let context = os::get().create_context(surface)?;
        Ok(Self { surface, context })
    }

    pub fn surface(&self) -> usize {
        self.surface
    }

    /// このコンテキストを今のものにする。ガードを捨てると、前のものに戻る。
    pub fn make_current(&self) -> Result<CurrentGuard, String> {
        let os = os::get();
        let (surface, context) = os.current_gl();
        if let Err(e) = os.make_current(self.surface, self.context) {
            // 切り替えに失敗すると、今のコンテキストがなくなっていることがある：ゲームのものに戻す。
            let _ = os.make_current(surface, context);
            return Err(e);
        }
        Ok(CurrentGuard {
            os,
            surface,
            context,
        })
    }

    /// コンテキストを消す。今のものであってはならない。
    pub fn delete(self) {
        if let Err(e) = os::get().delete_context(self.context) {
            log::warn!("cannot delete the overlay's context: {e}");
        }
    }
}

/// 捨てるときに、前に今のものだったコンテキスト（ゲームのもの）に戻す。パニックのときも。
pub struct CurrentGuard {
    os: &'static dyn Os,
    surface: usize,
    context: usize,
}

impl Drop for CurrentGuard {
    fn drop(&mut self) {
        if let Err(e) = self.os.make_current(self.surface, self.context) {
            log::error!("cannot restore the game's GL context: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_gl_version;

    #[test]
    fn gl_version_strings() {
        assert_eq!(parse_gl_version("4.6.0 NVIDIA 555.85"), Some((4, 6)));
        assert_eq!(
            parse_gl_version("3.2.0 Core Profile Context 24.10.1"),
            Some((3, 2))
        );
        assert_eq!(
            parse_gl_version("4.5 (Compatibility Profile) Mesa 25.2.8"),
            Some((4, 5))
        );
        assert_eq!(parse_gl_version("1.1.0"), Some((1, 1)));
        assert_eq!(parse_gl_version("garbage"), None);
        assert!(parse_gl_version("3.1.0") < Some((3, 2)));
    }
}
