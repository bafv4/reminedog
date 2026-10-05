package reminedog.installer

import com.sun.net.httpserver.HttpServer
import java.io.IOException
import java.net.InetAddress
import java.net.InetSocketAddress
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import org.junit.jupiter.api.io.TempDir
import kotlin.streams.toList
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class DownloadTest {
    @Test
    fun `the DLL of a release is found with its hash`() {
        val release = Download.parse(
            """
            {"tag_name": "v0.1.0", "assets": [
              {"name": "reminedog.pdb", "browser_download_url": "https://example.invalid/pdb", "size": 1},
              {"name": "reminedog.dll", "browser_download_url": "https://example.invalid/dll", "size": 7980032,
               "digest": "sha256:ABCDEF"}
            ]}
            """.trimIndent(),
        )
        assertEquals("v0.1.0", release.tag)
        assertEquals("https://example.invalid/dll", release.url)
        assertEquals(7980032L, release.size)
        assertEquals("abcdef", release.sha256)
    }

    @Test
    fun `a release without the DLL is refused`() {
        assertFailsWith<IOException> { Download.parse("""{"tag_name": "v1", "assets": []}""") }
    }

    @Test
    fun `a downloaded DLL is checked before it replaces the file`(@TempDir dir: Path) {
        val dll = byteArrayOf('M'.code.toByte(), 'Z'.code.toByte(), 1, 2, 3)
        val notDll = byteArrayOf(1, 2, 3, 4, 5)
        val server = HttpServer.create(InetSocketAddress(InetAddress.getLoopbackAddress(), 0), 0)
        for ((path, body) in listOf("/dll" to dll, "/other" to notDll)) {
            server.createContext(path) { exchange ->
                exchange.sendResponseHeaders(200, body.size.toLong())
                exchange.responseBody.use { it.write(body) }
            }
        }
        server.start()
        try {
            val base = "http://127.0.0.1:${server.address.port}"
            val sha256 = MessageDigest.getInstance("SHA-256").digest(dll).joinToString("") { "%02x".format(it) }
            val target = dir.resolve("reminedog/reminedog.dll")
            val release = Download.Release("v1", "$base/dll", dll.size.toLong(), sha256)

            assertTrue(Download.save(release, target))
            assertContentEquals(dll, Files.readAllBytes(target))
            assertFalse(Download.save(release, target), "the same file is not downloaded again")

            val other = dir.resolve("other/reminedog.dll")
            assertFailsWith<IOException> { Download.save(Download.Release("v1", "$base/dll", 6, null), other) }
            assertFailsWith<IOException> { Download.save(Download.Release("v1", "$base/dll", 5, "00"), other) }
            assertFailsWith<IOException> { Download.save(Download.Release("v1", "$base/other", 5, null), other) }
            assertFailsWith<IOException> { Download.save(Download.Release("v1", "$base/missing", 5, null), other) }
            assertEquals(emptyList(), Files.list(other.parent).use { files -> files.map { it.fileName.toString() }.toList() })
        } finally {
            server.stop(0)
        }
    }
}
