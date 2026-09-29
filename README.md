# reminedog

Minecraft（Java Edition 1.13 以降）のサバイバル向けゲーム内ツール。
Mod ではなく、JVM に `-agentpath:` で読み込ませるネイティブのライブラリとして動くので、Minecraft のバージョンに依存しない。

設計は [docs/HANDOFF.md](docs/HANDOFF.md) を参照。

## 現在の状態

**プロトタイプ 1**：GLFW の `glfwSwapBuffers` をフックし、自前の OpenGL コンテキストで egui のウィンドウを 1 つ描く。
キーやマウスの入力はまだ受け付けない。確認の手順は [docs/TESTING.md](docs/TESTING.md) にある。

| 機能 | 状態 |
|---|---|
| オーバーレイの描画（egui） | プロトタイプ 1 |
| 入力の横取り・ホットキー・ズーム | 未実装（プロトタイプ 2） |
| F3+C による座標の記録 | 未実装（プロトタイプ 3）。パースと保存は `core` に実装済み |

## 構成

```
core/      OS に依存しない処理：F3+C のパース、ウェイポイントの保存、ワールドの判定、ログ
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

1. `reminedog.dll` を日本語を含まない場所に置く（例：`C:\reminedog\reminedog.dll`）
2. JVM 引数に次を追加する
   - Prism Launcher：インスタンスの「編集」→「設定」→「Java」→「JVM 引数」
   - 公式ランチャー：「起動構成」→ 構成の「編集」→「その他のオプション」→「JVM 引数」

   ```
   -agentpath:C:\reminedog\reminedog.dll
   ```
3. ゲームを起動する。ログはゲームフォルダの `reminedog/reminedog.log` に出る
   （Prism Launcher ならインスタンスの `minecraft` フォルダ、公式ランチャーなら `.minecraft`）

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
cargo build -p reminedog-hook-win --target x86_64-pc-windows-gnu   # Linux から Windows 向けにビルド（mingw-w64 が必要）
scripts/wine-smoke.sh --agent target/x86_64-pc-windows-gnu/debug/reminedog.dll --screenshot shot.png
```

`scripts/wine-smoke.sh` は Wine 上の Windows 版 Java で LWJGL のウィンドウを開き、エージェントを読み込ませて動作を確かめる（Wine、Xvfb、mingw-w64 が必要）。
`--screenshot` を付けると最後のフレームを PNG で保存する。日本語を表示するには、Wine の `C:\windows\Fonts` に日本語フォントを `msgothic.ttc` などの名前で置く。
