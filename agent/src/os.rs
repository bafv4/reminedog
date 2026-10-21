//! エージェントが OS に頼ること。各 OS のエージェントのライブラリが実装し（Windows 用は
//! `hook-win`）、`Agent_OnLoad` で 1 回だけ入れる（[`set`]）。
//!
//! ハンドルは、OS が何と呼ぶものでも中身を見ない `usize`：ライブラリは LWJGL が関数を引いた
//! ハンドル（`HMODULE`、`dlopen` のハンドル）、ネイティブのウィンドウはウィンドウのライブラリの
//! もの（`HWND`、X11 の `Window`）、描画先は GL のコンテキストが描く先（`HDC`、GLX の
//! drawable、EGL の surface）、コンテキストは GL のコンテキスト（`HGLRC`、`GLXContext`、
//! `EGLContext`）。

use std::ffi::{CStr, c_void};
use std::path::Path;
use std::sync::OnceLock;

use reminedog_core::{InputId, Settings};
use reminedog_render::{
    BrowserAction, BrowserCommand, BrowserPixels, BrowserView, Notice, PointerSpeed,
};

pub trait Os: Sync {
    /// `library` の export の関数 `name` のアドレス。
    fn symbol(&self, library: usize, name: &CStr) -> Option<*const c_void>;
    /// `library` を読み込んだ場所（ログのため）。
    fn library_path(&self, library: usize) -> String;

    /// ウィンドウのネイティブのウィンドウを返す GLFW の関数（`glfwGetWin32Window`）。あれば。
    fn glfw_native_window(&self) -> Option<&'static CStr>;
    /// ネイティブのウィンドウを持つ SDL3 のウィンドウのプロパティ。
    fn sdl_native_window(&self) -> Option<SdlProperty>;

    /// ゲームのコンテキストが `native_window` を描く描画先（オーバーレイのコンテキストのため。
    /// なければ、このスレッドで今の描画先）。どちらもなければ 0。
    fn window_surface(&self, native_window: Option<usize>) -> usize;
    /// 描画先のピクセルフォーマット（ログのため）。
    fn describe_surface(&self, surface: usize) -> String;
    /// このスレッドで今の描画先とコンテキスト（なければ 0）。
    fn current_gl(&self) -> (usize, usize);
    /// このスレッドで今のコンテキストだけ（なければ 0）：ズームがゲームのフレームバッファの
    /// 結び付けのたびに聞く。
    fn current_context(&self) -> usize {
        self.current_gl().1
    }
    /// 今のコンテキストの GL の関数。なければ null。
    fn gl_function(&self, name: &CStr) -> *const c_void;
    /// `surface` のための新しいコンテキスト（そのピクセルフォーマットで）。
    fn create_context(&self, surface: usize) -> Result<usize, String>;
    /// `context` を `surface` で今のものにする（0 と 0：なし）。
    fn make_current(&self, surface: usize, context: usize) -> Result<(), String>;
    /// 今のものでないコンテキストを消す。
    fn delete_context(&self, context: usize) -> Result<(), String>;
    /// オーバーレイのコンテキストをこのスレッドで作れるか。作れなければ、作れるスレッドの
    /// スワップを待つ（macOS：AppKit はメインスレッドでだけビューに付けられる。Forge の早い読み込み
    /// 画面は専用のスレッドからスワップする）。
    fn can_create_context_here(&self) -> bool {
        true
    }
    /// OS が `native_window` を最小化して見せているか（安く分かる場合）。`None` ならウィンドウの
    /// ライブラリに聞く（Windows は答える：SDL3 のフラグは、ゲームがポーリングするまで変わらない）。
    fn window_minimized(&self, _native_window: Option<usize>) -> Option<bool> {
        None
    }

    /// キーかマウスのボタンを、システムが今離していると見ているか。分からなければ false
    /// （ゲームが受け取りそこねた解放の安全網、[`crate::rebind_state`]）。
    fn input_up(&self, id: InputId) -> bool;
    /// 相対移動でシステムのポインターがどう動くか（オーバーレイの自前のカーソル）。
    fn pointer_speed(&self) -> PointerSpeed;
    /// クリップボードの文字（メニューのため。読む間は `native_window` がクリップボードを持つ）。
    fn read_clipboard(&self, native_window: Option<usize>) -> Option<String>;
    /// メニューでコピーした文字をクリップボードに置く。
    fn write_clipboard(&self, native_window: Option<usize>, text: &str) -> bool;
    /// 日本語の文字のフォントを読む（専用のスレッドで。[`crate::fonts`] が持つ）。
    fn japanese_font(&self) -> Option<FontFile>;
    /// ゲーム内ブラウザ（ある OS では）。
    fn browser(&self) -> Option<&dyn Browser>;

    /// 別の場所のエージェント（それぞれ自分の static を持つ）がこのプロセスで動いているか。
    /// 動いていれば、こちらは止まる。
    fn another_copy_runs(&self) -> bool {
        false
    }
    /// ログを書けないときに、問題を伝える。
    fn report(&self, message: &str) {
        eprintln!("reminedog: {message}");
    }
    /// パニックが起きた場所（ログのため。パニックフックから呼ぶ）。
    fn panic_details(&self) -> Option<String> {
        None
    }
}

/// オーバーレイのために読んだフォントのファイル。
pub struct FontFile {
    pub name: String,
    pub data: Vec<u8>,
    /// コレクション（`.ttc`）の中の面。ふつうのフォントは 0。
    pub index: u32,
}

/// SDL3 がウィンドウのネイティブのウィンドウを渡す形。
#[derive(Clone, Copy)]
pub enum SdlProperty {
    /// ポインタのプロパティ（`SDL.window.win32.hwnd`）。
    Pointer(&'static CStr),
    /// 数のプロパティ（`SDL.window.x11.window`）。
    Number(&'static CStr),
}

/// オーバーレイから見たゲーム内ブラウザ（[`crate::frame`]）。
pub trait Browser: Sync {
    /// このフレームでオーバーレイが見せるブラウザと、ブラウザの新しい通知。
    fn before_frame(&self) -> (BrowserView, Vec<Notice>);
    /// ページの最新の絵が `version` から変わっていれば、それで `upload` を呼ぶ（`version` も
    /// 更新する）。
    fn with_new_picture(&self, version: &mut u64, upload: &mut dyn FnMut(BrowserPixels<'_>));
    /// オーバーレイとブラウザのホットキーが頼んだことを、順に行う。
    fn after_frame(
        &self,
        commands: Vec<BrowserCommand>,
        actions: Vec<BrowserAction>,
        settings: &Settings,
        game_dir: &Path,
    );
}

/// まだ絵がない（[`Browser::with_new_picture`] のため）。
pub const NO_PICTURE: u64 = u64::MAX;

static OS: OnceLock<&'static dyn Os> = OnceLock::new();

/// プロセスの OS を入れる。もう入っていれば false。
pub fn set(os: &'static dyn Os) -> bool {
    OS.set(os).is_ok()
}

/// OS。[`set`] の前に呼ぶとパニックする：エージェントの入口は、どのフックも動く前に入れる。
pub fn get() -> &'static dyn Os {
    *OS.get().expect("the OS is set in Agent_OnLoad")
}
