# サバイバル向けゲーム内ツール（仮称）— 設計の引き継ぎ

## 目的

Toolscreen（https://github.com/jojoe77777/Toolscreen 、MCSR向け・C++・MIT）のサバイバル版に当たるものを作る。
Modではなくゲーム外のネイティブライブラリとして動かし、**Minecraftのバージョンに依存しない**ことを重視する。

### 欲しい機能（初期スコープ）
1. **座標の記録**：ホットキーで現在地を名前付きで保存する。**ワールド／サーバーごと**に管理する
2. **ズーム**：画面の中央付近を拡大表示する
3. **ゲーム内UI**：ウェイポイントの名前入力、一覧表示、選択

## 決定事項

- **方式**：Rust製のネイティブエージェント（検討した案の中の「②」）。Toolscreenのフォークや外部オーバーレイ方式は採用しない
- **言語**：Rust
- **検証環境**：まずWindows 10/11 x64（手元で検証できる）。将来はFedora（Linux）にも対応したい
- **対応バージョン**：Minecraft 1.13以降（LWJGL3 / GLFW を使うバージョン。F3+Cでの座標コピーも1.13から）。1.12以前は対象外

### 採用しなかった案（理由）
- Toolscreenのフォーク：ズームとImGuiのUIが既にあって早いが、コードがC++で規模が大きく、MCSR向けの前提が多く、Windows専用
- 外部オーバーレイ（Tauri＋透明ウィンドウ）：開発は楽だが、排他的フルスクリーンでは重ねられない
- Java Agent（`-javaagent`）／Fabric Mod：ゲーム内部のデータに触れるが、バージョン依存が強い

## アーキテクチャ

### 読み込み
- JVMの引数に `-agentpath:C:\path\to\xxx.dll` を追加する（Prism LauncherならインスタンスのJVM引数の欄）
- JVMTIの `Agent_OnLoad` が入口。外部からの注入用exeは使わない（ウイルス対策の誤検知も避けられる）
- `-agentpath:` はLinuxでも同じように使える

### フック（インラインのデトア。`retour` か `minhook` クレート）
IATフックではなくインラインのデトアにする。LWJGLは関数のアドレスを取得して保持し、そこから直接呼び出すと考えられるため（要検証）。

1. `Agent_OnLoad` で `LoadLibraryExW`（と `LoadLibraryW`）をフックする
2. `glfw.dll` が読み込まれた瞬間に、以下のGLFW関数をデトアする
3. `opengl32.dll` の `wglSwapBuffers` をデトアする

| フック対象 | 用途 |
|---|---|
| `wglSwapBuffers` | フレームの最後でズームとUIを描く |
| `glfwSetKeyCallback` / `glfwSetCharCallback` | Minecraftのコールバックを包む。ホットキーの横取り、UI表示中の入力遮断、キー入力の偽装 |
| `glfwSetMouseButtonCallback` / `glfwSetCursorPosCallback` / `glfwSetScrollCallback` | UI表示中はeguiに渡し、ゲームには渡さない（視点が回るのを防ぐ） |
| `glfwGetKey` | F3+Cの偽装時に「F3が押されている」と返す |
| `glfwSetClipboardString` | F3+Cの出力を横取りする（OSのクリップボードには渡さない） |
| `glfwSetInputMode` など | UIを開いている間はカーソルを表示し、閉じたら元に戻す |

### 座標の取得（F3+C）
1. ホットキーを押す
2. 保持しておいたMinecraftのキーコールバックを直接呼び、Cキーを押したことにする。その間は `glfwGetKey(F3)` にPRESSを返す
3. Minecraftが呼ぶ `glfwSetClipboardString` を横取りし、`/execute in minecraft:overworld run tp @s x y z yaw pitch` 形式の文字列を正規表現でパースする（ディメンションも取れる）
4. OSのクリップボードは汚れないので、退避や復元は不要
- 偽装したキー入力はメインスレッド（swapのフックの中など）で送る。GLFWのコールバックは通常メインスレッドで呼ばれるため

### ワールドの判定
- シングルプレイ：`saves/<ワールド>/session.lock` が更新・ロックされているワールドを今のワールドとみなす
- マルチプレイ：`logs/latest.log` の接続先ホストが書かれた行を監視する
- 保存先の案：インスタンスフォルダの `waypoints/<ワールド or サーバー>.json`

### 描画
- **同じHDCに自分専用のGLコンテキストを作り**、swapのフックの中でだけ切り替える
  ```
  game_ctx = wglGetCurrentContext()
  wglMakeCurrent(hdc, my_ctx)
  ズーム・UIを描く
  wglMakeCurrent(hdc, game_ctx)
  本物の wglSwapBuffers を呼ぶ
  ```
  → Minecraftの描画状態を壊さない。ゲームがCore ProfileかCompatibility Profileか（1.17の前後）にも左右されない
- **ズーム**：デフォルトのフレームバッファの中央部分を `glBlitFramebuffer` で画面全体に引き伸ばす
  - 将来：一時的に高い解像度で描画してから切り出す方式（遠くの景色を細かく見られる）
- **UI**：egui + egui_glow

### 日本語
- フォント：Noto Sans JPを埋め込むか、OSのフォント（游ゴシックなど）を読み込む
- IME：確定した文字はcharコールバック経由で届く。GLFWは変換中の文字を渡さないので、変換中の表示は当面あきらめる。将来 `WM_IME_COMPOSITION` などをフックして改善する

## 既知の制約
- 座標はF3+Cを送ったときにしか取れない。現在地を常時HUDに出すのは難しい（バージョンによってはF3+Cのたびにチャットにメッセージが出る）
  → 初期は「記録」「一覧」「ホットキーを押したときに距離と方角を表示」の範囲にとどめる
- サーバーによってはクライアント改変ツールが規約違反になりうる

## 未検証の前提（プロトタイプで確認すること）
- [ ] Windows上でLWJGLが `glfw.dll` をどう読み込むか（一時フォルダに展開するのか、Prismの「システムのGLFWを使う」設定のときはどうなるか、DLL名）
- [ ] LWJGLが関数のアドレスを保持して直接呼んでいるか（インラインのデトアで確実に捕まえられるか）
- [ ] MinecraftのF3+Cの判定が `glfwGetKey` でF3の状態を見ているか
- [ ] F3+Cの出力形式がバージョンによってどう違うか（1.13〜最新）
- [ ] 同じHDCに別のコンテキストを作って切り替える方式が、ドライバ（NVIDIA/AMD/Intel）ごとに問題なく動くか

## プロトタイプの手順
1. `-agentpath:` で読み込み、`wglSwapBuffers` をフックし、自分のコンテキストでeguiのウィンドウを1つ描く
2. ホットキーでそのウィンドウを開閉し、開いている間はマウスとキーボードの入力をeguiが受け取る（ゲームには渡さない）
3. F3+Cを偽装して横取りし、パースした座標をeguiのウィンドウに表示する

## 想定しているクレート構成
```
<project>/
├─ core/       # OSに依存しない：ウェイポイントの保存、F3+Cのパース、ワールドの判定
├─ render/     # glow + egui_glow：ズームとUI
├─ hook-win/   # cdylib：JVMTIの入口、LoadLibraryExWの監視、デトア、wgl
└─ hook-linux/ # 将来：LD_PRELOADでdlsymをフックする方式、または -agentpath:＋デトア。glXSwapBuffers / eglSwapBuffers
```

## 参考
- Toolscreen：https://github.com/jojoe77777/Toolscreen （wglフック、EyeZoom、ImGuiの組み込み方の参考）
- MangoHud：Linuxでの `dlsym` フックとGLオーバーレイの実例
- waywall：MCSR向けのLinux用Waylandコンポジタ（ズームの操作感の参考）
- `retour` / `minhook`、`egui` / `egui_glow` / `glow`、`jni-sys`（JVMTIの型）

---

## 追記：実装前のレビュー（2026-09-29）

資料を読んだうえで、実装に反映する点をまとめる。「確度が高い」ものは設計を変更し、「要確認」はプロトタイプで確かめる。

### 設計の変更（確度が高いもの）

1. **ズームは2段階で行う**
   読み込み元と書き込み先が同じデフォルトのフレームバッファで矩形が重なると、`glBlitFramebuffer` の結果はGLの仕様で未定義になる。
   中央部分をいったん自前のFBOに同じ大きさでコピーし、そのFBOから画面全体に引き伸ばす。
   ドライバの設定でデフォルトのフレームバッファにMSAAが強制されていても、同じ大きさのコピーなら解決（resolve）できる。
2. **コールバックを登録する関数のフックは、Minecraft側のポインタを返す**
   `glfwSetKeyCallback` などの戻り値（前のコールバック）には、自前のラッパーではなくMinecraftが登録したポインタを返す。
   LWJGLは終了時に `Callbacks.glfwFreeCallbacks` で戻り値のポインタを解放するため、ラッパーを返すと終了時にクラッシュするおそれがある。
3. **DLLの読み込みは `LdrRegisterDllNotification` で検知する**
   `kernel32` の `LoadLibraryExW` をフックすると、`LoadLibraryA` から kernelbase の内部を通る経路を取りこぼすおそれがある。
   `LdrRegisterDllNotification`（MSDNに載っている通知用のAPI）なら、デトアなしですべてのDLLの読み込みを拾える。
   - 通知はDLLがマップされた直後、`LoadLibrary` が戻る前に届く。LWJGLがGLFWの関数を呼ぶ前にフックを入れられる
   - `Agent_OnLoad` の時点では `opengl32.dll` も読み込まれていない
   - GLFWかどうかは、ファイル名ではなく `glfwInit` などをエクスポートしているかで判定する（Prismのシステムの GLFW、名前の違うDLLにも対応できる）
4. **LWJGLは関数のアドレスを保持して直接呼ぶ（ほぼ確定）**
   LWJGLは `GetProcAddress` で得たアドレスを `GLFW.Functions` の static final のフィールドに持つ。IATのフックでは捕まらず、インラインのデトアが必要という前提は正しい。
5. **swapのフックは `glfwSwapBuffers` にする**
   GLFWは gdi32 の `SwapBuffers` を呼び、`wglSwapBuffers` はその先で呼ばれる。
   GLFWの関数ならLinuxでも同じ場所でフックでき（GLX／EGLの違いを気にしなくてよい）、`GLFWwindow*` も直接取れる。
   自前のコンテキストは引き続きWGL（Linuxでは GLX／EGL）で作る。
6. **フックの中でパニックを外に出さない**
   Rustのパニックが `extern "C"` の境界を越えるとプロセスごと終了する（＝ゲームがクラッシュする）。
   すべてのフックを `catch_unwind` で包み、失敗したら元の関数にそのまま渡す。エラーはログに残し、オーバーレイを止めるだけにする。

### 入力まわりの方針

- UIを開いている間も、キーとマウスボタンを「離した」イベントはMinecraftに渡す。渡さないと、Wを押したままUIを開いて離したときに歩き続ける
- UIを開くときにカーソルを `DISABLED` から `NORMAL` に切り替えると、閉じたときにMinecraftが大きなマウスの移動と受け取り、視点が飛ぶことがある。
  閉じる前にカーソルの位置を戻すか、`DISABLED` のまま自前のカーソルを描いて相対移動で動かす方式を試す
- ホットキーは、カーソルが `DISABLED`（ゲームを操作している状態）のときだけ反応させる。チャットに入力している最中に誤って発動せず、この判定もバージョンに依存しない

### 要確認（Minecraft本体の挙動についての記憶。jarでは未確認）

- 文字入力には `glfwSetCharCallback` ではなく `glfwSetCharModsCallback` を使っていたはず → 両方をフックする
- F3+C について
  - ゲームルール `reducedDebugInfo` が有効なワールドやサーバーでは効かず、座標を取れない（既知の制約）
  - Minecraftの画面（チャット、インベントリ、ポーズなど）が開いている間は効かない
  - 1.21.9前後でF3系のキーをキー設定で変えられるようになったはずで、F3の判定が `glfwGetKey` ではなくキー割り当ての状態に変わっている可能性がある
  - 対策として「F3↓ → C↓ →（クリップボードへの書き込みを待つ）→ C↑ → F3↑」をコールバックで数フレームに分けて送り、`glfwGetKey(F3)` の偽装も併用する。これならどちらの判定でも動くはず
- マルチプレイの接続先は `logs/latest.log` の `Connecting to <host>, <port>` の行から取る。1.16以前はSRVレコードを解決した後のホスト名が出る可能性がある

### 未検証の前提（追加）

- [ ] `LdrRegisterDllNotification` の通知の中でデトアを入れて問題ないか（ローダーロックを持った状態）
- [ ] 同じHDCの2つのコンテキストの間で、ゲーム側の描画が終わる前にズームのコピーが走らないか（必要なら同期を入れる）
- [ ] `glfwSetCharModsCallback` を使っているか、1.21.9以降でF3の判定がどう変わったか

## 追記：プロトタイプ1で分かったこと（2026-09-29）

### フックのクレートは MinHook にした
`retour` 0.3.1 は、RIP相対のアドレスの後ろに即値が続く命令を正しく移せない（変位を「命令の最後の4バイト」とみなして書き換える。`retour/src/arch/x86/trampoline/mod.rs` の `instruction_bytes.len() - 4`）。
GLFWのAPI関数の多くは、最初の数バイトの中で初期化済みかを調べる `cmp dword [rip+_glfw.initialized], 0`（`83 3d <disp32> 00`）を実行する。
`glfwSwapBuffers` では先頭の命令そのもので、`glfwSetKeyCallback`・`glfwSetCharModsCallback`・`glfwGetKey` などでは `sub rsp,28h` の直後（+4バイト）にある（LWJGL 3.1.6／3.2.2／3.3.3 の glfw.dll で確認）。
どちらもフックで書き換える先頭5バイトにかかるため、トランポリンに移される。このため `retour` でフックすると、元の関数を呼んだ瞬間に不正なアドレスを読んで落ちる（Wine上で再現を確認）。
MinHook は即値の長さを差し引いて変位の位置を求めるので問題ない。Windows では `minhook` クレートを使う。Linux対応のときは別の方法（`retour` の修正版など）を検討する。

### Wine上で確かめたこと（`scripts/wine-smoke.sh`）
実機のWindowsではなく、Wine 9.0 と Mesa llvmpipe での結果。
- `-agentpath:` で `Agent_OnLoad` が呼ばれ、`LdrRegisterDllNotification` の通知がすべてのDLLについて届く
- LWJGLは `glfw.dll` を `GLFW` クラスの初期化時（`glfwInit` の前）に読み込む。ファイル名はいつも `glfw.dll`で、場所は起動方法で変わる
  - `-Dorg.lwjgl.system.SharedLibraryExtractPath` があればそのフォルダ（公式ランチャー 1.19以降の `natives`）
  - なければ `%TEMP%\lwjgl_<ユーザー>\<バージョン>\x64\glfw.dll`（3.2.2 は `%TEMP%\lwjgl<ユーザー>\3.2.2-build-10\glfw.dll`）
  - `java.library.path` で見つける場合はそのフォルダ（1.18以前の公式ランチャー、Prism、MultiMC）
- `opengl32.dll` は `glfwCreateWindow` の中で読み込まれる。`glfwTerminate` の後も `glfw.dll` と `opengl32.dll` は解放されない
- 通知の中でGLFWのエクスポートを調べてフックを入れられる（エクスポート表は `GetProcAddress` を使わず、PEのヘッダーから直接読む）
- 同じHDCに作った自前のコンテキスト（Compatibility Profile）で egui を描き、ゲームのコンテキスト（Core Profile 3.2）に戻せる。ゲーム側のGLのエラーは増えず、終了時の `glfwFreeCallbacks` でも落ちない

## 追記：Minecraft 26.x はGLFWを使っていない（2026-09-29）

実機（Prism Launcher、Minecraft 26.3、バニラ、Java 25 の `java-runtime-epsilon`）で試したところ、`glfw.dll` は読み込まれず、LWJGL 3.4.3 の `SDL3.dll` が読み込まれた。
ウィンドウと入力はGLFWからSDL3に替わったとみられる。あわせて `vulkan-1.dll`・`shaderc.dll`・`spirv-cross.dll`・`lwjgl_vma.dll` と、NVIDIAのOpenGLドライバ（`nvoglv64.dll`）も読み込まれている。
1.21.11 は従来どおりGLFWで起動する。

- 「1.13以降はGLFW」という前提は 26.x では成り立たない。26.x は個別に対応する
- 描画がOpenGLなら、`SDL_GL_SwapWindow` のフックに付け替えればほぼ同じ方式で描ける。Vulkanなら `vkQueuePresentKHR` をフックしてVulkanで描く必要があり、描画部分の作り直しになる
- どちらかを確かめるため、いまのエージェントはSDL3を検出すると `SDL_CreateWindow`（フラグ）・`SDL_GL_CreateContext`・`SDL_Vulkan_CreateSurface`・`SDL_GL_SwapWindow` を診断用にフックし、結果をログに出す（`renderer: OpenGL via SDL3` など）。描画はしない
- 検証用に `ci/smoke/SmokeSdl.java`（SDL3でOpenGLのウィンドウを作る）を追加し、CIとWine（`scripts/wine-smoke.sh --sdl --lwjgl 3.4.3`）で確かめている

### 26.3 の結果と対応（2026-09-29）
- 実機のログ：`Minecraft - RenderPearl OpenGL Hidden Utility Window` と `... Hidden Test Window`（どちらも OpenGL・非表示）を作り、補助ウィンドウで `SDL_GL_CreateContext` を1回呼ぶ。続いて `Minecraft 26.3`（OpenGL）を作り、毎フレーム `SDL_GL_SwapWindow` を呼ぶ。Vulkanのサーフェスは作られない
- つまり描画はOpenGL。コンテキストは非表示の補助ウィンドウで作り、ゲームのウィンドウで使っている
- 対応：描画部分（`hook-win/src/frame.rs`）をウィンドウのライブラリに依存しない形（`WindowSystem` トレイト）にし、GLFWとSDL3の両方から呼ぶ。SDL3では `SDL_GL_SwapWindow` をフックし、HWNDは `SDL.window.win32.hwnd` プロパティ、大きさは `SDL_GetWindowSizeInPixels`、倍率は `SDL_GetWindowDisplayScale`、非表示の判定は `SDL_GetWindowFlags` で取る
- `ci/smoke/SmokeSdl.java` は26.3と同じ手順（非表示の補助ウィンドウでコンテキストを作り、ゲームのウィンドウで使う）にした。Wineで表示を確認済み

## 追記：プロトタイプ1の実機確認（2026-09-29）

Windows、Prism Launcher、バニラ、NVIDIA GeForce RTX 4060 Ti（ドライバ 591.86）、2560×1440、表示スケール125%で確認した。

| Minecraft | ウィンドウ | ゲームのGL | 自前のGL | 1フレームの処理時間 |
|---|---|---|---|---|
| 1.21.11 | GLFW 3.4.0 | 3.3.0 Core Profile | 4.6.0 Compatibility | 0.23 ms |
| 26.3 | SDL 3.4.14 | 3.3.0 Core Profile | 4.6.0 Compatibility | 0.16 ms |

- どちらもタイトル画面にオーバーレイが表示され、日本語（游ゴシック）も正しく出た。FPS（60、垂直同期）は下がっていない
- 未検証の前提5（同じHDCに別のコンテキストを作って切り替える方式）は、NVIDIAでは問題なく動いた。AMD・Intelは未確認
- 数字と英字だけegui内蔵のフォントで描いていたため、日本語と基準線がずれた（「プロトタイプ 1」の「1」が下がる）。日本語フォントがあるときはそれを優先して使うようにした

## 追記：プロトタイプ2（入力・ホットキー・ズーム）の設計（2026-09-29）

- ホットキー：Ctrl+I でメニューを開閉（どの画面でも有効）、Esc で閉じる、Z を押している間ズーム（ゲーム中＝カーソルを捕まえているときだけ。チャットでは効かない）。設定の保存は未実装
- 入力の振り分けは `render/src/input.rs` の `InputRouter`（OSに依存しない。単体テストあり）。GLFWとSDL3のフックは、イベントを渡して「ゲームに渡す／横取りする」の答えに従うだけ
  - メニューを開いている間は、押したイベントを横取りし、離したイベントはゲームに渡す（押しっぱなしの状態が残らないように）
  - オーバーレイが描けていないとき（`overlay=off`、初期化の失敗、フレーム中のパニック）は何も横取りしない。見えないメニューが入力を奪うことはない
- マウス：カーソルを捕まえたまま（GLFW の `CURSOR_DISABLED`、SDL3 の相対マウスモード）、自前の矢印を描いて相対移動で動かす。ゲームのカーソルの状態は切り替えない
  - GLFW は捕まえたカーソルの位置を、どこまでも増える仮想の座標で報告し、ゲームは前回との差で視点を回す。メニューを開いている間に動いた量を覚えておき、閉じた後にゲームへ渡す座標から差し引く（閉じた瞬間に視点が飛ばない）。ゲームがカーソルを捕まえ直したり放したりしたら、この補正は捨てる
- GLFW：`glfwSet{Key,Char,CharMods,MouseButton,CursorPos,Scroll,WindowFocus}Callback` をフックし、GLFWには自前のラッパーを登録する。戻り値にはゲームが前に登録したポインタを返す（終了時の `glfwFreeCallbacks` 対策）。ゲーム中かどうかは `glfwGetInputMode(CURSOR)` で判定
- SDL3：`SDL_PollEvent` をフックし、横取りするイベントは取り除いて次のイベントを返す。メニューを開いている間は `SDL_StartTextInput` で文字入力（とIME）を有効にし、閉じたら元に戻す。ゲームが別の経路で入力を読む可能性があるので、`SDL_PeepEvents`・`SDL_WaitEvent(Timeout)`・`SDL_GetKeyboardState`・`SDL_GetMouseState`・`SDL_GetRelativeMouseState`・`SDL_AddEventWatch`・`SDL_SetEventFilter` を最初に使ったときと、`SDL_SetWindowRelativeMouseMode` の呼び出しをログに記録する
- ズーム：`render/src/zoom.rs`。中央部分を同じ大きさで自前のFBOにコピーしてから、画面全体に引き伸ばす（2段階）。eguiが有効のまま残すscissor testをblitの前に切る。GL 3.0 未満では使わない
- Wine上で `xdotool` により実際のキー・マウス操作を送って確認した（GLFW・SDL3の両方）：Ctrl+I で開閉、テキスト欄への入力、ゲームには離したイベントだけが届くこと、ズーム。カーソルを捕まえた状態のマウスの相対移動は、Wine+Xvfbでは届かないため未確認

### 実機での指摘と修正（2026-09-29）

メニューを開けることは実機で確認できた。指摘された 2 点を直した。

- **チェックボックスと文字の高さがずれる**：egui は、フォントの ascent・descent・line gap から行の高さを決め、ascent の位置に基準線を置き、行をチェックボックスの中央に合わせる。
  游ゴシックは line gap が大きいので、文字が行の上のほうに寄り、チェックボックスより高く描かれていた。
  日本語フォントを読み込むときに、egui と同じ値（skrifa の metrics）から「行の中央」と「文字の中央（基準線から 0.38 em 上）」の差を求め、`FontTweak::y_offset_factor` で文字を下げる（`render/src/font_metrics.rs`）。
  Wine で line gap の大きいフォント（IPA ゴシックの hhea を 0.88／−0.12／0.72 em に変えたもの）を使うと実機と同じずれが出て、修正後は中央に揃うことを確かめた。egui 内蔵のフォントでは差がほぼ 0（0.02 em）になる
- **メニューのカーソルの速度に Windows の設定が反映されない**：ゲームは、カーソルを捕まえている間、Windows のポインターの速度も加速もかかっていない生の移動量（raw input）を読む。自前のカーソルはこれで動かしていたので、デスクトップと速さが違った。
  メニューを開くたびに `SPI_GETMOUSESPEED`（1〜20）と `SPI_GETMOUSE`（「ポインターの精度を高める」）、加速の曲線（`HKCU\Control Panel\Mouse` の `SmoothMouseXCurve`／`SmoothMouseYCurve`）を読み、移動量に掛ける（`render/src/pointer.rs`、`hook-win/src/pointer.rs`）。
  計算は SDL3 の `SDL_HINT_MOUSE_RELATIVE_SYSTEM_SCALE` と同じ（精度を高めるがオフなら速度ごとの倍率、オンなら 1 回の移動量に応じた曲線。Windows 本来の計算の近似）
  - ゲームがすでに Windows の速度のかかった移動量を読んでいるときは掛けない：GLFW 3.3 以降で `GLFW_RAW_MOUSE_MOTION` がオフ（Minecraft の「Raw Input」がオフ）のとき、SDL3 でヒント `SDL_MOUSE_RELATIVE_MODE_WARP` か `SDL_MOUSE_RELATIVE_SYSTEM_SCALE` がオンのとき。
    `glfwRawMouseMotionSupported` のない GLFW（LWJGL 3.1.6 の 3.3.0 の開発版）は、捕まえたカーソルでは常に raw input を使う

## 追記：高精細のズーム・キーの変更・設定の保存（2026-09-29）

主要なズーム Mod と同じく、画面全体を拡大する（枠の中だけを拡大する形式ではない）。描かれた画面を引き伸ばすのではなく、細かいところまで見えるようにした。

### 高精細のズーム（`hook-win/src/tall.rs`）

Minecraft の視野角は縦方向で決まる。同じ幅で縦に k 倍長い解像度で描かせると、1 度あたりの画素が縦横とも k 倍になり、その中央の部分（元の画面と同じ大きさ）が k 倍に拡大した絵になる。FOV を変えるのと同じ効果を、ゲームのコードに触れずに得られる（スピードランの縦長の解像度と同じ）。

ズームキーを押している間だけ、次の 3 つを行う。

1. **ゲームに縦長の大きさを伝える**：GLFW はゲームが登録したフレームバッファの大きさのコールバックを呼び、`glfwGetFramebufferSize` の結果も差し替える。SDL3 は `SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED` と `SDL_EVENT_WINDOW_RESIZED` のイベントを `SDL_PollEvent` から渡し、`SDL_GetWindowSizeInPixels` の結果も差し替える。ゲームはふつうのリサイズと同じように描画先を作り直す（Minecraft は `resizeDisplay` の中でマウスの基準も取り直すので、視点は飛ばない）
2. **ゲームのウィンドウへの出力を自前のフレームバッファに受ける**：縦長の出力はウィンドウに入りきらず切り捨てられる。opengl32.dll が読み込まれた時点で `wglGetProcAddress` をフックし、ゲームには `glBindFramebuffer`・`glBindFramebufferEXT`・`glBlitNamedFramebuffer` の代わりに、フレームバッファ 0 を自前のもの（縦長の大きさ）に差し替える関数を渡す。LWJGL も SDL3 も GL 3.0 以降の関数はこの関数で探すので、バージョンに依存しない
3. **バッファの入れ替えのときに、自前のフレームバッファの中央をウィンドウにコピーする**（ゲームのコンテキストで。触った状態は元に戻す）

- ゲームが本当に縦長で描いたかは、`glViewport` もラップして確かめる（GL 1.1 の関数なので多くのドライバーでは `wglGetProcAddress` が null を返すが、それでもラッパーを渡す）。3 フレーム続けて縦長で描かれなかったら、そのセッションでは高精細をやめて、引き伸ばす拡大にする
- 大きさを変えている間にウィンドウの本当の大きさが変わったら、その通知はゲームに渡さずにズームを終え、新しい大きさを伝え直す
- ゲームがカーソルを放したら（Z を押したままインベントリを開いた）ズームを終える
- 描く画素が倍率ぶん増える（2560×1440 で 4 倍なら 2560×5760）。倍率の上限は `GL_MAX_TEXTURE_SIZE` などで決まる
- 押した瞬間と離した瞬間にゲームが描画先を作り直すので、一瞬引っかかることがある。そのため、なめらかに倍率を変えるアニメーションはできない
- 拡大している間は画面の下の HUD が見えない（縦長の画面の下端に描かれる）
- Wine で確認した：GLFW（LWJGL 3.3.3 のコアプロファイル、3.2.2 の互換プロファイル）と SDL3（3.4.3）の両方で、テスト用のプログラムが Minecraft と同じように自前のフレームバッファに描いて最後にウィンドウへ転送するとき（`--mc`）に、4 倍で細部まで拡大されること。ゲームがウィンドウに直接描くとき（フックを通らない）は 3 フレームで引き伸ばす拡大に切り替わること
- 26.x は実機のコードを見られていないので、ゲームがリサイズのイベントに反応するかが未確認

### キーの変更と設定の保存

- ホットキーはキー（修飾キー付き）かマウスのボタン（ホイールクリック、ボタン 4・5）。設定ファイルには `"Ctrl+I"`・`"Z"`・`"Mouse4"` のように文字で保存する（egui のキーの名前）。Esc と修飾キーだけは割り当てられない
- ホットキーの修飾キーは「押されていること」だけを見る。余分な修飾キーが押されていても反応する（Ctrl でダッシュしながらズームできる）。そのため、メニューのキーがズームのキーを隠してしまう組み合わせ（例：メニューが Z、ズームが Ctrl+Z）は受け付けない
- メニューでボタンを押すと、次に押したキーを割り当てる（入力の振り分けで横取りし、egui にもゲームにも渡さない）。Esc で取り消し
- キーの表は F1〜F25、記号、テンキーまで広げた（GLFW はアメリカ配列の位置、SDL3 は配列に従った文字）
- 設定はゲームフォルダの `reminedog/settings.json`（`core/src/settings.rs`）。知らない項目は無視し、ない項目は既定値にするので、バージョンが変わっても読める。壊れたファイルは `settings.corrupt-<時刻>.json` に移す。メニューで変えてから 1 秒たつか、メニューを閉じたときに保存する
