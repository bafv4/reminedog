package reminedog.installer

import java.nio.file.Path

/** ランチャーのデータのフォルダ（設定とインスタンスがあるもの）。 */
abstract class Launcher(val label: String, val root: Path) {
    /** このランチャーのインスタンス（プロファイル）。今そのファイルから読む。 */
    abstract fun instances(): List<Instance>

    /**
     * ランチャーが動いていそうか。動いているランチャーは設定をメモリに持って後で書き戻すので、
     * こちらの変更を元に戻してしまう。
     */
    abstract fun isRunning(processes: Processes): Boolean

    /** ランチャーのファイルを識別する。同じフォルダを 2 回並べないため。 */
    open val key: String
        get() = "$label|" + root.toAbsolutePath().normalize().toString().lowercase()
}

/**
 * JVM 引数で reminedog を読み込めるインスタンス（か公式ランチャーのプロファイル）。
 * [place] はランチャーのほかのインスタンスと見分けるもの（フォルダかプロファイルの id）：
 * 名前は重なりうる。
 */
abstract class Instance(val launcher: Launcher, val name: String, val detail: String, val place: String) {
    /** ランチャーが今このインスタンスを起動するときの JVM 引数。 */
    abstract fun read(): Args

    /** インスタンスが [dllPath] から reminedog を読み込むようにする。 */
    abstract fun install(dllPath: String): Change

    /** インスタンスの JVM 引数から reminedog を外す。 */
    abstract fun remove(): Change

    val title: String get() = "${launcher.label} / $name"

    /** すべてのランチャーのインスタンスの中でインスタンスを識別する。 */
    val key: String get() = launcher.key + "|" + place
}

/**
 * インスタンスの JVM 引数。[shared] は、ランチャー全体の設定から来ている（インスタンスが上書き
 * していない）ことを表し、変えるとほかのインスタンスも変わる。
 */
class Args(val text: String, val shared: Boolean) {
    val agents: List<FoundAgent> get() = AgentArg.find(text)
}

/** インストールか取り外しでしたこと（ログ用）。 */
sealed class Change(val message: String) {
    class Installed(args: String) : Change("追加しました（JVM 引数：$args）")

    data object AlreadyInstalled : Change("すでに入っています")

    class Removed(args: String) : Change("外しました（JVM 引数：${args.ifBlank { "なし" }}）")

    data object NotInstalled : Change("入っていません")

    data object SharedOnly : Change(
        "ランチャー全体の JVM 引数に入っているので、ここでは外せません。ランチャーの設定から外してください",
    )
}
