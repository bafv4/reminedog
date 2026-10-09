package reminedog.installer

import java.nio.file.Path

/** A launcher's data folder (the one with its settings and instances). */
abstract class Launcher(val label: String, val root: Path) {
    /** The instances (profiles) of this launcher, read from its files now. */
    abstract fun instances(): List<Instance>

    /**
     * Whether the launcher seems to be running. A running launcher keeps its settings in memory
     * and writes them back later, which would undo our changes.
     */
    abstract fun isRunning(processes: Processes): Boolean

    /** Identifies the launcher's files, to avoid listing the same folder twice. */
    open val key: String
        get() = "$label|" + root.toAbsolutePath().normalize().toString().lowercase()
}

/**
 * An instance (or a profile of the official launcher) whose JVM arguments can load reminedog.
 * [place] tells it apart from the launcher's other instances (its folder, or the profile's id):
 * names can repeat.
 */
abstract class Instance(val launcher: Launcher, val name: String, val detail: String, val place: String) {
    /** The JVM arguments the launcher would start this instance with now. */
    abstract fun read(): Args

    /** Makes the instance load reminedog from [dllPath]. */
    abstract fun install(dllPath: String): Change

    /** Takes reminedog out of the instance's JVM arguments. */
    abstract fun remove(): Change

    val title: String get() = "${launcher.label} / $name"

    /** Identifies the instance among every launcher's. */
    val key: String get() = launcher.key + "|" + place
}

/**
 * The JVM arguments of an instance. [shared] means they come from the launcher-wide settings
 * (the instance does not override them), so changing them would change other instances too.
 */
class Args(val text: String, val shared: Boolean) {
    val agents: List<FoundAgent> get() = AgentArg.find(text)
}

/** What install or remove did, for the log. */
sealed class Change(val message: String) {
    class Installed(args: String) : Change("追加しました（JVM 引数：$args）")

    data object AlreadyInstalled : Change("すでに入っています")

    class Removed(args: String) : Change("外しました（JVM 引数：${args.ifBlank { "なし" }}）")

    data object NotInstalled : Change("入っていません")

    data object SharedOnly : Change(
        "ランチャー全体の JVM 引数に入っているので、ここでは外せません。ランチャーの設定から外してください",
    )
}
