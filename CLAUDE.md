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
  session.rs   WorldWatcher（今のワールド）と WaypointBook（今のワールドのウェイポイント。変更のたびに保存）
  keybinds.rs  options.txt の F3+C のキー（修飾キー、座標のコピー、クラッシュ）と、コピーのキーを共有する操作。GLFW のキーコード、SDL のスキャンコード・キーコードへの変換
  nav.rs       方角・距離、ネザー座標の変換、guide（ウェイポイントへの案内）
  options.rs   -agentpath:...=<オプション> の解析（gamedir, log, overlay, scale）
  gamedir.rs   ゲームフォルダの判定、reminedog/ 以下のパス
  settings.rs  settings.json（キー、ズームの倍率など）
  logfile.rs   log クレートのファイル出力
render/    glow + egui_glow の描画（OS に依存しない、テストあり）
  overlay.rs   egui のメニュー、状態の表示、ヒント、自前のカーソル、Hotkeys。FrameInput/FrameOutput でフックとやり取り
  waypoints.rs メニューの「ウェイポイント」の欄、通知、方角と距離の文言。WaypointView/WaypointCommand でフックとやり取り
  input.rs     InputRouter：各入力を「ゲームに渡す／横取りする」を決める。ホットキー（記録と更新は HotkeyAction のキュー。Minecraft の修飾キーを押している間はゲームに渡す）、キーの割り当て待ち、自前のカーソル
  hotkey.rs    Hotkey（キーかマウスのボタン＋修飾キー）。設定ファイルの文字列との変換
  zoom.rs      引き伸ばすズーム（高精細が使えないとき）と、縦長の大きさの計算
  pointer.rs   Windows のポインターの速度・加速の計算（自前のカーソル用）
  font_metrics.rs  日本語フォントの縦位置の補正
hook-win/  Windows のエージェント（reminedog.dll）
  lib.rs/agent.rs  Agent_OnLoad。オプション、ログ、パニックフック
  loader.rs    LdrRegisterDllNotification で DLL の読み込みを監視（glfw / SDL3 / opengl32）
  hook.rs      MinHook によるデトア
  glfw.rs, glfw_input.rs  GLFW（Minecraft 1.21 まで）：glfwSwapBuffers、コールバックの差し替え。F3+C（glfwSetClipboardString・glfwGetKey のフック、キーコールバックへの送信）
  sdl.rs, sdl_input.rs    SDL3（Minecraft 26.x）：SDL_GL_SwapWindow、SDL_PollEvent のフィルタ。F3+C（SDL_SetClipboardText のフック、キーのイベントの注入）
  frame.rs     スワップのたびの処理：自前の WGL コンテキストに切り替えてオーバーレイを描く。設定の読み込みと保存
  f3c.rs       F3+C の要求と結果（ウィンドウのライブラリに依存しない）。送った F3+C の書き込みは握りつぶし、利用者の F3+C は通す
  waypoints.rs ワールドの追跡、options.txt、F3+C の要求と結果、通知（frame.rs から毎フレーム）
  tall.rs      高精細のズーム（下で説明）
  wgl.rs       opengl32 の関数表、自前のコンテキスト
  pointer.rs   Windows のマウスの設定の読み取り
  fonts.rs     日本語フォント（游ゴシック → メイリオ → MS ゴシック）
ci/smoke/  LWJGL で Minecraft と同じようにウィンドウを作るテスト用の Java（GLFW 版 Smoke、SDL3 版 SmokeSdl）。F3+C の真似（--world、--f3c-refuse、--f3c-events）
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
  - ウェイポイントを試すなら `--world=<名前>` を付ける（作業フォルダに latest.log と saves を書く）。記録できれば `reminedog/waypoints/sp-<名前>.json` ができ、終了時の行が `F3C STATE overlay=off modifier=up copies=1` になる。拒否は `--f3c-refuse`。詳しくは HANDOFF.md の「追記：プロトタイプ 3」
  - 本物の F3+C を送るときは、PowerShell の `SendKeys` ではなく `Add-Type` で `SendInput` を使う（`SendKeys` では F3 を押したまま C を押せない）
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
- F3+C を送るときは、C（クラッシュのキー）が押されているとゲームに読ませない（修飾キーと同時だと、10 秒でゲームを落とすデバッグのクラッシュが動く）。GLFW の修飾キーの偽装は、ガードの Drop で必ず消す。ロックの順序は `hook-win/src/f3c.rs` の先頭に書いてある

## 今の状態（2026-09-30）

| 機能 | 状態 |
|---|---|
| オーバーレイ（egui、日本語フォント） | 実機で確認済み（1.21.11、26.3） |
| メニュー（Ctrl+I）、入力の横取り、自前のカーソル（Windows のポインターの速度を反映） | 実機で確認済み |
| チェックボックスと文字の縦位置の補正 | 実機で確認済み |
| 高精細のズーム（`tall.rs`） | 実機で確認済み（1.21.11、26.3） |
| キーの変更、settings.json の保存 | 倍率の保存は実機で確認済み。キーの変更は Wine のみ |
| F3+C によるウェイポイント（J で記録、K で方角と距離、メニューの一覧） | 手元のスモーク（GLFW・SDL3）で確認済み。実機では未確認 |

### 高精細のズームの仕組み（`hook-win/src/tall.rs`）

1. ズームキーを押している間、ゲームにフレームバッファが縦に k 倍長いと伝える（GLFW：ゲームのフレームバッファの大きさのコールバックをスワップの後に呼び、`glfwGetFramebufferSize` を差し替え。SDL3：`SDL_PollEvent` から大きさの変更のイベントを渡し、`SDL_GetWindowSizeInPixels` を差し替え）
2. opengl32.dll が読み込まれたときに `wglGetProcAddress` をフックし、ゲームに `glBindFramebuffer(EXT)`・`glBlitNamedFramebuffer` のラッパーを渡す。ズーム中はフレームバッファ 0 を自前の縦長のフレームバッファに差し替える
3. スワップのときに、その中央をウィンドウに blit する
4. `glViewport` のラッパーで、ゲームが本当に縦長で描いたかを確かめる。3 フレーム以上かつ 1 秒以上続けて描かれなければ、引き伸ばすズームに切り替える（描画先の作り直しで数フレーム止まっても見切らないように）

26.3 の実機では、ゲームは縦長で描いていたが幅が 2560（SDL が報告するウィンドウは 2561 px）で、幅まで一致を求める判定に落ちて引き伸ばすズームになっていた。判定を高さだけにして直し、実機で確認した。26.3 はウィンドウの大きさを `SDL_GetWindowSizeInPixels` で問い合わせず、イベントの値を使う。`=log=debug` の `zoom: frame not rendered ...` の行に、`glViewport` の回数・最大の大きさ・スワップ時の viewport・大きさの問い合わせの回数が出る。

### ウェイポイントの仕組み（`hook-win/src/f3c.rs`、`waypoints.rs`）

1. J・K（またはメニューのボタン）で、`waypoints.rs` が F3+C の要求を出す。キーは options.txt（修飾キー・座標のコピー・クラッシュ）から 5 秒ごとに読む
2. GLFW はスワップの後にゲームのキーコールバックを直接呼び、SDL3 は `SDL_PollEvent` の注入のキューから、修飾キーの押下 → コピーの押下 → コピーの解放 → 修飾キーの解放を送る（GLFW は 1.16 向けに、その間だけ `glfwGetKey` に修飾キーを押していると答える）
3. その間の `glfwSetClipboardString`／`SDL_SetClipboardText` を横取りして `parse_f3c` で読み、OS には渡さない。書き込みがなければ拒否（デバッグ情報の制限）で、F3 画面を戻すために修飾キーをもう一度押して離し、そのワールドでは送るのをやめる
4. 座標は `WaypointBook`（ワールドは `WorldWatcher`）に保存し、通知と方角・距離（`guide`）を出す。利用者自身の F3+C も、コピーのキーをゲームに渡す間の書き込みから拾う（こちらは OS に渡す）

ゲームの挙動（1.16.1、1.21.11、26.3 の jar で確かめたこと）と、確かめていないことは HANDOFF.md の「追記：プロトタイプ 3」。

## 次にやること（候補）

1. ウェイポイント、高精細のズーム、キーの変更を実機で確かめてもらう（`docs/TESTING.md` の「ウェイポイントで確認すること」と「ズームとキーの変更で確認すること」）。問題があれば `=log=debug` のログの `F3+C:`・`world:`・`zoom:` の行から直す
2. CI のスモークテストで F3+C を確かめる（`--world`・`--f3c-refuse` を使い、マーカーに `F3+C: clipboard hooks ready` などを足す。今の CI は新しいオプションを使っていない）
3. IME の変換中の文字をメニューの入力欄に出す（今は確定した文字だけ）
4. Linux（Fedora）対応。方針の案は HANDOFF.md の構成の `hook-linux/`（GLFW の関数をフックする、自前のコンテキストは GLX／EGL。MinHook は使えないのでデトアの方法を検討する）

## 開発環境についての注意

- Wine＋Xvfb では、カーソルを捕まえた状態でのマウスの相対移動が届かない（自前のカーソルの動きは Wine では確かめられない）
- `scripts/wine-smoke.sh --grab` は X の画面全体を保存する。SDL3 版のウィンドウは画面の中央寄りに開く
- 利用者のインスタンスのクライアントの jar は `%APPDATA%\PrismLauncher\libraries\com\mojang\minecraft\<版>\minecraft-<版>-client.jar` にある（1.16.1、1.21.11、26.3）。
  26.3 は難読化されていない。1.21.11 と 1.16.1 は難読化されているので、クラスは文字列の定数から探す（`javap -c -p -constants`）。展開したものはリポジトリの外（scratchpad）に置く
- 利用者の 1.16.1 はスピードラン用の Mod（SeedQueue など）を入れた構成で、バニラの挙動の確認には使えない
