package reminedog.installer

/** A reminedog argument found in an argument string: its place, the DLL and the agent's options. */
data class FoundAgent(val start: Int, val end: Int, val path: String, val options: String)

/**
 * The reminedog argument (`-agentpath:<dll>[=<options>]`) in a launcher's JVM arguments.
 *
 * The arguments are edited as text: only the reminedog argument changes, everything else
 * (spacing, quotes) stays as it was. An argument is reminedog's when its DLL's file name starts
 * with "reminedog" and ends with ".dll".
 */
object AgentArg {
    const val PREFIX = "-agentpath:"

    /**
     * Why a DLL path cannot be put in the JVM arguments, or null when it can.
     *
     * The launchers split the arguments at spaces (MCSR Launcher ignores quotes), the Java
     * launcher reads its command line in the ANSI code page, and the JVM ends the path at '='.
     */
    fun problem(path: String): String? {
        if (path.isEmpty()) return "DLL の場所が空です"
        for (c in path) {
            when {
                c == ' ' || c == '\u3000' -> return "パスに空白（スペース）が入っています"
                c < '!' || c > '~' -> return "パスに日本語などの英数字以外の文字が入っています"
                c == '=' || c == '"' || c == '\'' -> return "パスに記号「$c」が入っています"
            }
        }
        if (!(path.length > 2 && path[0].isLetter() && path[1] == ':' && path[2] == '\\')) {
            return "ドライブから始まるパス（C:\\... など）にしてください"
        }
        if (!isAgentFile(fileName(path))) return "ファイル名は reminedog で始まる .dll にしてください（例：reminedog.dll）"
        return null
    }

    fun token(path: String, options: String = ""): String =
        PREFIX + path + if (options.isEmpty()) "" else "=$options"

    /** The reminedog arguments in an argument string, in order. */
    fun find(args: String): List<FoundAgent> {
        val found = mutableListOf<FoundAgent>()
        var i = 0
        while (i < args.length) {
            if (args[i].isWhitespace()) {
                i++
                continue
            }
            val start = i
            while (i < args.length && !args[i].isWhitespace()) i++
            // Prism and MultiMC accept an argument in quotes.
            val word = args.substring(start, i).let {
                if (it.length >= 2 && it.startsWith('"') && it.endsWith('"')) it.substring(1, it.length - 1) else it
            }
            if (!word.startsWith(PREFIX)) continue
            val rest = word.removePrefix(PREFIX)
            val path = rest.substringBefore('=')
            if (isAgentFile(fileName(path))) {
                found += FoundAgent(start, i, path, rest.substringAfter('=', ""))
            }
        }
        return found
    }

    /**
     * The arguments with reminedog loaded from [path]: the first reminedog argument is replaced
     * (keeping its options), any others are removed, and if there was none it is added at the end.
     */
    fun install(args: String, path: String): String {
        val found = find(args)
        if (found.isEmpty()) {
            val trimmed = args.trimEnd()
            return if (trimmed.isEmpty()) token(path) else "$trimmed ${token(path)}"
        }
        val first = found.first()
        val replaced = args.substring(0, first.start) + token(path, first.options) + args.substring(first.end)
        val shift = replaced.length - args.length
        return cut(replaced, found.drop(1).map { it.copy(start = it.start + shift, end = it.end + shift) })
    }

    /** The arguments without any reminedog argument. */
    fun remove(args: String): String = cut(args, find(args))

    fun samePath(a: String, b: String): Boolean = normalize(a) == normalize(b)

    fun fileName(path: String): String = path.substringAfterLast('\\').substringAfterLast('/')

    private fun normalize(path: String) = path.replace('/', '\\').lowercase()

    private fun isAgentFile(name: String): Boolean {
        val lower = name.lowercase()
        return lower.startsWith("reminedog") && lower.endsWith(".dll")
    }

    /** Removes the found arguments with the spaces before them (after them for the first word). */
    private fun cut(args: String, found: List<FoundAgent>): String {
        val out = StringBuilder(args)
        for (f in found.asReversed()) {
            var start = f.start
            var end = f.end
            while (start > 0 && out[start - 1].isWhitespace()) start--
            if (start == 0) {
                while (end < out.length && out[end].isWhitespace()) end++
            }
            out.delete(start, end)
        }
        return out.toString()
    }
}
