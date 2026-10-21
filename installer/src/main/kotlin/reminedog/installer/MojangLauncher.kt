package reminedog.installer

import java.io.IOException
import java.nio.file.Path

/**
 * 公式の Minecraft Launcher：.minecraft のフォルダの launcher_profiles.json（Microsoft Store・
 * Xbox アプリのランチャーは launcher_profiles_microsoft_store.json）のプロファイル（「起動構成」）。
 * プロファイルの JVM 引数は "javaArgs"。
 */
class MojangLauncher(minecraftDir: Path, fileName: String) : Launcher(
    if (fileName == STORE_FILE) "Minecraft Launcher（Microsoft Store）" else "Minecraft Launcher",
    minecraftDir,
) {
    private val file = minecraftDir.resolve(fileName)

    override fun instances(): List<Instance> {
        val profiles = JsonDoc.load(file).root["profiles"].asJsonObject() ?: return emptyList()
        return profiles.mapNotNull { (id, value) -> value.asJsonObject()?.let { Profile(id, it) } }
    }

    override fun isRunning(processes: Processes) =
        processes.has("MinecraftLauncher.exe") || processes.has("Minecraft.exe")

    override val key: String
        get() = super.key + "|" + file.fileName

    private inner class Profile(private val id: String, profile: Map<String, Any?>) :
        Instance(this@MojangLauncher, displayName(profile), detail(profile), id) {

        private fun profileIn(doc: JsonDoc): MutableMap<String, Any?> =
            doc.root["profiles"].asJsonObject()?.get(id).asJsonObject()
                ?: throw IOException("起動構成が見つかりません（ランチャーで消されたかもしれません）")

        private fun argsOf(profile: Map<String, Any?>) = profile["javaArgs"] as? String ?: DEFAULT_ARGS

        override fun read() = Args(argsOf(profileIn(JsonDoc.load(file))), shared = false)

        override fun install(dllPath: String): Change {
            val doc = JsonDoc.load(file)
            val profile = profileIn(doc)
            val args = AgentArg.install(argsOf(profile), dllPath)
            if (args == profile["javaArgs"]) return Change.AlreadyInstalled
            profile["javaArgs"] = args
            doc.save()
            return Change.Installed(args)
        }

        override fun remove(): Change {
            val doc = JsonDoc.load(file)
            val profile = profileIn(doc)
            val old = argsOf(profile)
            val args = AgentArg.remove(old)
            if (args == old) return Change.NotInstalled
            profile["javaArgs"] = args
            doc.save()
            return Change.Removed(args)
        }
    }

    companion object {
        const val FILE = "launcher_profiles.json"
        const val STORE_FILE = "launcher_profiles_microsoft_store.json"

        /**
         * "javaArgs" のないプロファイルをランチャーが起動するときのもの（「JVM 引数」のスイッチを
         * オンにしたときにランチャーが出す文字）。プロファイルが 2 GB のメモリを使い続けるように、
         * reminedog を足すときに残す。
         */
        const val DEFAULT_ARGS = "-Xmx2G -XX:+UnlockExperimentalVMOptions -XX:+UseG1GC" +
            " -XX:G1NewSizePercent=20 -XX:G1ReservePercent=20 -XX:MaxGCPauseMillis=50 -XX:G1HeapRegionSize=32M"

        private fun displayName(profile: Map<String, Any?>): String {
            val name = profile["name"] as? String
            return when {
                !name.isNullOrEmpty() -> name
                profile["type"] == "latest-release" -> "最新のリリース"
                profile["type"] == "latest-snapshot" -> "最新のスナップショット"
                else -> "（名前なし）"
            }
        }

        private fun detail(profile: Map<String, Any?>): String {
            val type = profile["type"]
            if (type == "latest-release" || type == "latest-snapshot") return ""
            return profile["lastVersionId"] as? String ?: ""
        }
    }
}
