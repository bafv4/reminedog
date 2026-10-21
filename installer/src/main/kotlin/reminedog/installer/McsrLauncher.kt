package reminedog.installer

import java.io.IOException
import java.nio.channels.FileChannel
import java.nio.channels.OverlappingFileLockException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardOpenOption

/**
 * MCSR Launcher。データのフォルダは MCSRLauncher.jar の隣の "launcher" のフォルダで、
 * ランチャーの設定（options.json）と instances/<id>/instance.json がある。
 *
 * インスタンスは、"options.useLauncherJavaOption" が false でない限り、ランチャーの Java の設定
 * （Java、メモリ、JVM 引数をまとめて）を使う。ランチャーの設定を使うインスタンスに reminedog を
 * 足すときは、引数だけが変わるように、先にそれらをインスタンスに写す。
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
                // フォルダで並べる。読み直すとエラーが出る。
                emptyMap<String, Any?>()
            }
            val name = (json["displayName"] as? String).orEmpty().ifEmpty { file.parent.fileName.toString() }
            JsonInstance(file, name, json["minecraftVersion"] as? String ?: "")
        }
    }

    /** ランチャーは動いている間 launcher/_app.lock をロックしている。 */
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
        Instance(this@McsrLauncher, name, detail, file.parent.fileName.toString()) {

        private fun usesLauncher(options: Map<String, Any?>) = options["useLauncherJavaOption"] != false

        /** インスタンス自身の引数。"jvmArguments" がなければランチャーのものを使う。 */
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
            // インスタンスはランチャーの Java の設定に従う：それをインスタンスに写す。
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
