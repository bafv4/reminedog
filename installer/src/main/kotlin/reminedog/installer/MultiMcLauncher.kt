package reminedog.installer

import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path

/**
 * MultiMC and Prism Launcher (a fork of MultiMC with the same files).
 *
 * An instance uses its own JVM arguments ("JvmArgs" in instance.cfg) when it overrides them
 * ("OverrideJavaArgs"; MultiMC also takes "OverrideJava"), and the launcher's ("JvmArgs" in
 * multimc.cfg / prismlauncher.cfg) otherwise. Adding reminedog turns the override on, starting
 * from the arguments the instance used before.
 */
class MultiMcLauncher private constructor(label: String, root: Path, private val prism: Boolean) :
    Launcher(label, root) {

    private val config: Path = root.resolve(if (prism) PRISM_CONFIG else MULTIMC_CONFIG)

    private fun config(): Ini = Ini.parse(readUtf8(config))

    private fun globalArgs(): String = config()["JvmArgs"] ?: ""

    override fun instances(): List<Instance> {
        val dir = config()["InstanceDir"]?.trim().orEmpty().ifEmpty { "instances" }
        val instances = root.resolve(dir)
        if (!Files.isDirectory(instances)) return emptyList()
        val folders = Files.newDirectoryStream(instances).use { stream ->
            stream.filter { folder ->
                val name = folder.fileName.toString()
                // The launchers keep temporary folders here (".LAUNCHER_TEMP", "_MMC_TEMP").
                !name.startsWith(".") && !name.startsWith("_") && Files.isRegularFile(folder.resolve("instance.cfg"))
            }
        }
        return folders.sorted().map { folder ->
            val name = try {
                Ini.parse(readUtf8(folder.resolve("instance.cfg")))["name"]
            } catch (e: IOException) {
                null
            }
            CfgInstance(folder, if (name.isNullOrEmpty()) folder.fileName.toString() else name)
        }
    }

    override fun isRunning(processes: Processes) = processes.has(if (prism) "prismlauncher.exe" else "MultiMC.exe")

    private inner class CfgInstance(folder: Path, name: String) :
        Instance(this@MultiMcLauncher, name, version(folder), folder.fileName.toString()) {

        private val file = folder.resolve("instance.cfg")

        private fun overrides(ini: Ini) = ini.getBool("OverrideJavaArgs") || (!prism && ini.getBool("OverrideJava"))

        private fun own(ini: Ini) = ini["JvmArgs"] ?: ""

        override fun read(): Args {
            val ini = Ini.parse(readUtf8(file))
            return if (overrides(ini)) Args(own(ini), shared = false) else Args(globalArgs(), shared = true)
        }

        override fun install(dllPath: String): Change {
            val ini = Ini.parse(readUtf8(file))
            val overrides = overrides(ini)
            val old = if (overrides) own(ini) else globalArgs()
            val args = AgentArg.install(old, dllPath)
            if (overrides && args == old) return Change.AlreadyInstalled
            ini["JvmArgs"] = args
            ini["OverrideJavaArgs"] = "true"
            writeAtomically(file, ini.text())
            return Change.Installed(args)
        }

        override fun remove(): Change {
            val ini = Ini.parse(readUtf8(file))
            if (!overrides(ini)) {
                return if (AgentArg.find(globalArgs()).isEmpty()) Change.NotInstalled else Change.SharedOnly
            }
            val old = own(ini)
            val args = AgentArg.remove(old)
            if (args == old) return Change.NotInstalled
            ini["JvmArgs"] = args
            writeAtomically(file, ini.text())
            return Change.Removed(args)
        }
    }

    companion object {
        const val PRISM_CONFIG = "prismlauncher.cfg"
        const val MULTIMC_CONFIG = "multimc.cfg"

        fun prism(root: Path) = MultiMcLauncher("Prism Launcher", root, prism = true)

        fun multimc(root: Path) = MultiMcLauncher("MultiMC", root, prism = false)

        /** The Minecraft version from the instance's mmc-pack.json, or "". */
        private fun version(folder: Path): String {
            val pack = try {
                JsonDoc.load(folder.resolve("mmc-pack.json")).root
            } catch (e: IOException) {
                return ""
            }
            val minecraft = pack["components"].asJsonArray()
                ?.mapNotNull { it.asJsonObject() }
                ?.firstOrNull { it["uid"] == "net.minecraft" }
                ?: return ""
            return minecraft["version"] as? String ?: minecraft["cachedVersion"] as? String ?: ""
        }
    }
}
