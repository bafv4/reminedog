package reminedog.installer

import java.io.File
import java.io.IOException
import java.util.concurrent.TimeUnit

/** 動いているプログラムの名前（tasklist から）。ランチャーが開いているかを知るため。 */
class Processes private constructor(private val names: Set<String>) {
    fun has(name: String) = name.lowercase() in names

    companion object {
        fun of(vararg names: String) = Processes(names.map { it.lowercase() }.toSet())

        /**
         * 動いているプログラム。一覧にできなければ（Windows でない、tasklist に 10 秒より長く
         * かかった）なし。少しかかる：ウィンドウのスレッドでは呼ばない。
         */
        fun running(): Processes {
            val names = HashSet<String>()
            var output: File? = null
            try {
                output = File.createTempFile("reminedog-tasklist", ".csv")
                // ファイルに書くので、下の待ちがいっぱいのパイプで止まることはない。
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
                // 分からない：どのランチャーも閉じているとみなす。
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
 * フルパス（`%SystemRoot%\System32\<name>`）での Windows のプログラム：名前だけだと、Windows は
 * まず作業フォルダを探し、そこに同じ名前のファイルを仕込まれうる。
 */
fun system32(name: String): String {
    val root = System.getenv("SystemRoot")?.takeIf { it.isNotEmpty() } ?: "C:\\Windows"
    return "$root\\System32\\$name"
}
