//! エージェント本体。OS に依存する部分は除く。それは各 OS のエージェントのライブラリが、
//! JVMTI の入口と一緒に渡す（[`os::Os`]。Windows 用は `hook-win` が作る `reminedog.dll`）。
//!
//! - [`jvmti`]：JVM は LWJGL のネイティブを結び付けるたびに知らせる。LWJGL の関数の引き当てを
//!   差し替え、ゲームのウィンドウのライブラリ（[`library`]）のフック（[`hook`]）を渡す
//! - [`glfw`]、[`glfw_input`]（Minecraft 1.21 まで）、[`sdl`]、[`sdl_input`]（26.x）：フック、
//!   入力、キーの置き換えから見たキーの状態（[`rebind_state`]）
//! - [`frame`]：バッファのスワップのたびに自前のコンテキスト（[`gl`]）で描くオーバーレイと、
//!   高精細のズーム（[`tall`]）
//! - [`f3c`]、[`waypoints`]：F3+C の要求と結果、今のワールドのウェイポイント
//! - [`input`]：ウィンドウのライブラリのフックとオーバーレイが共有する入力の振り分け
//! - [`saver`]：ファイルを書くスレッド。[`globals`]：オプションとゲームフォルダ
//! - [`start`]：`Agent_OnLoad` の流れ（各 OS のライブラリが呼ぶ）

mod f3c;
mod fonts;
mod frame;
mod gl;
mod glfw;
mod glfw_input;
pub mod globals;
mod hook;
mod input;
pub mod jvmti;
mod library;
pub mod os;
mod rebind_state;
mod saver;
mod sdl;
mod sdl_input;
mod start;
mod tall;
mod waypoints;

pub use start::start;

/// 版：リリースの版（リリースのワークフローが REMINEDOG_VERSION を入れる）、なければ crate の版
/// （`build.rs`）。
pub const VERSION: &str = env!("REMINEDOG_VERSION");

/// ログでビルドを見分ける。CI はコミットのハッシュを入れる（それ以外は "local"）。
pub const BUILD_ID: &str = env!("REMINEDOG_BUILD_ID");

use std::panic::{AssertUnwindSafe, catch_unwind};

/// `f` を動かし、パニックをログの行にして止める。JVM・LWJGL・ライブラリから呼ばれる関数は、
/// すべてこれを通すこと：よその呼び出しの枠までパニックが巻き戻るとプロセスが止まる。
/// ここではそれはゲームが落ちることを意味する。
pub fn catch<R>(what: &str, f: impl FnOnce() -> R) -> Option<R> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => Some(value),
        Err(_) => {
            // パニックフックが、メッセージと場所をもうログに書いた。
            log::error!("recovered from a panic in {what}");
            None
        }
    }
}
