# reminedog：開発の引き継ぎ

Minecraft Java Edition（1.13 以降、26.x を含む）のサバイバル向けゲーム内ツール。
Mod ではなく、JVM に `-agentpath:` で読み込ませる Rust 製のネイティブのエージェント（JVMTI）。ゲームのウィンドウにオーバーレイ（egui）を描き、入力を横取りする。
まず Windows。Linux は後回し。

- 設計と経緯：[docs/HANDOFF.md](docs/HANDOFF.md)（最初の設計資料に、実装で分かったことを「追記」として順に足している）
- 使い方：[README.md](README.md)
- 実機で確かめる手順：[docs/TESTING.md](docs/TESTING.md)

## 利用者とのやり取り

- 利用者は日本語で話す。返事・ドキュメント・UI の文言は日本語。コードのコメント、コミットメッセージ、ログは英語
- 実機で確かめるのは利用者（Windows、NVIDIA RTX 4060 Ti、Prism Launcher、バニラ）。確かめたバージョンは 1.21.11（GLFW 3.4.0）と 26.3（SDL 3.4.14）
- JVM 引数は `-agentpath:C:\reminedog\reminedog.dll`（先頭の `-` を忘れると起動しない）。ログは `<ゲームフォルダ>/reminedog/reminedog.log`、詳しいログは `=log=debug` を付ける
- 利用者に確かめてほしいことは、`docs/TESTING.md` のチェックリストに書いて渡す

## 構成

```
core/      OS に依存しない処理（テストあり）
  location.rs  F3+C のクリップボードの文字列のパース
  waypoint.rs  ワールドごとのウェイポイントの JSON（アトミックな保存）
  world.rs     ワールドの判定（latest.log の追跡、シングルプレイのワールド）
  nav.rs       方角・距離、ネザー座標の変換
  options.rs   -agentpath:...=<オプション> の解析（gamedir, log, overlay, scale）
  gamedir.rs   ゲームフォルダの判定、reminedog/ 以下のパス
  settings.rs  settings.json（キー、ズームの倍率など）
  logfile.rs   log クレートのファイル出力
render/    glow + egui_glow の描画（OS に依存しない、テストあり）
  overlay.rs   egui のメニュー、状態の表示、ヒント、自前のカーソル。FrameInput/FrameOutput でフックとやり取り
  input.rs     InputRouter：各入力を「ゲームに渡す／横取りする」を決める。ホットキー、キーの割り当て待ち、自前のカーソル
  hotkey.rs    Hotkey（キーかマウスのボタン＋修飾キー）。設定ファイルの文字列との変換
  zoom.rs      引き伸ばすズーム（高精細が使えないとき）と、縦長の大きさの計算
  pointer.rs   Windows のポインターの速度・加速の計算（自前のカーソル用）
  font_metrics.rs  日本語フォントの縦位置の補正
hook-win/  Windows のエージェント（reminedog.dll）
  lib.rs/agent.rs  Agent_OnLoad。オプション、ログ、パニックフック
  loader.rs    LdrRegisterDllNotification で DLL の読み込みを監視（glfw / SDL3 / opengl32）
  hook.rs      MinHook によるデトア
  glfw.rs, glfw_input.rs  GLFW（Minecraft 1.21 まで）：glfwSwapBuffers、コールバックの差し替え
  sdl.rs, sdl_input.rs    SDL3（Minecraft 26.x）：SDL_GL_SwapWindow、SDL_PollEvent のフィルタ
  frame.rs     スワップのたびの処理：自前の WGL コンテキストに切り替えてオーバーレイを描く。設定の読み込みと保存
  tall.rs      高精細のズーム（下で説明）
  wgl.rs       opengl32 の関数表、自前のコンテキスト
  pointer.rs   Windows のマウスの設定の読み取り
  fonts.rs     日本語フォント（游ゴシック → メイリオ → MS ゴシック）
ci/smoke/  LWJGL で Minecraft と同じようにウィンドウを作るテスト用の Java（GLFW 版 Smoke、SDL3 版 SmokeSdl）
scripts/   wine-smoke.sh（Linux 上で Wine を使って動かす）、fetch-wine-jre.sh
```

## ビルドとテスト

Windows で：

```
cargo build --release -p reminedog-hook-win      # target\release\reminedog.dll
cargo test --workspace
cargo clippy --workspace --all-targets
```

- Rust 1.95 以降（egui 0.36 の要件。今は 1.98）、edition 2024
- MSVC では CRT を静的にリンクする（`.cargo/config.toml` の `+crt-static`。Java 8 に vcruntime140 がないため）
- Linux からは `--target x86_64-pc-windows-gnu` で DLL をビルドし、`scripts/wine-smoke.sh` で試せる（README の「開発」）。Windows では DLL を置いてゲームで確かめるのが早い
- テスト用プログラムを Windows で直接動かすなら、`ci/smoke/*.java` を LWJGL の jar と一緒にコンパイルし、`java -agentpath:...\reminedog.dll -cp ... Smoke --seconds=20 --capture --mc` のように動かす（`.github/workflows/ci.yml` の `smoke-windows` ジョブが手順の見本）
- CI（GitHub Actions）：Linux（fmt、clippy、テスト、mingw での clippy）、Windows（clippy、テスト、リリースビルド、成果物 `reminedog-windows-x64`）、Windows のスモークテスト（LWJGL 3.2.2/Java 8、3.3.3/Java 21、3.4.3/Java 25 の SDL3。Mesa の llvmpipe で描く）

コミットする前に `cargo fmt --all`、clippy（警告 0）、テストを通す。

## 守ること

- **コマンドラインをログに出さない**（Minecraft の `--accessToken` が入っている）
- **Minecraft の jar やアセットをリポジトリに入れない**
- フックの中でパニックさせない（`ffi::catch` で包む）。失敗しても `JNI_OK` を返し、ゲームは止めない
- ゲームの GL の状態を変えたら必ず元に戻す（オーバーレイは自前のコンテキストで描く。`tall.rs` だけはゲームのコンテキストで blit し、`SavedState` で戻す）
- ゲームのコードを呼ぶ間（コールバックの転送など）は、`input::router()` のロックを持たない
- デトアには MinHook を使う。retour 0.3 は、直後に即値が続く RIP 相対の命令を正しく移せず、GLFW の関数の先頭で落ちる
- export は `ffi::export_address`（PE を自分で読む）で引く。ローダーロックの中で GetProcAddress を呼ばないため

## 今の状態（2026-09-29）

| 機能 | 状態 |
|---|---|
| オーバーレイ（egui、日本語フォント） | 実機で確認済み（1.21.11、26.3） |
| メニュー（Ctrl+I）、入力の横取り、自前のカーソル（Windows のポインターの速度を反映） | 実機で確認済み |
| チェックボックスと文字の縦位置の補正 | 実機で確認済み |
| 高精細のズーム（`tall.rs`） | Wine のみ。**実機は未確認**（特に 26.x） |
| キーの変更、settings.json の保存 | Wine のみ。実機は未確認 |
| F3+C によるウェイポイントの記録 | 未実装（core のパースと保存はある） |

### 高精細のズームの仕組み（`hook-win/src/tall.rs`）

1. ズームキーを押している間、ゲームにフレームバッファが縦に k 倍長いと伝える（GLFW：ゲームのフレームバッファの大きさのコールバックをスワップの後に呼び、`glfwGetFramebufferSize` を差し替え。SDL3：`SDL_PollEvent` から大きさの変更のイベントを渡し、`SDL_GetWindowSizeInPixels` を差し替え）
2. opengl32.dll が読み込まれたときに `wglGetProcAddress` をフックし、ゲームに `glBindFramebuffer(EXT)`・`glBlitNamedFramebuffer` のラッパーを渡す。ズーム中はフレームバッファ 0 を自前の縦長のフレームバッファに差し替える
3. スワップのときに、その中央をウィンドウに blit する
4. `glViewport` のラッパーで、ゲームが本当に縦長で描いたかを確かめる。3 フレーム続けて描かれなければ、引き伸ばすズームに切り替える

26.x がリサイズのイベントに反応するかは未確認。反応しなければ、ログに `zoom: the game does not render at the tall size` と出て、引き伸ばすズームになる。

## 次にやること（候補）

1. 高精細のズームとキーの変更を実機で確かめてもらう（`docs/TESTING.md` の「ズームとキーの変更で確認すること」）。問題があれば `=log=debug` のログの `zoom:` の行から直す
2. プロトタイプ 3：F3+C でウェイポイントを記録する。`glfwSetClipboardString`（SDL3 は `SDL_SetClipboardText`）をフックして F3+C の文字列を横取りし、`core::parse_f3c` と `WaypointStore` で保存。ワールドの判定は `core::world`（`LogTail`、`WorldTracker`）。メニューに一覧と方角・距離（`core::nav`）を出す。設計は HANDOFF.md
3. IME の変換中の文字をメニューの入力欄に出す（今は確定した文字だけ）
4. Linux（Fedora）対応。方針の案は HANDOFF.md の構成の `hook-linux/`（GLFW の関数をフックする、自前のコンテキストは GLX／EGL。MinHook は使えないのでデトアの方法を検討する）

## 開発環境についての注意

- Wine＋Xvfb では、カーソルを捕まえた状態でのマウスの相対移動が届かない（自前のカーソルの動きは Wine では確かめられない）
- `scripts/wine-smoke.sh --grab` は X の画面全体を保存する。SDL3 版のウィンドウは画面の中央寄りに開く
- 26.x の Minecraft の jar は手元にない（中身を読んで確かめられない）。挙動はログ（SDL3 の各関数の最初の呼び出しを記録するプローブ）から推測している
