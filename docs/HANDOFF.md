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
