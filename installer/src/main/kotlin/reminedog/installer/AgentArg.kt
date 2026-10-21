package reminedog.installer

/** 引数の文字列の中に見つけた reminedog の引数：その場所、DLL、エージェントのオプション。 */
data class FoundAgent(val start: Int, val end: Int, val path: String, val options: String)

/**
 * ランチャーの JVM 引数の中の reminedog の引数（`-agentpath:<dll>[=<options>]`）。
 *
 * 引数は文字として書き換える：変えるのは reminedog の引数だけで、ほか（空白、引用符）はそのまま
 * 残す。DLL のファイル名が "reminedog" で始まり ".dll" で終わる引数が reminedog のもの。
 */
object AgentArg {
    const val PREFIX = "-agentpath:"

    /**
     * DLL のパスを JVM 引数に入れられない理由。入れられれば null。
     *
     * ランチャーは引数を空白で分け（MCSR Launcher は引用符を無視する）、Java のランチャーは
     * コマンドラインを ANSI のコードページで読み、JVM はパスを '=' で終える。
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
        if (!isDrivePath(path)) return "ドライブから始まるパス（C:\\... など）にしてください"
        if (!isAgentFile(fileName(path))) return "ファイル名は reminedog で始まる .dll にしてください（例：reminedog.dll）"
        return null
    }

    fun token(path: String, options: String = ""): String =
        PREFIX + path + if (options.isEmpty()) "" else "=$options"

    /** 引数の文字列の中の reminedog の引数（順に）。 */
    fun find(args: String): List<FoundAgent> {
        val found = mutableListOf<FoundAgent>()
        var i = 0
        while (i < args.length) {
            if (args[i].isWhitespace()) {
                i++
                continue
            }
            val start = i
            // Prism と MultiMC は、引用符で囲んだ引数（またはその一部）を空白ごと 1 つにし、
            // 引用符を外す。
            var quoted = false
            while (i < args.length && (quoted || !args[i].isWhitespace())) {
                if (args[i] == '"') quoted = !quoted
                i++
            }
            val word = args.substring(start, i).replace("\"", "")
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
     * [path] から reminedog を読み込む引数：最初の reminedog の引数を置き換え（オプションは残す）、
     * ほかは外し、なければ最後に足す。
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

    /** reminedog の引数をすべて外した引数。 */
    fun remove(args: String): String = cut(args, find(args))

    fun samePath(a: String, b: String): Boolean = pathKey(a) == pathKey(b)

    /** Windows が同じファイルとみなすパス（大文字小文字とスラッシュの向きは問わない）に同じ文字。 */
    fun pathKey(path: String): String = path.replace('/', '\\').lowercase()

    /** パスがドライブ（`C:\` か `C:/`）で始まり、作業フォルダに左右されないか。 */
    fun isDrivePath(path: String): Boolean =
        path.length > 2 && (path[0] in 'A'..'Z' || path[0] in 'a'..'z') && path[1] == ':' && (path[2] == '\\' || path[2] == '/')

    fun fileName(path: String): String = path.substringAfterLast('\\').substringAfterLast('/')

    private fun isAgentFile(name: String): Boolean {
        val lower = name.lowercase()
        return lower.startsWith("reminedog") && lower.endsWith(".dll")
    }

    /** 見つけた引数を、その前の空白ごと外す（最初の語なら後ろの空白ごと）。 */
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
