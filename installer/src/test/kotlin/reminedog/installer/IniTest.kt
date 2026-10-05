package reminedog.installer

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class IniTest {
    @Test
    fun `Prism's Qt format is read and written`() {
        val text = "[General]\r\nConfigVersion=1.3\r\nJvmArgs=\"-agentpath:C:\\\\reminedog\\\\reminedog.dll=log=debug\"\r\nname=26.3\r\n[UI]\r\nfoo=1\r\n"
        val ini = Ini.parse(text)
        assertTrue(ini.isQt)
        assertEquals("-agentpath:C:\\reminedog\\reminedog.dll=log=debug", ini["JvmArgs"])
        assertEquals("26.3", ini["name"])

        ini["JvmArgs"] = "-Xmx4G -agentpath:C:\\reminedog\\reminedog.dll"
        ini["OverrideJavaArgs"] = "true"
        assertEquals(
            "[General]\r\nConfigVersion=1.3\r\nJvmArgs=-Xmx4G -agentpath:C:\\\\reminedog\\\\reminedog.dll\r\nname=26.3\r\n" +
                "OverrideJavaArgs=true\r\n[UI]\r\nfoo=1\r\n",
            ini.text(),
        )
        assertEquals("-Xmx4G -agentpath:C:\\reminedog\\reminedog.dll", Ini.parse(ini.text())["JvmArgs"])
        assertEquals(null, ini["foo"])
    }

    @Test
    fun `Qt escapes round trip`() {
        for (value in listOf("a=b", " lead", "trail ", "x;y,z", "q\"uote", "back\\slash", "tab\tnew\nline", "@at", "日本語", "")) {
            assertEquals(value, Ini.qtUnescape(Ini.qtEscape(value)), value)
        }
        assertEquals("\"-Dx=1 -Dy=2\"", Ini.qtEscape("-Dx=1 -Dy=2"))
        assertEquals("-XX:+UseZGC -XX:+AlwaysPreTouch", Ini.qtUnescape("-XX:+UseZGC -XX:+AlwaysPreTouch  "))
    }

    @Test
    fun `MultiMC's format is read and written`() {
        val text = "InstanceType=OneSix\nJvmArgs=-agentpath:C:\\\\old\\\\reminedog.dll\nname=Speedrun \\#1\n"
        val ini = Ini.parse(text)
        assertTrue(!ini.isQt)
        assertEquals("-agentpath:C:\\old\\reminedog.dll", ini["JvmArgs"])
        assertEquals("Speedrun #1", ini["name"])

        ini["JvmArgs"] = "-Dx=1 -agentpath:C:\\reminedog\\reminedog.dll"
        ini["OverrideJavaArgs"] = "true"
        assertEquals(
            "InstanceType=OneSix\nJvmArgs=-Dx=1 -agentpath:C:\\\\reminedog\\\\reminedog.dll\nname=Speedrun \\#1\nOverrideJavaArgs=true\n",
            ini.text(),
        )
        assertTrue(ini.getBool("OverrideJavaArgs"))
    }
}
