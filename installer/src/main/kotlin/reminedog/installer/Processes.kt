package reminedog.installer

import java.io.IOException
import java.util.concurrent.TimeUnit

/** The names of the running programs (from tasklist), to tell whether a launcher is open. */
class Processes private constructor(private val names: Set<String>) {
    fun has(name: String) = name.lowercase() in names

    companion object {
        fun of(vararg names: String) = Processes(names.map { it.lowercase() }.toSet())

        /** The running programs, or none when they cannot be listed (not on Windows). */
        fun running(): Processes {
            val names = HashSet<String>()
            try {
                val process = ProcessBuilder("tasklist", "/FO", "CSV", "/NH").redirectErrorStream(true).start()
                process.inputStream.bufferedReader().useLines { lines ->
                    // "image.exe","1234","Console","1","12,345 K"
                    for (line in lines) {
                        if (line.startsWith("\"")) names += line.substring(1).substringBefore('"').lowercase()
                    }
                }
                process.waitFor(10, TimeUnit.SECONDS)
            } catch (e: IOException) {
                // Unknown: every launcher counts as closed.
            } catch (e: InterruptedException) {
                Thread.currentThread().interrupt()
            }
            return Processes(names)
        }
    }
}
