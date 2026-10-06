package reminedog.installer

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

class AgentArgTest {
    private val dll = "C:\\reminedog\\reminedog.dll"

    @Test
    fun `a plain path on a drive is accepted`() {
        assertNull(AgentArg.problem(dll))
        assertNull(AgentArg.problem("D:\\games\\tools\\reminedog-dev.dll"))
    }

    @Test
    fun `paths the launchers or the JVM would cut are refused`() {
        assertNotNull(AgentArg.problem(""))
        assertNotNull(AgentArg.problem("C:\\Program Files\\reminedog\\reminedog.dll"))
        assertNotNull(AgentArg.problem("C:\\ユーザー\\reminedog.dll"))
        assertNotNull(AgentArg.problem("C:\\a=b\\reminedog.dll"))
        assertNotNull(AgentArg.problem("reminedog\\reminedog.dll"))
        assertNotNull(AgentArg.problem("\\\\server\\share\\reminedog.dll"))
        assertNotNull(AgentArg.problem("C:\\reminedog\\agent.dll"))
    }

    @Test
    fun `installing into empty arguments gives just the agent`() {
        assertEquals("-agentpath:$dll", AgentArg.install("", dll))
        assertEquals("-agentpath:$dll", AgentArg.install("  \n", dll))
    }

    @Test
    fun `installing appends after the other arguments`() {
        assertEquals("-Xmx4G -XX:+UseZGC -agentpath:$dll", AgentArg.install("-Xmx4G -XX:+UseZGC  ", dll))
    }

    @Test
    fun `installing replaces the old path in place and keeps the agent options`() {
        val old = "-Xmx4G -agentpath:C:\\old\\reminedog.dll=log=debug -Dfoo=bar"
        assertEquals("-Xmx4G -agentpath:$dll=log=debug -Dfoo=bar", AgentArg.install(old, dll))
    }

    @Test
    fun `installing removes extra copies and leaves other agents alone`() {
        val old = "-agentpath:C:\\a\\reminedog.dll -agentpath:C:\\tools\\other.dll \"-agentpath:C:/b/ReMineDog.DLL\" -Xmx2G"
        assertEquals("-agentpath:$dll -agentpath:C:\\tools\\other.dll -Xmx2G", AgentArg.install(old, dll))
    }

    @Test
    fun `removing keeps the rest of the arguments`() {
        assertEquals("-Xmx4G -Dfoo=bar", AgentArg.remove("-Xmx4G -agentpath:$dll -Dfoo=bar"))
        assertEquals("-Xmx4G", AgentArg.remove("-agentpath:$dll  -Xmx4G"))
        assertEquals("-Xmx4G", AgentArg.remove("-Xmx4G -agentpath:$dll=log=debug"))
        assertEquals("", AgentArg.remove("-agentpath:$dll"))
        assertEquals("-Xmx4G\n-Dx=y", AgentArg.remove("-Xmx4G\n-agentpath:$dll\n-Dx=y"))
    }

    @Test
    fun `finding reports the path and the options`() {
        val found = AgentArg.find("-Xmx4G -agentpath:C:/x/reminedog.dll=log=debug,gamedir=C:\\g")
        assertEquals(1, found.size)
        assertEquals("C:/x/reminedog.dll", found[0].path)
        assertEquals("log=debug,gamedir=C:\\g", found[0].options)
        assertTrue(AgentArg.samePath("c:/X/REMINEDOG.dll", "C:\\x\\reminedog.dll"))
    }

    @Test
    fun `only paths from a drive are taken as the same file wherever the game runs`() {
        assertTrue(AgentArg.isDrivePath("C:\\reminedog\\reminedog.dll"))
        assertTrue(AgentArg.isDrivePath("d:/games/reminedog.dll"))
        assertFalse(AgentArg.isDrivePath("reminedog.dll"))
        assertFalse(AgentArg.isDrivePath("..\\reminedog.dll"))
        assertFalse(AgentArg.isDrivePath("C:reminedog.dll"))
        assertFalse(AgentArg.isDrivePath("\\\\server\\share\\reminedog.dll"))
        assertEquals(AgentArg.pathKey("C:\\Tools\\ReMineDog.dll"), AgentArg.pathKey("c:/tools/reminedog.DLL"))
    }
}
