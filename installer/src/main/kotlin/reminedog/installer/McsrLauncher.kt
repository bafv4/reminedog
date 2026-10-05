package reminedog.installer

import java.io.IOException
import java.nio.channels.FileChannel
import java.nio.channels.OverlappingFileLockException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption

/**
 * MCSR Launcher. Its data folder is the "launcher" folder next to MCSRLauncher.jar, with the
 * launcher's settings (options.json) and instances/<id>/instance.json.
 *
 * An instance takes the launcher's Java settings (the Java, the memory and the JVM arguments
 * together) unless its "options.useLauncherJavaOption" is false. Adding reminedog to an instance
 * that takes the launcher's settings copies them into the instance first, so that only the
 * arguments change.
 */
class McsrLauncher(root: Path) : Launcher("MCSR Launcher", root) {
    private fun options(): Map<String, Any?> {
        val file = root.resolve("options.json")
        return if (Files.isRegularFile(file)) JsonDoc.load(file).root else emptyMap()
    }

    private fun globalArgs(): String = options()["jvmArguments"] as? String ?: ""

    override fun instances(): List<Instance> {
        val instances = root.resolve("instances")
        if (!Files.isDirectory(instances)) return emptyList()
        val files = Files.newDirectoryStream(instances).use { stream ->
            stream.map { it.resolve("instance.json") }.filter { Files.isRegularFile(it) }
        }
        return files.sorted().map { file ->
            val json = try {
                JsonDoc.load(file).root
            } catch (e: IOException) {
                // Listed by its folder; reading it again shows the error.
                emptyMap<String, Any?>()
            }
            val name = (json["displayName"] as? String).orEmpty().ifEmpty { file.parent.fileName.toString() }
            JsonInstance(file, name, json["minecraftVersion"] as? String ?: "")
        }
    }

    /** The launcher holds a lock on launcher/_app.lock while it runs. */
    override fun isRunning(processes: Processes): Boolean {
        val lockFile = root.resolve("_app.lock")
        if (!Files.isRegularFile(lockFile)) return false
        return try {
            FileChannel.open(lockFile, StandardOpenOption.READ, StandardOpenOption.WRITE).use { channel ->
                val lock = channel.tryLock() ?: return true
                lock.release()
                false
            }
        } catch (e: OverlappingFileLockException) {
            true
        } catch (e: IOException) {
            false
        }
    }

    private inner class JsonInstance(private val file: Path, name: String, detail: String) :
        Instance(this@McsrLauncher, name, detail) {

        private fun usesLauncher(options: Map<String, Any?>) = options["useLauncherJavaOption"] != false

        /** The instance's own arguments; without "jvmArguments" it gets the launcher's. */
        private fun own(options: Map<String, Any?>) = options["jvmArguments"] as? String ?: globalArgs()

        override fun read(): Args {
            val options = JsonDoc.load(file).root["options"].asJsonObject()
            return if (options == null || usesLauncher(options)) {
                Args(globalArgs(), shared = true)
            } else {
                Args(own(options), shared = false)
            }
        }

        override fun install(dllPath: String): Change {
            val doc = JsonDoc.load(file)
            val options = doc.root["options"].asJsonObject()
            if (options != null && !usesLauncher(options)) {
                val args = AgentArg.install(own(options), dllPath)
                if (args == options["jvmArguments"]) return Change.AlreadyInstalled
                options["jvmArguments"] = args
                doc.save()
                return Change.Installed(args)
            }
            // The instance follows the launcher's Java settings: copy them into the instance.
            val launcher = options()
            val javaPath = launcher["javaPath"] as? String
            val minMemory = launcher["minMemory"] as? JsonNumber
            val maxMemory = launcher["maxMemory"] as? JsonNumber
            if (javaPath == null || minMemory == null || maxMemory == null) {
                throw IOException("MCSR Launcher の設定（options.json）に Java の設定がありません。ランチャーの設定を一度保存してから試してください")
            }
            val args = AgentArg.install(globalArgs(), dllPath)
            val target = options ?: LinkedHashMap<String, Any?>().also { doc.root["options"] = it }
            target["useLauncherJavaOption"] = false
            target["javaPath"] = javaPath
            target["minMemory"] = minMemory
            target["maxMemory"] = maxMemory
            target["jvmArguments"] = args
            doc.save()
            return Change.Installed(args)
        }

        override fun remove(): Change {
            val doc = JsonDoc.load(file)
            val options = doc.root["options"].asJsonObject()
            if (options == null || usesLauncher(options)) {
                return if (AgentArg.find(globalArgs()).isEmpty()) Change.NotInstalled else Change.SharedOnly
            }
            val old = own(options)
            val args = AgentArg.remove(old)
            if (args == old) return Change.NotInstalled
            options["jvmArguments"] = args
            doc.save()
            return Change.Removed(args)
        }
    }
}
