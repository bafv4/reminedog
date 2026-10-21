package reminedog.installer

/**
 * MultiMC と Prism Launcher の設定ファイル（instance.cfg、multimc.cfg、prismlauncher.cfg）。
 * ほかの行をそのまま残すように、行単位で書き換える。
 *
 * 形式は 2 つ。MultiMC は独自のエスケープ（`\\`、`\n`、`\t`、`\#`）の `key=value` の行を書く。
 * Prism Launcher は Qt の INI の形式（`[General]` のセクション、`; , =` を含む値を囲む引用符、
 * C に似たエスケープ）で書き、`ConfigVersion` のキーで印を付ける。そのキーのないファイルは
 * MultiMC の形式で読む。読み書きするのは一番上（`[General]`）のセクションのキーだけ。
 */
class Ini private constructor(text: String) {
    private val newline = if ("\r\n" in text) "\r\n" else "\n"
    private val body = text.removePrefix("﻿")
    private val endsWithNewline = body.isEmpty() || body.endsWith("\n")
    private val lines: MutableList<String> = body.split(Regex("\r?\n")).toMutableList().apply {
        if (endsWithNewline) removeAt(lastIndex)
    }

    /** ファイルが Qt の形式（Prism Launcher が書いたもの）か。 */
    val isQt: Boolean = lineOf("ConfigVersion", sections = true) >= 0

    /** 一番上のキーの値。なければ null。 */
    operator fun get(key: String): String? {
        val index = lineOf(key, isQt)
        if (index < 0) return null
        val raw = lines[index].substringAfter('=')
        return if (isQt) qtUnescape(raw) else multimcUnescape(raw)
    }

    fun getBool(key: String): Boolean = get(key)?.trim().equals("true", ignoreCase = true)

    /** 一番上のキーを設定する。その行を置き換えるか、セクションの最後に足す。 */
    operator fun set(key: String, value: String) {
        val line = "$key=" + if (isQt) qtEscape(value) else multimcEscape(value)
        val index = lineOf(key, isQt)
        if (index >= 0) lines[index] = line else lines.add(insertionPoint(), line)
    }

    fun text(): String {
        val joined = lines.joinToString(newline)
        return if (endsWithNewline && lines.isNotEmpty()) joined + newline else joined
    }

    /** 一番上のキーの最後の行（読むときは最後のものが勝つ）の番号。なければ -1。 */
    private fun lineOf(key: String, sections: Boolean): Int {
        var found = -1
        var top = true
        lines.forEachIndexed { i, raw ->
            val line = raw.trim()
            when {
                sections && line.startsWith("[") -> top = line.equals("[General]", ignoreCase = true)
                !top || line.startsWith("#") || line.startsWith(";") -> {}
                line.indexOf('=') > 0 && line.substringBefore('=').trim() == key -> found = i
            }
        }
        return found
    }

    /** 新しい一番上のキーを置く場所：一番上のセクションの最後の行の後。 */
    private fun insertionPoint(): Int {
        if (!isQt) return lines.size
        var point = 0
        var top = true
        lines.forEachIndexed { i, raw ->
            val line = raw.trim()
            if (line.startsWith("[")) {
                top = line.equals("[General]", ignoreCase = true)
                if (top) point = i + 1
            } else if (top && line.isNotEmpty()) {
                point = i + 1
            }
        }
        return point
    }

    companion object {
        fun parse(text: String) = Ini(text)

        // MultiMC のエスケープ（INIFile::escape / unescape）。Prism は ConfigVersion のないファイルを、
        // エスケープしていない '#' で値を切ってから同じように読む。

        fun multimcEscape(value: String): String = buildString {
            for (c in value) {
                when (c) {
                    '\n' -> append("\\n")
                    '\t' -> append("\\t")
                    '\\' -> append("\\\\")
                    '#' -> append("\\#")
                    else -> append(c)
                }
            }
        }

        fun multimcUnescape(raw: String): String = buildString {
            var escaped = false
            for (c in raw.trim()) {
                when {
                    escaped -> {
                        append(
                            when (c) {
                                'n' -> '\n'
                                't' -> '\t'
                                else -> c
                            },
                        )
                        escaped = false
                    }
                    c == '\\' -> escaped = true
                    c == '#' -> break
                    else -> append(c)
                }
            }
        }

        // Qt の INI のエスケープ（QSettingsPrivate::iniEscapedString / iniUnescapedStringList）。

        fun qtEscape(value: String): String {
            val out = StringBuilder()
            var needsQuotes = false
            var escapeNextIfHex = false
            for (c in if (value.startsWith("@")) "@$value" else value) {
                if (c == ';' || c == ',' || c == '=') needsQuotes = true
                if (escapeNextIfHex && Character.digit(c, 16) >= 0) {
                    out.append("\\x").append(Integer.toHexString(c.code))
                    continue
                }
                escapeNextIfHex = false
                when (c) {
                    '\u0000' -> {
                        out.append("\\0")
                        escapeNextIfHex = true
                    }
                    '\u0007' -> out.append("\\a")
                    '\b' -> out.append("\\b")
                    '\u000C' -> out.append("\\f")
                    '\n' -> out.append("\\n")
                    '\r' -> out.append("\\r")
                    '\t' -> out.append("\\t")
                    '\u000B' -> out.append("\\v")
                    '"', '\\' -> out.append('\\').append(c)
                    else -> if (c < ' ') {
                        out.append("\\x").append(Integer.toHexString(c.code))
                        escapeNextIfHex = true
                    } else {
                        out.append(c)
                    }
                }
            }
            if (needsQuotes || out.startsWith(" ") || out.endsWith(" ")) {
                out.insert(0, '"').append('"')
            }
            return out.toString()
        }

        fun qtUnescape(raw: String): String {
            val out = StringBuilder()
            var quoted = false
            // 末尾の、引用符の外の空白は捨てる：文字はここまで残す。
            var keep = 0
            var i = 0
            while (i < raw.length && (raw[i] == ' ' || raw[i] == '\t')) i++
            while (i < raw.length) {
                val c = raw[i++]
                if (c == '"') {
                    quoted = !quoted
                    keep = out.length
                    continue
                }
                if (c == '\\' && i < raw.length) {
                    val e = raw[i++]
                    when (e) {
                        'a' -> out.append('\u0007')
                        'b' -> out.append('\b')
                        'f' -> out.append('\u000C')
                        'n' -> out.append('\n')
                        'r' -> out.append('\r')
                        't' -> out.append('\t')
                        'v' -> out.append('\u000B')
                        'x' -> {
                            val start = i
                            while (i < raw.length && Character.digit(raw[i], 16) >= 0) i++
                            if (i > start) out.append(raw.substring(start, minOf(i, start + 4)).toInt(16).toChar())
                        }
                        in '0'..'7' -> {
                            val start = i - 1
                            while (i < raw.length && i < start + 3 && raw[i] in '0'..'7') i++
                            out.append(raw.substring(start, i).toInt(8).toChar())
                        }
                        else -> out.append(e)
                    }
                    keep = out.length
                    continue
                }
                out.append(c)
                if (quoted || (c != ' ' && c != '\t')) keep = out.length
            }
            val value = out.substring(0, keep)
            return if (value.startsWith("@@")) value.substring(1) else value
        }
    }
}
