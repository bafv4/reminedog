# reminedog

Minecraft（Java Edition 1.13 以降）のサバイバル向けゲーム内ツール。
Mod ではなく、JVM に `-agentpath:` で読み込ませるネイティブのライブラリとして動くので、Minecraft のバージョンに依存しない。

設計は [docs/HANDOFF.md](docs/HANDOFF.md) を参照。

## 現在の状態

**プロトタイプ 3**：ゲーム中に J で今いる場所をウェイポイントとして記録し、K で目的地（メニューで選んだ地点）への方角と距離を出す。座標は Minecraft の F3+C で取る。
Ctrl+I でメニューを開き、マウスとキーボードで操作できる。ゲーム中に Z を押している間、画面の中央を拡大する（ゲームに縦長の解像度で描かせるので、拡大しても細かいところまで見える）。キーはメニューで変えられ、設定は保存される。
確認の手順は [docs/TESTING.md](docs/TESTING.md) にある。

| 機能 | 状態 |
|---|---|
| オーバーレイの描画（egui） | プロトタイプ 1。実機（1.21.11 と 26.3、NVIDIA）で確認済み |
| 入力の横取り・メニュー | プロトタイプ 2。実機（1.21.11 と 26.x）で確認済み |
| 高精細のズーム・キーの変更・設定の保存 | 高精細のズームは実機（1.21.11 と 26.3）で確認済み。倍率の保存も実機で確認済み、キーの変更は Wine でだけ確認 |
| F3+C によるウェイポイント（記録、一覧、方角と距離） | プロトタイプ 3。テスト用のプログラムで確認済み、実機では未確認 |
| Minecraft 26.x | 対応（ウィンドウが GLFW ではなく SDL3 になったため、SDL3 の `SDL_GL_SwapWindow` をフックする）。26.3 で確認済み |

## 構成

```
core/      OS に依存しない処理：F3+C のパース、options.txt のキー設定、ウェイポイントの保存、ワールドの判定、方角と距離、ログ
render/    glow + egui_glow によるオーバーレイの描画
hook-win/  Windows 用のエージェント（reminedog.dll）：DLL の読み込みの監視、デトア、WGL
ci/smoke/  LWJGL で Minecraft と同じように GLFW のウィンドウを作るテスト用のプログラム
scripts/   Linux 上で Wine を使ってエージェントを動かすスクリプト
```

## DLL の入手

- **GitHub Actions**：リポジトリの Actions タブ → 最新の `CI` の実行 → Artifacts の `reminedog-windows-x64` をダウンロードする
- **自分でビルドする**（Windows）：[Rust](https://rustup.rs/) と Visual Studio Build Tools（C++ によるデスクトップ開発）を入れて、次を実行する

  ```
  cargo build --release -p reminedog-hook-win
  ```

  `target\release\reminedog.dll` ができる。

## 使い方

必要なもの：Windows 10 / 11 と 64 ビット（x64）版の Java（Minecraft のランチャーが使う Java は通常これ）。
Arm 版 Windows の arm64 版 Java では読み込めない。

JVM は `-agentpath:` の DLL を読み込めないと起動をやめるので、ゲームが起動しなくなったときは JVM 引数から外す。

1. `reminedog.dll` を、パスに日本語も空白（スペース）も含まない場所に置く（例：`C:\reminedog\reminedog.dll`）。
   JVM 引数は空白で区切られるので、`C:\Program Files` や空白を含むユーザー名のフォルダは使えない（引用符で囲む方法はランチャーによって効かない）
2. JVM 引数に次を追加する
   - Prism Launcher：インスタンスの「編集」→「設定」→「Java」→「JVM 引数」
   - 公式ランチャー：「起動構成」→ 構成の「編集」→「その他のオプション」→「JVM 引数」

   ```
   -agentpath:C:\reminedog\reminedog.dll
   ```
3. ゲームを起動する。ログはゲームフォルダの `reminedog/reminedog.log` に出る
   （Prism Launcher ならインスタンスの `minecraft` フォルダ。公式ランチャーなら起動構成の「ゲームディレクトリ」で、空欄なら `%APPDATA%\.minecraft`）。
   実際のフォルダはオーバーレイの「ゲームフォルダ」の行とログの `game dir:` の行に出る

### 操作

| キー（既定） | 動作 |
|---|---|
| Ctrl+I | メニューを開く／閉じる（開いている間、マウスとキーボードはメニューが受け取る） |
| Esc | メニューを閉じる |
| Z（ゲーム中に押している間） | 画面の中央を拡大する。倍率はメニューで変えられる |
| J（ゲーム中） | 今いる場所をウェイポイントとして記録する（メニューでは「ウェイポイントを記録」） |
| K（ゲーム中） | 現在地を取り直し、目的地への方角と距離を出す（メニューでは「現在地を更新」） |

「ゲーム中」は、カーソルが消えていて、Minecraft の画面（チャット、インベントリなど）も reminedog のメニューも開いていないとき。
Ctrl+I・Z・J・K は、メニューの「キー」でほかのキーやマウスのボタン（ホイールクリック、ボタン 4・5）に変えられる。
F3 を押している間は、Z・J・K をゲームに渡す（F3 との組み合わせはそのまま使える。Minecraft のキー設定で F3 の代わりのキーを決めていれば、そのキー）。

メニューの設定はゲームフォルダの `reminedog/settings.json` に保存される。

#### ウェイポイント

- J を押すと、今いる場所を「地点 1」「地点 2」…の名前で記録し、画面の上のほうに「「地点 1」を記録した（12, 64, -7）」と出す。
  ワールドごと（シングルプレイはワールドのフォルダ、マルチプレイはサーバー）に、ゲームフォルダの `reminedog/waypoints/` に保存する
- メニューの「ウェイポイント」の欄に、今のワールド、最後に取った現在地、記録した地点の一覧が出る。名前を押すと、その地点が目的地になる。「名前を変える」と「削除」もここで行う。
  「現在地を記録」と「現在地を更新」のボタンは、メニューを開いたままでも使える
- K を押すと現在地を取り直し、目的地への方角と距離を 8 秒出す（例：`地点 3：北東 128 m（右に 35°・12 m 上）`。左右は今向いている方向から）。
  オーバーワールドとネザーの間は、座標を換算して出す。目的地を選んでいなければ、座標は取らずにそう知らせる
- 自分で F3+C を押したときも、現在地を更新する（クリップボードにはいつもどおり座標が入る）

座標は Minecraft の F3+C で取る。reminedog がゲームに F3+C のキーを送り、Minecraft がクリップボードに書く座標を横取りする。

- 記録や更新のたびに、チャットに「［デバッグ］：クリップボードに座標をコピーしました」と出る（Minecraft が出すので消せない）。実際にはクリップボードは変わらない
- デバッグ情報が制限されたワールドやサーバー（ゲームルール `reduced_debug_info` など）では、座標を取れない。そのとき、C に割り当てたほかの操作（「アイテムを捨てる」など）が 1 回動くことがある。
  一度断られたワールドでは、メニューの「もう一度試す」を押すまで F3+C を送らない
- F3・C（Minecraft のキー設定の、デバッグの修飾キー・座標のコピー・クラッシュのキー）を押している間は、座標を取れない。C に「アイテムを捨てる」も割り当てているときは、Ctrl を押している間も取れない（断られたときにスタックごと捨てないため）
- 記録キーと更新キーには、Ctrl との組み合わせを割り当てられない
- F3 と C の割り当ては、ゲームフォルダの `options.txt` から読む（キー設定で変えていても使える。ただしマウスのボタンなど、対応していないキーがある）
- ワールドは `logs/latest.log` と `saves` のフォルダから判定する。SeedQueue のように裏でワールドを作る Mod を入れていると、判定できない
- シングルプレイはワールドのフォルダ名で区別する。ワールドを消して同じ名前で作り直すと、前のワールドの地点がそのまま出る（`reminedog/waypoints/sp-<名前>.json` を消すと空になる）

#### ズーム

既定では「高精細」で拡大する。ズームキーを押している間だけ、ゲームに縦に倍率ぶん長い解像度（例：2560×1440 で 4 倍なら 2560×5760）で描かせ、その中央を画面に出す。
Minecraft の視野角は縦方向で決まるので、縦に長く描かせると同じ視野に多くの画素が入り、拡大しても細かいところまで見える（スピードランの「縦長の解像度」と同じ考え方）。

- 描く画素が倍率ぶん増えるので、そのぶん重くなる。倍率の上限は GPU が扱える大きさ（多くは 16384 px）で決まる
- 押した瞬間と離した瞬間に、ゲームが画面の大きさの変更を処理する（一瞬引っかかることがある）
- 拡大している間は、画面の下の HUD（ホットバーなど）は見えない。クロスヘアは見える
- 高精細が使えないとき（OpenGL 3.0 未満など）や、メニューで「高精細にする」を外したときは、描かれた画面の中央を引き伸ばす

### オプション

`-agentpath:C:\reminedog\reminedog.dll=log=debug,scale=1.5` のように `=` の後ろにカンマ区切りで書く。

| キー | 値 | 既定 |
|---|---|---|
| `gamedir` | ゲームフォルダのパス（自動判定が外れるとき） | `--gameDir` の引数、なければ作業フォルダ |
| `log` | `off` / `error` / `warn` / `info` / `debug` / `trace` | `info` |
| `overlay` | `on` / `off`（`off` ならフックだけ入れて何も描かない） | `on` |
| `scale` | UI の拡大率（0.5〜4） | `1` |

## 開発

```sh
cargo test --workspace                 # core と render のテスト（Linux でも動く）
cargo clippy --workspace --all-targets
rustup target add x86_64-pc-windows-gnu                           # 初回のみ。mingw-w64 も必要
cargo build -p reminedog-hook-win --target x86_64-pc-windows-gnu   # Linux から Windows 向けにビルド
scripts/wine-smoke.sh --agent target/x86_64-pc-windows-gnu/debug/reminedog.dll --screenshot shot.png
```

`scripts/wine-smoke.sh` は Wine 上の Windows 版 Java で LWJGL のウィンドウを開き、エージェントを読み込ませて動作を確かめる。
必要なもの：64 ビットの Wine、Xvfb（`xvfb-run`、Mesa の GLX）、JDK 9 以降の `javac`、pip の入った `python3`、`curl`、`flock` と `timeout`。
初回は PyPI（Windows 版 Java）と Maven Central（LWJGL）からダウンロードし、`target/wine-cache` に Wine の環境を作る（`cargo clean` で消える）。
`--screenshot` を付けると最後のフレームを PNG で保存する。
`--seconds 8 --capture --xdotool "key ctrl+i"` のように、秒数を決めて動かし、`xdotool` でキーやマウスの操作を送れる（`--sdl` で SDL3 版）。
`--mc` を付けると Minecraft と同じように自前のフレームバッファに描いてからウィンドウに転送するので、高精細のズームを試せる（例：`--capture --mc --xdotool "keydown z sleep 2" --grab zoom.png` で Z を押したままの画面を保存する）。
日本語を表示するには、日本語フォントを `target/wine-cache/prefix/drive_c/windows/Fonts/msgothic.ttc` などの名前で置く（`~/.wine` ではない。初回の実行でフォルダができる）。
