package reminedog.installer

import java.nio.file.Files
import java.nio.file.InvalidPathException
import java.nio.file.Path
import java.nio.file.Paths

/** ランチャーのデータのフォルダを探す。 */
object Launchers {
    /** いつもの場所にあるランチャー。 */
    fun detect(): List<Launcher> {
        val appData = System.getenv("APPDATA")
        val localAppData = System.getenv("LOCALAPPDATA")
        val home = System.getProperty("user.home")
        val places = buildList {
            if (appData != null) {
                add(path(appData, ".minecraft"))
                add(path(appData, "PrismLauncher"))
                add(path(appData, "MultiMC"))
            }
            if (localAppData != null) {
                add(path(localAppData, "MCSRLauncher"))
                add(path(localAppData, "MultiMC"))
            }
            if (home != null) {
                // MultiMC は展開した場所ならどこからでも動く。これはよくある場所。
                for (folder in listOf("Desktop", "Downloads", "Documents")) add(path(home, folder, "MultiMC"))
                add(path(home, "MultiMC"))
                add(path(home, "scoop", "persist", "prismlauncher"))
            }
            add(path("C:\\", "MultiMC"))
        }
        return places.filterNotNull().filter { Files.isDirectory(it) }.flatMap { at(it) }
    }

    /**
     * データのフォルダが [folder] であるか [folder] を含むランチャー（インスタンスのフォルダや
     * インスタンスを選んでも見つかるように、3 階層上まで）。
     */
    fun find(folder: Path): List<Launcher> =
        generateSequence(folder.toAbsolutePath().normalize()) { it.parent }
            .take(4)
            .map { at(it) }
            .firstOrNull { it.isNotEmpty() }
            .orEmpty()

    /** データのフォルダがちょうど [dir] のランチャー。 */
    fun at(dir: Path): List<Launcher> = buildList {
        for (file in listOf(MojangLauncher.FILE, MojangLauncher.STORE_FILE)) {
            if (Files.isRegularFile(dir.resolve(file))) add(MojangLauncher(dir, file))
        }
        if (Files.isRegularFile(dir.resolve(MultiMcLauncher.PRISM_CONFIG))) {
            add(MultiMcLauncher.prism(dir))
        } else if (Files.isRegularFile(dir.resolve(MultiMcLauncher.MULTIMC_CONFIG))) {
            add(MultiMcLauncher.multimc(dir))
        }
        // MCSR Launcher：インストールのフォルダ（MCSRLauncher.jar と launcher/）か、その launcher/ のフォルダ。
        val parent = dir.parent
        if (Files.isRegularFile(dir.resolve("MCSRLauncher.jar")) && Files.isDirectory(dir.resolve("launcher"))) {
            add(McsrLauncher(dir.resolve("launcher")))
        } else if (dir.fileName?.toString().equals("launcher", ignoreCase = true) &&
            Files.isDirectory(dir.resolve("instances")) &&
            (Files.isRegularFile(dir.resolve("options.json")) ||
                (parent != null && Files.isRegularFile(parent.resolve("MCSRLauncher.jar"))))
        ) {
            add(McsrLauncher(dir))
        }
    }

    private fun path(first: String, vararg more: String): Path? =
        try {
            Paths.get(first, *more)
        } catch (e: InvalidPathException) {
            null
        }
}
