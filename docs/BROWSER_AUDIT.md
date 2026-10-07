# ゲーム内ブラウザの調査結果

## 調査概要

- 調査日：2026-10-07
- 対象：reminedogのゲーム内ブラウザ機能
- 方法：コードレビュー、関連APIの公式仕様確認、既存テスト、追加の再現テスト
- 結果：再現済みの不具合2件、コード上の問題・対策漏れ3件
- 任意コード実行や認証情報の窃取につながる脆弱性は今回の調査では未確認。安全性全般を保証する調査ではない。
- WebView2の実動作テストとMinecraft実機検証は未実施。
- 調査では製品コードを変更していない。以下の修正方針は未実装。

この文書は調査時点の結果を引き継ぐためのもの。後続の変更により行番号や状況が変わるため、修正前に現行コードと照合すること。

## 機能の構成

- Windows側が専用スレッドでWebView2を動かす。
- ページの描画結果をDirect3D経由で取得し、OpenGLテクスチャとしてゲーム画面に表示する。
- ページへの入力はChrome DevTools Protocol（CDP）で送る。
- ゲーム側とブラウザ側はコマンドキュー・共有状態・最新画像を介して連携する。
- ホストオブジェクトとWebメッセージは無効化されている。

| ファイル | 役割 |
|---|---|
| [hook-win/src/browser.rs](../hook-win/src/browser.rs) | 起動・表示・終了、コマンドキュー、共有状態 |
| [thread.rs](../hook-win/src/browser/thread.rs) | COM初期化、DispatcherQueue、メッセージループ |
| [webview.rs](../hook-win/src/browser/webview.rs) | WebView2の設定、イベント、入力・動画操作 |
| [capture.rs](../hook-win/src/browser/capture.rs) | 描画結果の取り込み |
| [render/src/browser.rs](../render/src/browser.rs) | ブラウザUI、サイズ変更、入力変換 |
| [core/src/browser.rs](../core/src/browser.rs) | CDPパラメーター、URL整形、動画操作スクリプト |

## 発見事項

### 1. メニューを閉じるとページに入力の解放が届かない

- 状態：再現済み
- 優先度：中
- 対象：[render/src/browser.rs](../render/src/browser.rs) のメニュー終了処理（調査時296行付近）とキー転送条件（651行付近）

#### 発生条件

1. ページ上でマウスボタン、またはキーを押す。
2. 押したままメニューを閉じる。
3. メニューを閉じた状態で離す。

#### 原因と影響

メニューを閉じると、ページへの解放イベントを送らずにマウスの押下状態を消す。キーもフォーカスがある間だけ転送するため、`MouseUp`／`KeyUp`が欠落する。

ページ側のドラッグや長押し処理が継続する原因になる。再現テストで確認したのは送信コマンドの欠落であり、実際のWebView2上のページ挙動は未検証。

#### 修正方針

ページに送信した押下中のキー・ボタンを追跡し、メニュー終了、非表示、フォーカス喪失時に解放する。解放は現在のフォーカスだけで判定しない。

### 2. ドラッグによるサイズ変更が上書きされる

- 状態：再現済み
- 優先度：中
- 対象：[render/src/browser.rs](../render/src/browser.rs) のサイズ変更処理（480行付近）と後続の保存処理（372行付近）

#### 発生条件

ブラウザ右下のハンドルをドラッグしてサイズを変更する。

#### 原因と再現結果

ドラッグ処理が新しいサイズを設定した後、同じフレームのウィンドウ位置保存処理が、変更前の描画領域のサイズで上書きする。

400×225ポイントの領域を80×50ポイント拡大しても、設定値は400×225のままだった。

#### 修正方針

ウィンドウ位置とサイズの更新を分離し、ドラッグで算出した新しいサイズを後続処理で保持する。

### 3. ファイル選択ダイアログの遮断がない

- 状態：コード上の対策漏れ。実動作未検証
- 対象：[webview.rs](../hook-win/src/browser/webview.rs) の `configure` とイベント登録処理（378行以降）

#### 発生条件

ページ内の `<input type="file">` などを操作する。

#### 原因と想定される影響

JavaScriptダイアログとダウンロードは制限しているが、ファイル選択ダイアログを遮断する処理がない。`AreDefaultScriptDialogsEnabled` の対象は `alert`、`confirm`、`prompt`、`beforeunload` である。

OSダイアログが開き、ゲームのフォーカスを奪う可能性がある。利用者の選択なしにファイルを窃取できると確認したものではない。

#### 修正方針

ファイル選択の遮断を別途実装する。CDPのファイル選択インターセプトなどについて、対象WebView2ランタイムでの対応を検証する。

参考：[Microsoftのダイアログ設定仕様](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2settings?view=webview2-1.0.3537.50#get_aredefaultscriptdialogsenabled)

### 4. DispatcherQueueの終了手順が不足している

- 状態：公式APIの終了要件との不一致を確認。実害未検証
- 優先度：中
- 対象：[thread.rs](../hook-win/src/browser/thread.rs) の `run_in_apartment`（59行付近）

#### 原因と想定される影響

`DQTYPE_THREAD_CURRENT` で作成したDispatcherQueueに対して、`ShutdownQueueAsync` を呼ばずにスレッドを終了している。

終了時の後処理が完了せず、リソース解放に問題が生じる可能性がある。リーク量やクラッシュは確認していない。

#### 修正方針

正常終了と初期化途中の失敗の両方で、キューの終了を要求し、完了までメッセージを処理してからCOMを終了する。

参考：[Microsoftの終了要件](https://learn.microsoft.com/en-us/windows/win32/api/dispatcherqueue/nf-dispatcherqueue-createdispatcherqueuecontroller#remarks)

### 5. 非表示時の動画停止がiframe内に届かない

- 状態：コード上で対象範囲の不足を確認。実動作未検証
- 対象：[core/src/browser.rs](../core/src/browser.rs) の `MediaCommand::PauseAll`（343行付近）

#### 原因と想定される影響

停止処理は最上位ページの `document.querySelectorAll('video, audio')` だけを対象としており、iframe内のプレイヤーを操作しない。

埋め込み動画が再生中の場合、ブラウザを非表示にしても再生・音声が継続する可能性がある。

#### 修正方針

非表示時のブラウザ全体のミュートと、必要に応じてフレームごとの停止処理を検討する。ミュートだけでは動画の再生自体は停止しない点に注意する。

## 検証結果

| 検証 | 結果 |
|---|---|
| 既存の関連テスト | 27件成功 |
| サイズ変更の再現テスト | 失敗：変更後のサイズが残らない |
| マウス解放の再現テスト | 失敗：ページへの解放通知なし |
| キー解放の再現テスト | 失敗：ページへの解放通知なし |
| WebView2実動作テスト | 未実施 |
| Minecraft実機検証 | 未実施 |

### 既存テスト

リポジトリのルートで実行：

```powershell
cargo test -p reminedog-core -p reminedog-render browser --offline
```

### 再現テスト

追加コードは [target/browser-audit/browser_audit.rs](../target/browser-audit/browser_audit.rs) に退避済み。これはローカルのビルド領域にある調査用ファイルで、Gitでは共有されず、`cargo clean` で消える可能性がある。

コード中の相対パスは元の配置を前提としている。再実行する場合は `render/tests/browser_audit.rs` に配置し、次を実行する。同名ファイルが既にある場合は上書きしないこと。

```powershell
cargo test -p reminedog-render --test browser_audit audit_ --offline -- --nocapture
```

調査時の失敗内容：

```text
audit_resizing_keeps_the_new_dimensions
  resize lost: [300.0, 200.0, 400.0, 225.0]

audit_closing_menu_releases_page_mouse
  page never released: []

audit_losing_page_focus_releases_page_key
  page never released: []
```

これらは期待する動作を検証するテストであり、未修正のコードでは失敗する。調査終了時には通常のテスト対象から取り除き、上記の退避先だけに残した。

## 次の作業

1. 入力解放とサイズ変更を修正し、再現テストを回帰テストとして組み込む。
2. DispatcherQueueの終了手順を修正する。
3. ファイル選択ダイアログとiframe内の動画について、隔離したテストページで実動作を確認する。
4. GLFW・SDL3両方のスモークテストとMinecraft実機で確認する。
5. 実機確認が必要な項目を [TESTING.md](./TESTING.md) に追記する。

## 対応（2026-10-08）

5 件とも直した。詳しくは [HANDOFF.md](./HANDOFF.md) の「追記：ゲーム内ブラウザ」の「確かめたこと（2026-10-08）」。

| 発見事項 | 対応 | 確認 |
|---|---|---|
| 1. 入力の解放が届かない | ページで押したボタンとキーを `BrowserMenu` に覚え、メニューを閉じたとき・非表示にしたときに `MouseUp`／`KeyUp` を送る。キーはページのフォーカスが外れたときにも送る | render の単体テスト 2 件（直す前のコードでは落ちる） |
| 2. サイズ変更が上書きされる | 窓からの書き戻しはページの位置だけにし、大きさは settings（つまみが書いたもの）のまま | render の単体テスト（直す前のコードでは落ちる） |
| 3. ファイル選択ダイアログ | `Page.enable` と `Page.setInterceptFileChooserDialog`（`cancel: true`）。直す前は「開く」のダイアログが reminedog と同じプロセスのブラウザのスレッドで開き、スレッドが止まることを確かめた | WebView2 のテスト（ページに `cancel` が届き、スクリプトが答える） |
| 4. DispatcherQueue の終了 | 終わるときに `ShutdownQueueAsync` を呼び、終わるまでメッセージを回してから COM を閉じる（起動の途中で失敗したときも） | WebView2 のテストで終了が最後まで進むこと |
| 5. iframe 内の動画 | 非表示の間は WebView2 ごとミュートする。iframe の中の再生は続く（HANDOFF の「確かめていないこと」に記載） | WebView2 のテスト（非表示でミュート、表示で解除） |

実機（Minecraft）での確認は [TESTING.md](./TESTING.md) の「ブラウザで確認すること」に足した。
