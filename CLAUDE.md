# reminedog：開発の引き継ぎ

Minecraft Java Edition（1.13 以降、26.x を含む）のサバイバル向けゲーム内ツール。
Mod ではなく、JVM に `-agentpath:` で読み込ませる Rust 製のネイティブのエージェント（JVMTI）。ゲームのウィンドウにオーバーレイ（egui）を描き、入力を横取りする。
まず Windows。Linux は後回し。

- 設計と経緯：[docs/HANDOFF.md](docs/HANDOFF.md)（最初の設計資料に、実装で分かったことを「追記」として順に足している）
- 使い方：[README.md](README.md)
- 実機で確かめる手順：[docs/TESTING.md](docs/TESTING.md)

## 利用者とのやり取り

- 利用者は日本語で話す。返事・ドキュメント・UI の文言は日本語。コードのコメント、コミットメッセージ、ログは英語
- 実機で確かめるのは利用者（Windows、NVIDIA RTX 4060 Ti、Prism Launcher）。確かめたバージョンは 1.21.11（GLFW 3.4.0）と 26.3（SDL 3.4.14）。
  1.21.11 には 2026-10-05 から Fabric と Sodium・Iris・Lithium が入っている（それより前の 1.21.11 での確認はバニラ）。26.3 はバニラ
- JVM 引数は `-agentpath:C:\reminedog\reminedog.dll`（先頭の `-` を忘れると起動しない）。ログは `<ゲームフォルダ>/reminedog/reminedog.log`、詳しいログは `=log=debug` を付ける
- 利用者に確かめてほしいことは、`docs/TESTING.md` のチェックリストに書いて渡す

## 構成

```
core/      OS に依存しない処理（テストあり）
  location.rs  F3+C のクリップボードの文字列のパース
  waypoint.rs  ワールドごとのウェイポイントの JSON（アトミックな保存）
  world.rs     ワールドの判定（latest.log の追跡、シングルプレイのワールド、level.dat のワールド名）。WorldId（マルチはラベル付き）
  session.rs   WorldWatcher（今のワールド）、WaypointBook（今のワールドのウェイポイント。変更のたびに保存。deferred なら SaveJob を別スレッドへ）、
               ServerLabels（サーバーのアドレスごとの最後のラベル）
  keybinds.rs  options.txt の F3+C のキー（修飾キー、座標のコピー、クラッシュ）と、コピーのキーを共有する操作、各キーの割り当て（bindings_by_key）。
               InputId（キーは SDL のスキャンコード、マウスは SDL の番号）と、名前（Naming で 1.21 と 26.x を読み分ける）・ラベル・GLFW／SDL のコードの変換
  keytable.rs  26.3 と 1.21.11 のキーの名前の表（jar から抜き出した TSV から生成）
  nav.rs       方角・距離、ネザー座標の変換、guide（ウェイポイントへの案内）
  options.rs   -agentpath:...=<オプション> の解析（gamedir, log, overlay, scale）
  gamedir.rs   ゲームフォルダの判定、reminedog/ 以下のパス
  settings.rs  settings.json（キー、ズームの倍率、キーの置き換えなど）
  logfile.rs   log クレートのファイル出力
  browser.rs   ゲーム内ブラウザの OS に依存しない部分：ページへの入力（PageInput と CDP の JSON）、アドレス欄の文字の URL、動画を操作する JS と通知の文言
render/    glow + egui_glow の描画（OS に依存しない、テストあり）
  overlay.rs   egui のメニュー、状態の表示、ヒント、自前のカーソル、Hotkeys。FrameInput/FrameOutput でフックとやり取り
  waypoints.rs メニューの「ウェイポイント」の欄、通知、方角と距離の文言。WaypointView/WaypointCommand でフックとやり取り
  input.rs     InputRouter：各入力を「ゲームに渡す／横取りする／置き換える」を決める。ホットキー（記録と更新は HotkeyAction のキュー。Minecraft の修飾キーを押している間はゲームに渡す）、キーの割り当て待ち、自前のカーソル
  rebind.rs    キーの置き換えの状態機械（Rebinder）。押したときに決め、リピートと解放はそれに従う。RebindState（押さえている出力、隠す元のキー、そのまま渡したルールの出力）
  rebinds_ui.rs  メニューの「キーの置き換え」の欄と resolve（settings のルール → router のルール）
  hotkey.rs    Hotkey（キーかマウスのボタン＋修飾キー）。設定ファイルの文字列との変換
  zoom.rs      引き伸ばすズーム（高精細が使えないとき）と、縦長の大きさの計算
  pointer.rs   Windows のポインターの速度・加速の計算（自前のカーソル用）
  font_metrics.rs  日本語フォントの縦位置の補正
  browser.rs   ゲーム内ブラウザ：メニューを開いている間の窓（移動、右下の角で大きさ、アドレス欄、ページへの入力の変換）、閉じている間のページの絵、メニューの欄、
               ページの絵のテクスチャ（PageTexture）。BrowserView/BrowserCommand/BrowserPixels でフックとやり取り
hook-win/  Windows のエージェント（reminedog.dll）
  lib.rs/agent.rs  Agent_OnLoad。オプション、ログ、パニックフック
  loader.rs    LdrRegisterDllNotification で DLL の読み込みを監視（glfw / SDL3 / opengl32）
  hook.rs      MinHook によるデトア
  glfw.rs, glfw_input.rs  GLFW（Minecraft 1.21 まで）：glfwSwapBuffers、コールバックの差し替え。F3+C（glfwSetClipboardString・glfwGetKey のフック、キーコールバックへの送信）。
                          キーの置き換え（出力をコールバックで送る、mods の補正、glfwGetKey の偽装）
  sdl.rs, sdl_input.rs    SDL3（Minecraft 26.x）：SDL_GL_SwapWindow、SDL_PollEvent のフィルタ。F3+C（SDL_SetClipboardText のフック、キーのイベントの注入）。
                          キーの置き換え（イベントをその場で書き換える、mod の補正、SDL_GetKeyboardState の偽装）
  rebind_state.rs  ゲームから見えるキーの状態の表（ロックなし、512 項目）と、元のキーの解放を取り逃したときの安全網
  frame.rs     スワップのたびの処理：自前の WGL コンテキストに切り替えてオーバーレイを描く。設定の読み込みと保存（saver へ）、メニューのクリップボード
  saver.rs     ファイルの書き込みのスレッド（設定・地点・ラベル。渡した順に書き、結果は Pending で受け取る）
  clipboard.rs メニューの貼り付け・コピー用の Win32 のクリップボード（GLFW・SDL の関数は F3+C でフックしているので使わない）
  f3c.rs       F3+C の要求と結果（ウィンドウのライブラリに依存しない）。送った F3+C の書き込みは握りつぶし、利用者の F3+C は通す
  waypoints.rs ワールドの追跡、options.txt（デバッグのキーと、メニューに出す各キーの割り当て）、F3+C の要求と結果、通知（frame.rs から毎フレーム）
  tall.rs      高精細のズーム（下で説明）
  wgl.rs       opengl32 の関数表、自前のコンテキスト
  pointer.rs   Windows のマウスの設定の読み取り
  fonts.rs     日本語フォント（游ゴシック → メイリオ → MS ゴシック）。読み込みの直後に別スレッドで読み、プロセスの間持つ
  browser.rs   ゲーム内ブラウザ（WebView2）のゲームのスレッド側：表示・非表示・終了、コマンドのメールボックス、状態、最新の絵（FRAME）。frame.rs から毎フレーム
  browser/     ブラウザのスレッド（thread.rs）、WebView2 の作成・設定・イベント（webview.rs）、画面の取り込み（capture.rs）。MSVC のビルドだけ（GNU では外す）
installer/ インストーラー（Kotlin、Swing＋FlatLaf、Gradle）。Java 8 以降で動く 1 つの jar（reminedog-installer.jar）
  Main.kt            FlatLaf（Windows のダークモードに合わせる）と画面の起動
  InstallerFrame.kt  画面：DLL（GitHub からダウンロードするか、PC のファイル）、インスタンスの一覧と状態、インストール／アンインストール、
                     最新版に更新（インスタンスが読み込んでいる DLL を、その場所のまま置き換える）、ログ
  AgentArg.kt        JVM 引数の中の -agentpath:...reminedog*.dll を探す・置き換える・外す（ほかの部分は文字のまま残す）。使えないパスの判定
  Launcher.kt        Launcher（ランチャーのデータのフォルダ）・Instance・Args・Change
  MojangLauncher.kt  公式ランチャー（launcher_profiles.json の javaArgs）
  MultiMcLauncher.kt MultiMC と Prism Launcher（instance.cfg の JvmArgs・OverrideJavaArgs）
  McsrLauncher.kt    MCSR Launcher（instance.json の options）
  Launchers.kt       いつもの場所の検出と、選んだフォルダの判定
  Ini.kt, Json.kt    設定ファイルの読み書き（Qt の INI と MultiMC の INI を行単位で、JSON はキーの順序と数値の書き方を保つ）
  Download.kt        GitHub の最新のリリースの reminedog.dll（サイズ・SHA-256（必須）・MZ を確かめてから置き換える）。replaceAll で複数の DLL を 1 回のダウンロードで置き換える
  Acl.kt             新しく作るフォルダの ACL（自分・SYSTEM・Administrators）と、ほかのユーザーが書ける場所の判定
  Processes.kt       起動中のランチャーの判定（%SystemRoot%\System32\tasklist.exe、裏のスレッドで）
ci/smoke/  LWJGL で Minecraft と同じようにウィンドウを作るテスト用の Java（GLFW 版 Smoke、SDL3 版 SmokeSdl）。F3+C の真似（--world、--f3c-refuse、--f3c-events）。
           キーの置き換えの確認（SMOKE_VERBOSE=1 で受け取ったイベント、--screen-key・--watch-keys で画面を閉じたときのキーの状態）
scripts/   wine-smoke.sh（Linux 上で Wine を使って動かす）、fetch-wine-jre.sh
```

## ビルドとテスト

Windows で：

```
cargo build --release -p reminedog-hook-win      # target\release\reminedog.dll
cargo test --workspace
cargo clippy --workspace --all-targets
cd installer; .\gradlew.bat build                  # インストーラーのテストと installer\build\libs\reminedog-installer.jar
```

- Rust 1.95 以降（egui 0.36 の要件。今は 1.99。CI とリリースはワークフローの `RUST_TOOLCHAIN` で固定）、edition 2024
- MSVC では CRT を静的にリンクする（`.cargo/config.toml` の `+crt-static`。Java 8 に vcruntime140 がないため）
- Linux からは `--target x86_64-pc-windows-gnu` で DLL をビルドし、`scripts/wine-smoke.sh` で試せる（README の「開発」）。Windows では DLL を置いてゲームで確かめるのが早い
- テスト用プログラムを Windows で直接動かすなら、`ci/smoke/*.java` を LWJGL の jar と一緒にコンパイルし、`java -agentpath:...\reminedog.dll -cp ... Smoke --seconds=20 --capture --mc` のように動かす（`.github/workflows/ci.yml` の `smoke-windows` ジョブが手順の見本）。
  Prism の libraries に LWJGL のネイティブの jar がないときは、ゲームが展開したもの（`-Dorg.lwjgl.librarypath=%TEMP%\lwjgl_yuanq\3.3.3+5\x64` など）を使う
  - ズームは、Z の WM_KEYDOWN／WM_KEYUP をスモークのウィンドウに `PostMessage` すれば試せる（利用者の入力に触れない）。`=log=debug` の `zoom:` の行と、`PrintWindow` で撮ったウィンドウで見る。
    `--sodium`（`--mc` と一緒に）で、Sodium と同じく直前と同じ値の `glViewport` を省く
  - ウェイポイントを試すなら `--world=<名前>` を付ける（作業フォルダに latest.log と saves を書く）。記録できれば `reminedog/waypoints/sp-<名前>.json` ができ、終了時の行が `F3C STATE overlay=off modifier=up copies=1` になる。拒否は `--f3c-refuse`。詳しくは HANDOFF.md の「追記：プロトタイプ 3」
  - 本物の F3+C を送るときは、PowerShell の `SendKeys` ではなく `Add-Type` で `SendInput` を使う（`SendKeys` では F3 を押したまま C を押せない）
  - キーの置き換えを試すなら、ゲームフォルダ（`gamedir=` か作業フォルダ）の `reminedog/settings.json` にルールを書いてから起動する（例：`{"rebinds": [{"from": "key.keyboard.b", "to": "key.keyboard.w"}]}`）。
    `SMOKE_VERBOSE=1` でゲームが受け取ったイベント（`KEY`・`BUTTON`・`FOCUS`）が出る。偽装は `--seconds=30 --capture --screen-key=69 --watch-keys=87,66`（SDL3 は `--seconds=30 --capture --screen-key=8 --watch-keys=26,5`。`--seconds` がないとすぐ終わる）で、B を押したまま E を 2 回押して `SETALL 87=1 66=0` になるかで見る。詳しくは HANDOFF.md の「追記：キーの置き換え」
- ブラウザ（WebView2）のテストは `#[ignore]`（WebView2 ランタイムと画面のあるセッションが要る）：`cargo test -p reminedog-hook-win browser -- --ignored --test-threads=1`
  - スモークで試すなら settings.json に `"browser_toggle_key": "B"`（とメニューを `"menu_key": "M"`。`PostMessage` では Ctrl を押せない）を書き、`--capture` 付きで B を送る。
    PageUp／PageDown・矢印などの拡張キーを `PostMessage` で送るときは lParam の 24 ビット目を立てる（立てないと GLFW がテンキーとして受け取る）。メニューのマウス操作は `--capture` なしで `WM_MOUSEMOVE`・`WM_LBUTTONDOWN` を送る
- インストーラーは Gradle 9.8（ラッパー。JDK 17 以降で動く。手元は JDK 25）、Kotlin 2.4、FlatLaf 3.7。Kotlin は `jvmTarget` 1.8 と `-Xjdk-release=1.8`（Java 8 の API だけを使う）、警告はエラー。
  画面を確かめるなら、利用者の設定ファイルの写しを scratchpad に作り、`APPDATA`・`LOCALAPPDATA` をそこに向けて起動する（本物のランチャーの設定を書き換えない）
- CI（GitHub Actions。push は main だけ、ほかは pull request）：Linux（fmt、clippy、テスト、mingw での clippy）、インストーラー（ラッパーの確認、テスト、成果物 `reminedog-installer`）、Windows（clippy、テスト）、
  Windows の DLL（リリースビルド、`ci/check-dll.ps1` で x64・export・読み込む DLL の許可リスト・バージョン情報、成果物 `reminedog-windows-x64`）、Windows のスモークテスト（LWJGL 3.2.2/Java 8、3.3.3/Java 21、3.4.3/Java 25 の SDL3。Mesa の llvmpipe で描く。
  Mesa は `MESA_TAG` と `MESA_SHA256`、LWJGL の jar は `ci/smoke/lwjgl.sha256` で確かめる）。アクションはコミットの SHA で固定（`.github/dependabot.yml`）、cargo は `--locked`。
  DLL が新しい DLL を読み込むようになったら check-dll.ps1 の許可リストに足す（どの Windows にもあるものだけ）
- リリース（`.github/workflows/release.yml`）：Actions から版を入れて、main で手で実行する（そのコミットで CI が通っていること。キャッシュは使わない）。版は環境変数 `REMINEDOG_VERSION`（DLL：`agent::VERSION` とログ・状態の行、`hook-win/build.rs` が embed-resource で埋めるバージョン情報）と Gradle の `-PreminedogVersion`（jar の名前と manifest の `Implementation-Version`、画面のタイトル）で入れる。
  リポジトリの `Cargo.toml` の版は変えない（版を渡さないビルドでは、DLL は Cargo の版、インストーラーは `dev`）。成果物は `reminedog-<版>.dll`・`.pdb`・`reminedog-installer-<版>.jar` の下書きのリリース。利用者の PC では DLL は `reminedog.dll` のまま（インストーラーが名前を変えて置く。`Download` は `reminedog-<版>.dll` を探す）

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
- インストーラーはランチャーの設定ファイルを書き換える。変えるのは JVM 引数の reminedog の部分と、それを効かせるためのキーだけで、ほかの行・キー・引数はそのまま残す。書き込みは一時ファイルからの置き換えで行い、試すときは設定ファイルの写しを使う
- キーの置き換えは押したときに決め、その押下のリピートと解放は押したときの決定に従う（ルール・画面・メニューがその後で変わっても）。ゲームの中でキーが押されたまま残らないようにする。送る F3+C は置き換えを通さない
- ゲーム内ブラウザ（WebView2）は、ゲームの前にウィンドウを出さず、OS のフォーカスを取らない。ページへの入力は CDP で送る（`SendMouseInput`・`MoveFocus`・`window.focus()` は使わない）。
  右クリックメニュー・ダイアログ（ファイルの選択も）・新しいウィンドウ・ダウンロード・外部のアプリを開くリンクは止めるか同じビューで開く。ページの URL はログに出さない（クエリにトークンが入りうる。ホスト名だけ）
- ゲームが読むキーの状態（`glfwGetKey`、`SDL_GetKeyboardState`、キーのイベントの修飾キー、F3+C の確認）は `rebind_state` の表に合わせる。表は、イベントをゲームに渡す前（とフォーカスを失ったとき、安全網の後）に router のロックを持ったまま書き直し、ゲームのコードはロックを外してから呼ぶ。表を読む側はロックを取らない（ゲームが頻繁に読む）

## 今の状態（2026-10-07）

| 機能 | 状態 |
|---|---|
| オーバーレイ（egui、日本語フォント） | 実機で確認済み（1.21.11、26.3） |
| メニュー（Ctrl+I）、入力の横取り、自前のカーソル（Windows のポインターの速度を反映） | 実機で確認済み |
| チェックボックスと文字の縦位置の補正 | 実機で確認済み |
| 高精細のズーム（`tall.rs`） | 実機で確認済み（1.21.11、26.3）。Sodium を入れた 1.21.11 で崩れていたのを直した（スモークの `--sodium` と実機で確認） |
| キーの変更、settings.json の保存 | 倍率の保存は実機で確認済み。キーの変更は Wine のみ |
| F3+C によるウェイポイント（J で記録、K で方角と距離、メニューの一覧） | 手元のスモーク（GLFW・SDL3）で確認済み。実機では未確認 |
| キーの置き換え（キーとマウスのボタン、ゲーム中だけ。メニューの「キーの置き換え」） | 手元のスモーク（GLFW・SDL3）で確認済み。実機では未確認 |
| ゲーム内ブラウザ（WebView2。メニューで表示・移動・大きさの変更・ページの操作、ゲーム中の PageUp／PageDown と ↓←→ の動画の操作） | WebView2 を動かすテストと手元のスモーク（GLFW）で確認済み。実機では未確認 |
| インストーラー（公式ランチャー、MultiMC、Prism Launcher、MCSR Launcher） | 設定ファイルの写しに対して画面から操作して確認済み、Java 8・17・21・25 で表示を確認。本物のランチャーでは未確認。ダウンロードと「最新版に更新」はリリースがないので失敗する（置き換えは単体テストで確認） |
| 監査（`docs/AUDIT-2026-10-09.md`）の全件の修正 | テスト・clippy・インストーラーのビルドで確認。実機では未確認（`docs/TESTING.md` の「監査の修正で確認すること」）。設計は HANDOFF.md の「追記：監査の修正」 |

### 高精細のズームの仕組み（`hook-win/src/tall.rs`）

1. ズームキーを押している間、ゲームにフレームバッファが縦に k 倍長いと伝える（GLFW：ゲームのフレームバッファの大きさのコールバックをスワップの後に呼び、`glfwGetFramebufferSize` を差し替え。SDL3：`SDL_PollEvent` から大きさの変更のイベントを渡し、`SDL_GetWindowSizeInPixels` を差し替え）
2. opengl32.dll が読み込まれたときに `wglGetProcAddress` をフックし、ゲームに `glBindFramebuffer(EXT)`・`glBlitNamedFramebuffer` のラッパーを渡す。ズーム中はフレームバッファ 0 を自前の縦長のフレームバッファに差し替える
3. スワップのときに、その中央をウィンドウに blit する
4. `glViewport` のラッパーで、ゲームが本当に縦長で描いたかを確かめる（ズームを始めてから縦長の高さの viewport を一度でも設定したか。Sodium は直前と同じ値の `glViewport` を省くので、フレームごとには見ない）。
   3 フレーム以上かつ 1 秒以上続けて描かれなければ、引き伸ばすズームに切り替える（描画先の作り直しで数フレーム止まっても見切らないように）。
   縦長で描かれなかったフレームでは、ゲームが自前のフレームバッファに描いた絵をウィンドウに写してから引き伸ばす（ウィンドウには古い絵しか残っていないため）

26.3 の実機では、ゲームは縦長で描いていたが幅が 2560（SDL が報告するウィンドウは 2561 px）で、幅まで一致を求める判定に落ちて引き伸ばすズームになっていた。判定を高さだけにして直し、実機で確認した。26.3 はウィンドウの大きさを `SDL_GetWindowSizeInPixels` で問い合わせず、イベントの値を使う。`=log=debug` の `zoom: frame not rendered ...` の行に、`glViewport` の回数・最大の大きさ・スワップ時の viewport・大きさの問い合わせの回数が出る。

### ウェイポイントの仕組み（`hook-win/src/f3c.rs`、`waypoints.rs`）

1. J・K（またはメニューのボタン）で、`waypoints.rs` が F3+C の要求を出す。キーは options.txt（修飾キー・座標のコピー・クラッシュ）から読む（5 秒ごとと要求の直前に更新時刻と大きさを見て、変わっていれば）
2. GLFW はスワップの後にゲームのキーコールバックを直接呼び、SDL3 は `SDL_PollEvent` の注入のキューから、修飾キーの押下 → コピーの押下 → コピーの解放 → 修飾キーの解放を送る（GLFW は 1.16 向けに、その間だけ `glfwGetKey` に修飾キーを押していると答える）
3. その間の `glfwSetClipboardString`／`SDL_SetClipboardText` を横取りして `parse_f3c` で読み、OS には渡さない。書き込みがなければ拒否（デバッグ情報の制限）で、F3 画面を戻すために修飾キーをもう一度押して離し、そのワールドでは送るのをやめる
4. 座標は `WaypointBook`（ワールドは `WorldWatcher`。マルチは `ServerLabels` のラベル付き）に入れ、保存は `saver` のスレッドで行い、通知と方角・距離（`guide`）を出す。
   利用者自身の F3+C も、ゲームから見て修飾キーを押しているときにコピーのキーをゲームに渡す間の書き込みから拾う（こちらは OS に渡す）。
   ゲーム中（前のポーリングからずっとカーソルを捕まえていた）に読んだワールドの行は捨てる（サーバーの文字で偽の行を作れるため）

ゲームの挙動（1.16.1、1.21.11、26.3 の jar で確かめたこと）と、確かめていないことは HANDOFF.md の「追記：プロトタイプ 3」。

### キーの置き換えの仕組み（`render/src/rebind.rs`、`hook-win/src/rebind_state.rs`）

1. ルールは settings.json の `rebinds`（26.x のキーの名前）。`resolve` が使えないもの（ライブラリが扱えないキーを含む）を除いて router に渡す（`set_rebinds`。ルールかホットキーが変わったときだけ）。除いた理由はメニューの行の下に出る
2. router は、ホットキーとメニューを今までどおり物理のキーで判定し、ゲームに渡す押下だけを `Rebinder` に渡す。ゲーム中（カーソルを捕まえていて、メニューを閉じている）にルールのある元のキーなら `Delivery::Send(出力)`。リピートと解放は、押したときの記録に従う
3. GLFW は出力をゲームのキーかマウスのボタンのコールバックで送り、SDL3 は `SDL_PollEvent` のイベントをその場で書き換える。その前に `rebind_state` の表を書き直し、`glfwGetKey`／`SDL_GetKeyboardState` とキーのイベントの修飾キーを「元のキーは離している、出力は押している」に合わせる（画面を閉じたときの `KeyMapping.setAll`、Ctrl+Q、クラッシュのキーのため）
4. フォーカスを失ったら出力を離し、その後に届く元のキーの解放は捨てる。解放が届かないとき（IME に取られた、フォーカスを失っている間に離した、26.x がワールドの読み込み中や「セーブしてタイトルへ戻る」の間に入力のイベントを捨てた など）は、スワップの後の安全網が `GetAsyncKeyState` で 100 ms 離れているのを見て、元のキーなら置き換え先を離す。
   ルールの出力と同じ id をそのまま渡した押下（`Passthrough`）なら、ゲームに何も送らずに忘れる（残すと、その id へのルールの押下が捨てられる）。解放が来ないことがある JIS の IME のキー（GLFW の key -1 のキーもこれ）は、元のキーにできない

ゲームとライブラリの挙動（jar と GLFW・SDL のソースで確かめたこと）、設計の理由、確かめていないことは HANDOFF.md の「追記：キーの置き換え」。

### ゲーム内ブラウザの仕組み（`hook-win/src/browser.rs`、`render/src/browser.rs`）

1. 最初に表示したときに、専用のスレッド（STA、DispatcherQueue、表示しない親のウィンドウ）で WebView2 を visual hosting で作る。`RootVisualTarget` は Windows.UI.Composition の `ContainerVisual`
2. その visual を `GraphicsCaptureItem::CreateFromVisual` で取り込み、D3D11 のステージングテクスチャから `FRAME` に写す。ゲームのスレッドはスワップのときに `FRAME` を `try_lock` し、新しい絵を `glTexSubImage2D`（BGRA）で上げて egui で描く
3. メニューを開いている間は egui の窓で、ページのマウス・ホイール・キー・文字を CDP（`Input.*`）で送る（座標は CSS ピクセル。egui の座標を拡大率で割る）。閉じている間は絵だけ
4. ゲーム中のキーは `BrowserAction` のキュー。PageUp／PageDown はページの中央でホイールを 1 画面ぶん回す（キーはページを一度クリックするまで効かない）。動画は `ExecuteScript`。非表示で再生中のものを止め、表示で再開する

設計の理由、採らなかった案、確かめたことと確かめていないことは HANDOFF.md の「追記：ゲーム内ブラウザ」。

## 次にやること（候補）

1. ウェイポイント、キーの置き換え、高精細のズーム、キーの変更、インストーラー、ゲーム内ブラウザを実機で確かめてもらう（`docs/TESTING.md` の「インストーラーで確認すること」「キーの置き換えで確認すること」「ウェイポイントで確認すること」「ズームとキーの変更で確認すること」「ブラウザで確認すること」）。問題があれば `=log=debug` のログの `F3+C:`・`world:`・`rebinds:`・`zoom:`・`browser:` の行から直す
   - インストーラーのダウンロードには、公開されたリポジトリの公開済みのリリース（`release.yml` で作った下書きを公開したもの）が要る（今は非公開でリリースもない）
2. CI のスモークテストで F3+C とキーの置き換えを確かめる（`--world`・`--f3c-refuse`、settings.json のルールと `--screen-key`・`--watch-keys` を使い、マーカーに `F3+C: clipboard hooks ready`・`SETALL` などを足す。今の CI は新しいオプションを使っていない。`scripts/wine-smoke.sh` もまだ渡せない）
3. IME の変換中の文字をメニューの入力欄に出す（今は確定した文字だけ）
4. Linux（Fedora）対応。方針の案は HANDOFF.md の構成の `hook-linux/`（GLFW の関数をフックする、自前のコンテキストは GLX／EGL。MinHook は使えないのでデトアの方法を検討する）

## 開発環境についての注意

- Wine＋Xvfb では、カーソルを捕まえた状態でのマウスの相対移動が届かない（自前のカーソルの動きは Wine では確かめられない）
- `scripts/wine-smoke.sh --grab` は X の画面全体を保存する。SDL3 版のウィンドウは画面の中央寄りに開く
- 利用者のインスタンスのクライアントの jar は `%APPDATA%\PrismLauncher\libraries\com\mojang\minecraft\<版>\minecraft-<版>-client.jar` にある（1.16.1、1.21.11、26.3）。
  26.3 は難読化されていない。1.21.11 と 1.16.1 は難読化されているので、クラスは文字列の定数から探す（`javap -c -p -constants`）。展開したものはリポジトリの外（scratchpad）に置く
- 利用者の 1.16.1 はスピードラン用の Mod（SeedQueue など）を入れた構成で、バニラの挙動の確認には使えない。
  1.21.11 も 2026-10-05 から Fabric＋Sodium 0.8・Iris 1.10（シェーダーパックは未選択で無効）・Lithium・fabric-regrowth を入れていて、フルスクリーン、GUI の大きさ 5。
  1.21.11 の描画の不具合は、まず Sodium の変更を疑う（Mod の jar は `minecraft\mods` にある。調べるときは scratchpad に展開して `javap`）
- 利用者のキー設定（チェックリストを書くとき）：26.3 は「アイテムを捨てる」が C、ホットバー 1 が Q、「オフハンドと交換」が CapsLock、ダッシュが M（トグル）、チャットが Backspace。1.21.11 は捨てるが Q、ダッシュが左 Ctrl、「ホットバーの保存」が C。どちらでも空いているキーは B・H・N・Y（B・H・N は F3 との組み合わせだけ。U は 1.21.11 では Iris の「シェーダーの再読み込み」、K は「シェーダーの切り替え」）。キーボードは US 配列（kbd101）なので、JIS のキーは確かめられない
- SendInput でスモークに送るときは、利用者が操作していないこととスモークのウィンドウが前面にあることを確かめてから送る（`--screen-key` ではカーソルをウィンドウの中央に動かす）。PC がロックされていると SendInput は届かない（自分のウィンドウへの PostMessage なら届くが、SDL3 はキーボードのフォーカスがないとキーのイベントの windowID が 0 になる）。
  右 Ctrl・右 Alt・矢印・Insert などの拡張キーには `KEYEVENTF_EXTENDEDKEY` を付け、マウスの X ボタンは `mouseData` で指定する。CapsLock を送ったら、ロックの状態を元に戻す
