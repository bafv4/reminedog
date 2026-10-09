package reminedog.installer

import java.io.File
import java.io.IOException
import java.util.concurrent.TimeUnit

/** The names of the running programs (from tasklist), to tell whether a launcher is open. */
class Processes private constructor(private val names: Set<String>) {
    fun has(name: String) = name.lowercase() in names

    companion object {
        fun of(vararg names: String) = Processes(names.map { it.lowercase() }.toSet())

        /**
         * The running programs, or none when they cannot be listed (not on Windows, or tasklist
         * took longer than 10 seconds). Takes a moment: not on the window's thread.
         */
        fun running(): Processes {
            val names = HashSet<String>()
            var output: File? = null
            try {
                output = File.createTempFile("reminedog-tasklist", ".csv")
                // Written to a file, so the wait below cannot hang on a full pipe.
                val process = ProcessBuilder(system32("tasklist.exe"), "/FO", "CSV", "/NH")
                    .redirectErrorStream(true)
                    .redirectOutput(output)
                    .start()
                if (!process.waitFor(10, TimeUnit.SECONDS)) {
                    process.destroyForcibly()
                    return Processes(names)
                }
                // "image.exe","1234","Console","1","12,345 K"
                for (line in output.readLines()) {
                    if (line.startsWith("\"")) names += line.substring(1).substringBefore('"').lowercase()
                }
            } catch (e: IOException) {
                // Unknown: every launcher counts as closed.
            } catch (e: InterruptedException) {
                Thread.currentThread().interrupt()
            } finally {
                output?.delete()
            }
            return Processes(names)
        }
    }
}

/**
 * A Windows program by its full path (`%SystemRoot%\System32\<name>`): by its name alone,
 * Windows would look in the working folder first, where a file of that name could be planted.
 */
fun system32(name: String): String {
    val root = System.getenv("SystemRoot")?.takeIf { it.isNotEmpty() } ?: "C:\\Windows"
    return "$root\\System32\\$name"
}
