package reminedog.installer

import java.nio.file.Files
import java.nio.file.InvalidPathException
import java.nio.file.Path
import java.nio.file.Paths

/** Finds the launchers' data folders. */
object Launchers {
    /** The launchers in their usual places. */
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
                // MultiMC runs from wherever it was unzipped; these are the common places.
                for (folder in listOf("Desktop", "Downloads", "Documents")) add(path(home, folder, "MultiMC"))
                add(path(home, "MultiMC"))
                add(path(home, "scoop", "persist", "prismlauncher"))
            }
            add(path("C:\\", "MultiMC"))
        }
        return places.filterNotNull().filter { Files.isDirectory(it) }.flatMap { at(it) }
    }

    /**
     * The launchers whose data folder is [folder] or holds it (up to three levels up, so that
     * picking the instances folder or an instance works too).
     */
    fun find(folder: Path): List<Launcher> =
        generateSequence(folder.toAbsolutePath().normalize()) { it.parent }
            .take(4)
            .map { at(it) }
            .firstOrNull { it.isNotEmpty() }
            .orEmpty()

    /** The launchers whose data folder is exactly [dir]. */
    fun at(dir: Path): List<Launcher> = buildList {
        for (file in listOf(MojangLauncher.FILE, MojangLauncher.STORE_FILE)) {
            if (Files.isRegularFile(dir.resolve(file))) add(MojangLauncher(dir, file))
        }
        if (Files.isRegularFile(dir.resolve(MultiMcLauncher.PRISM_CONFIG))) {
            add(MultiMcLauncher.prism(dir))
        } else if (Files.isRegularFile(dir.resolve(MultiMcLauncher.MULTIMC_CONFIG))) {
            add(MultiMcLauncher.multimc(dir))
        }
        // MCSR Launcher: the install folder (MCSRLauncher.jar and launcher/) or its launcher/ folder.
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
