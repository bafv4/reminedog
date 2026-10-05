package reminedog.installer

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue

class JsonTest {
    @Test
    fun `rewriting keeps the order, the numbers and the text`() {
        val text = """
            {
              "profiles" : {
                "b" : { "name" : "Fabric", "javaArgs" : "-Xmx4G", "icon" : "data:image/png;base64,iVBOR+/=" },
                "a" : { "name" : "日本語 \"quoted\" \\ \t", "type" : "custom" }
              },
              "version" : 3,
              "ratio" : 1.50e2,
              "flag" : false,
              "nothing" : null,
              "list" : [ 1, "two", [] ]
            }
        """.trimIndent()
        val parsed = Json.parse(text)
        val written = Json.write(parsed, "  ")
        assertEquals(written, Json.write(Json.parse(written), "  "))
        val root = parsed.asJsonObject()!!
        assertEquals(listOf("profiles", "version", "ratio", "flag", "nothing", "list"), root.keys.toList())
        assertEquals(listOf("b", "a"), root["profiles"].asJsonObject()!!.keys.toList())
        assertEquals("1.50e2", root["ratio"].toString())
        assertEquals("日本語 \"quoted\" \\ \t", root["profiles"].asJsonObject()!!["a"].asJsonObject()!!["name"])
        assertTrue(written.contains("\"ratio\": 1.50e2"))
        assertTrue(written.contains("\"nothing\": null"))
    }

    @Test
    fun `escapes and the indent of a file are read`() {
        assertEquals("a\u00e9\n/", Json.parse("\"a\\u00e9\\n\\/\""))
        assertEquals("  ", Json.indentOf("{\n  \"a\": 1\n}"))
        assertEquals("    ", Json.indentOf("{\"a\":1}"))
    }

    @Test
    fun `broken JSON is refused`() {
        for (text in listOf("{", "{\"a\" 1}", "[1,]", "{} x", "\"\\q\"", "-")) {
            assertFailsWith<IllegalArgumentException>(text) { Json.parse(text) }
        }
    }
}
