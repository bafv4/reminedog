# プロトタイプ 1 の確認手順

目的：`-agentpath:` で読み込んだ DLL が、Minecraft の画面に自前のコンテキストで egui のウィンドウを描けるか確かめる。
あわせて、資料の「未検証の前提」のうち、GLFW の読み込み方とドライバとの相性の情報を集める。

## 準備

1. `reminedog.dll` を入手する（[README](../README.md#dll-の入手)）
2. パスに日本語も空白（スペース）も含まない場所に置く（例：`C:\reminedog\reminedog.dll`。`C:\Program Files` は不可）
3. 確かめたいインスタンスの JVM 引数に `-agentpath:C:\reminedog\reminedog.dll` を追加する

## 確認すること

起動すると、画面の左上に「reminedog」というウィンドウが出るはず。

- [ ] タイトル画面でウィンドウが出る
- [ ] 日本語（「日本語の表示テスト：あいうえお・カタカナ・漢字」）が文字化けしない
- [ ] 「フレーム」の数字が増え続け、「FPS」がゲームの FPS とおおむね同じ
- [ ] ワールドに入っても表示が崩れず、ゲームの描画もおかしくならない
- [ ] F11 で全画面にしても、ウィンドウに戻しても表示される
- [ ] ウィンドウの大きさを変えても表示される
- [ ] 最小化して戻しても落ちない
- [ ] ゲームを普通に終了できる（終了時に落ちない）
- [ ] DLL を外したときと比べて、FPS が大きく下がらない（ウィンドウの「処理時間」が 1 フレームあたりの重さ）

ゲームが起動しなくなったときは JVM 引数から `-agentpath:...` を外し、ランチャーのログ（`Could not find agent library` などの行）を送ってほしい。

うまくいかないときは、JVM 引数に `=log=debug` を付けて（`-agentpath:C:\reminedog\reminedog.dll=log=debug`）もう一度試す。

## 送ってほしいもの

- 確かめた Minecraft のバージョン、ランチャー（Prism／公式）、Mod ローダーの有無
- GPU（NVIDIA／AMD／Intel）
- 上のチェックの結果（スクリーンショットがあると助かる）
- ゲームフォルダの `reminedog/reminedog.log`
  - Prism Launcher：インスタンスのフォルダの中の `minecraft`（または `.minecraft`）
  - 公式ランチャー：起動構成の「ゲームディレクトリ」。空欄なら `%APPDATA%\.minecraft`
  - 分からないときは、オーバーレイの「ゲームフォルダ」の行を見る

ログにはトークンなどの秘密の情報は書き出していない（コマンドラインは記録しない）。
ただし、ゲームフォルダや読み込んだ DLL のパスにはユーザー名が含まれることがある。

## ログの見方

| 行 | 意味 |
|---|---|
| `agent loaded` | JVM が DLL を読み込んだ |
| `DLL loaded: ...glfw.dll` | LWJGL が GLFW を読み込んだ場所（資料の未検証の前提 1） |
| `GLFW hooks installed` | `glfwSwapBuffers` をフックできた |
| `game GL: ...` | ゲームのコンテキスト（バージョン、GPU、core／compatibility） |
| `pixel format ...` | ウィンドウのピクセルフォーマット |
| `overlay GL: ...` | 自前のコンテキスト（資料の未検証の前提 5） |
| `overlay: first frame rendered` | 1 フレーム目を描けた |
| `[ERROR]` / `[WARN]` | 問題があった。内容を送ってほしい |

## Minecraft 26.x について

26.x は GLFW ではなく SDL3 でウィンドウを作るため、オーバーレイはまだ出ない。
代わりに、ゲームが OpenGL と Vulkan のどちらで描いているかをログに記録する。
26.x で一度起動し、ログの次の行を送ってほしい（26.x 対応の方針を決めるのに使う）。

| 行 | 意味 |
|---|---|
| `SDL3 loaded` | SDL3 を検出した |
| `SDL_CreateWindow(... flags ... [OpenGL])` | ウィンドウの作成時の指定（`OpenGL` か `Vulkan`） |
| `renderer: OpenGL via SDL3` / `renderer: Vulkan via SDL3` | 実際に作られたもの |

## テストしたい組み合わせ（できる範囲で）

| Minecraft | Java | LWJGL | 理由 |
|---|---|---|---|
| 1.16.5 | 8 | 3.2.2 | Java 8 の時代。Compatibility Profile |
| 1.20.1 | 17 | 3.3.1 | Core Profile。利用者が多い |
| 最新版 | 21 以降 | 3.3.3 以降 | 最新の GLFW |
