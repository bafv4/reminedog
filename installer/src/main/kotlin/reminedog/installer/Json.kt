package reminedog.installer

import java.io.IOException
import java.nio.file.Path

/** A JSON number, kept as it was written so that rewriting a file does not change it. */
class JsonNumber(val text: String) {
    override fun toString() = text
}

/**
 * A small JSON reader and writer for the launchers' settings files.
 *
 * Objects are LinkedHashMaps (the order of the keys is kept), arrays are ArrayLists, numbers are
 * [JsonNumber]s and null is Kotlin's null.
 */
object Json {
    fun parse(text: String): Any? {
        val reader = Reader(text)
        reader.skipSpace()
        val value = reader.value()
        reader.skipSpace()
        if (reader.pos != text.length) throw reader.error("unexpected text after the value")
        return value
    }

    /** The JSON text of a parsed value, one member per line with the given indent. */
    fun write(value: Any?, indent: String): String = buildString { writeValue(value, indent, "") }

    /** The indent of the first indented line of a JSON text (four spaces when there is none). */
    fun indentOf(text: String): String {
        val line = text.substringAfter('\n', "")
        val indent = line.takeWhile { it == ' ' || it == '\t' }
        return indent.ifEmpty { "    " }
    }

    private fun StringBuilder.writeValue(value: Any?, indent: String, current: String) {
        when (value) {
            null -> append("null")
            is Map<*, *> -> writeMembers('{', '}', value.entries.toList(), indent, current) { entry, inner ->
                writeString(entry.key as String)
                append(": ")
                writeValue(entry.value, indent, inner)
            }
            is List<*> -> writeMembers('[', ']', value, indent, current) { item, inner ->
                writeValue(item, indent, inner)
            }
            is String -> writeString(value)
            is Boolean, is JsonNumber -> append(value.toString())
            else -> throw IllegalArgumentException("not a JSON value: $value")
        }
    }

    private fun <T> StringBuilder.writeMembers(
        open: Char,
        close: Char,
        members: List<T>,
        indent: String,
        current: String,
        writeMember: StringBuilder.(T, String) -> Unit,
    ) {
        if (members.isEmpty()) {
            append(open).append(close)
            return
        }
        val inner = current + indent
        append(open).append('\n')
        members.forEachIndexed { i, member ->
            if (i > 0) append(",\n")
            append(inner)
            writeMember(member, inner)
        }
        append('\n').append(current).append(close)
    }

    private fun StringBuilder.writeString(s: String) {
        append('"')
        for (c in s) {
            when (c) {
                '"' -> append("\\\"")
                '\\' -> append("\\\\")
                '\n' -> append("\\n")
                '\r' -> append("\\r")
                '\t' -> append("\\t")
                '\b' -> append("\\b")
                '\u000C' -> append("\\f")
                else -> if (c < ' ') append("\\u%04x".format(c.code)) else append(c)
            }
        }
        append('"')
    }

    private class Reader(private val text: String) {
        // A byte order mark is not JSON, but some editors write one.
        var pos = if (text.startsWith('﻿')) 1 else 0

        fun error(message: String) = IllegalArgumentException("JSON: $message at $pos")

        fun skipSpace() {
            while (pos < text.length && text[pos] in " \t\n\r") pos++
        }

        private fun peek(): Char = if (pos < text.length) text[pos] else throw error("unexpected end")

        private fun expect(c: Char) {
            if (peek() != c) throw error("expected '$c'")
            pos++
        }

        fun value(): Any? = when (val c = peek()) {
            '{' -> obj()
            '[' -> array()
            '"' -> string()
            't' -> literal("true", true)
            'f' -> literal("false", false)
            'n' -> literal("null", null)
            '-', in '0'..'9' -> number()
            else -> throw error("unexpected '$c'")
        }

        private fun literal(word: String, value: Any?): Any? {
            if (!text.startsWith(word, pos)) throw error("expected $word")
            pos += word.length
            return value
        }

        private fun obj(): MutableMap<String, Any?> {
            expect('{')
            val map = LinkedHashMap<String, Any?>()
            skipSpace()
            if (peek() == '}') {
                pos++
                return map
            }
            while (true) {
                skipSpace()
                val key = string()
                skipSpace()
                expect(':')
                skipSpace()
                map[key] = value()
                skipSpace()
                if (peek() == ',') {
                    pos++
                    continue
                }
                expect('}')
                return map
            }
        }

        private fun array(): MutableList<Any?> {
            expect('[')
            val list = ArrayList<Any?>()
            skipSpace()
            if (peek() == ']') {
                pos++
                return list
            }
            while (true) {
                skipSpace()
                list.add(value())
                skipSpace()
                if (peek() == ',') {
                    pos++
                    continue
                }
                expect(']')
                return list
            }
        }

        private fun string(): String {
            expect('"')
            val out = StringBuilder()
            while (true) {
                val c = peek()
                pos++
                when (c) {
                    '"' -> return out.toString()
                    '\\' -> {
                        val e = peek()
                        pos++
                        when (e) {
                            '"', '\\', '/' -> out.append(e)
                            'b' -> out.append('\b')
                            'f' -> out.append('\u000C')
                            'n' -> out.append('\n')
                            'r' -> out.append('\r')
                            't' -> out.append('\t')
                            'u' -> {
                                val hex = text.substring(pos, minOf(pos + 4, text.length))
                                if (hex.length < 4 || !hex.all { Character.digit(it, 16) >= 0 }) throw error("bad \\u escape")
                                out.append(hex.toInt(16).toChar())
                                pos += 4
                            }
                            else -> throw error("bad escape")
                        }
                    }
                    else -> out.append(c)
                }
            }
        }

        private fun number(): JsonNumber {
            val start = pos
            if (peek() == '-') pos++
            while (pos < text.length && text[pos] in "0123456789.eE+-") pos++
            val number = text.substring(start, pos)
            if (number.toDoubleOrNull() == null) throw error("bad number")
            return JsonNumber(number)
        }
    }
}

/** This value as a JSON object, or null when it is something else. */
@Suppress("UNCHECKED_CAST")
fun Any?.asJsonObject(): MutableMap<String, Any?>? = this as? MutableMap<String, Any?>

/** This value as a JSON array, or null when it is something else. */
@Suppress("UNCHECKED_CAST")
fun Any?.asJsonArray(): MutableList<Any?>? = this as? MutableList<Any?>

/** A JSON file whose top level is an object, written back with the file's indent and newlines. */
class JsonDoc private constructor(val file: Path, text: String) {
    val root: MutableMap<String, Any?> =
        Json.parse(text).asJsonObject() ?: throw IllegalArgumentException("JSON: the top level is not an object")
    private val indent = Json.indentOf(text)
    private val newline = if ("\r\n" in text) "\r\n" else "\n"
    private val endsWithNewline = text.endsWith("\n")

    fun save() {
        val text = Json.write(root, indent).replace("\n", newline)
        writeAtomically(file, if (endsWithNewline) text + newline else text)
    }

    companion object {
        fun load(file: Path): JsonDoc =
            try {
                JsonDoc(file, readUtf8(file))
            } catch (e: IllegalArgumentException) {
                throw IOException("${file.fileName} を読めません（${e.message}）", e)
            }
    }
}
