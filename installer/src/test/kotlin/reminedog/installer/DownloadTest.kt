package reminedog.installer

import com.sun.net.httpserver.HttpServer
import java.io.IOException
import java.net.InetAddress
import java.net.InetSocketAddress
import java.nio.file.Files
import java.nio.file.Path
import java.security.MessageDigest
import java.util.concurrent.atomic.AtomicInteger
import org.junit.jupiter.api.io.TempDir
import kotlin.streams.toList
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertTrue

class DownloadTest {
    @TempDir
    lateinit var dir: Path

    private val dll = byteArrayOf('M'.code.toByte(), 'Z'.code.toByte(), 1, 2, 3)

    /** A local HTTP server with the files a download asks for, counting the requests. */
    private class Server(files: Map<String, ByteArray>) : AutoCloseable {
        private val server = HttpServer.create(InetSocketAddress(InetAddress.getLoopbackAddress(), 0), 0)
        val requests = AtomicInteger()

        init {
            for ((path, body) in files) {
                server.createContext(path) { exchange ->
                    requests.incrementAndGet()
                    exchange.sendResponseHeaders(200, body.size.toLong())
                    exchange.responseBody.use { it.write(body) }
                }
            }
            server.start()
        }

        fun url(path: String) = "http://127.0.0.1:${server.address.port}$path"

        override fun close() = server.stop(0)
    }

    private fun sha256(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

    private fun write(relative: String, bytes: ByteArray): Path {
        val file = dir.resolve(relative)
        Files.createDirectories(file.parent)
        Files.write(file, bytes)
        return file
    }

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
    fun `the DLL of a release is found by its versioned name`() {
        val release = Download.parse(
            """
            {"tag_name": "v1.2.3", "assets": [
              {"name": "reminedog-installer-1.2.3.jar", "browser_download_url": "https://example.invalid/jar", "size": 3},
              {"name": "reminedog-1.2.3.pdb", "browser_download_url": "https://example.invalid/pdb", "size": 2},
              {"name": "reminedog-1.2.3-beta.1.dll", "browser_download_url": "https://example.invalid/dll", "size": 1,
               "digest": "sha256:00FF"}
            ]}
            """.trimIndent(),
        )
        assertEquals("https://example.invalid/dll", release.url)
        assertEquals("00ff", release.sha256)
    }

    @Test
    fun `a release without a hash is refused`() {
        val e = assertFailsWith<IOException> {
            Download.parse(
                """{"tag_name": "v1", "assets": [{"name": "reminedog.dll", "browser_download_url": "x", "size": 1}]}""",
            )
        }
        assertTrue(e.message!!.contains("ハッシュ"), e.message)
    }

    @Test
    fun `a release without the DLL is refused`() {
        assertFailsWith<IOException> {
            Download.parse("""{"tag_name": "v1", "assets": [{"name": "other.dll", "browser_download_url": "x", "size": 1}]}""")
        }
        assertFailsWith<IOException> { Download.parse("""{"tag_name": "v1", "assets": []}""") }
    }

    @Test
    fun `a downloaded DLL is checked before it replaces the file`() {
        Server(mapOf("/dll" to dll, "/other" to byteArrayOf(1, 2, 3, 4, 5))).use { server ->
            val target = dir.resolve("reminedog/reminedog.dll")
            val release = Download.Release("v1", server.url("/dll"), dll.size.toLong(), sha256(dll))

            assertTrue(Download.save(release, target))
            assertContentEquals(dll, Files.readAllBytes(target))
            assertFalse(Download.save(release, target), "the same file is not downloaded again")

            val other = dir.resolve("other/reminedog.dll")
            val hash = sha256(dll)
            assertFailsWith<IOException> { Download.save(Download.Release("v1", server.url("/dll"), 6, hash), other) }
            assertFailsWith<IOException> { Download.save(Download.Release("v1", server.url("/dll"), 5, "00"), other) }
            val notDll = byteArrayOf(1, 2, 3, 4, 5)
            assertFailsWith<IOException> { Download.save(Download.Release("v1", server.url("/other"), 5, sha256(notDll)), other) }
            assertFailsWith<IOException> { Download.save(Download.Release("v1", server.url("/missing"), 5, hash), other) }
            assertFalse(Files.exists(other.parent), "nothing is written when the download fails")
        }
    }

    @Test
    fun `replacing downloads once and changes only the files that differ`() {
        Server(mapOf("/dll" to dll)).use { server ->
            val release = Download.Release("v2", server.url("/dll"), dll.size.toLong(), sha256(dll))
            val stale = write("a/reminedog.dll", byteArrayOf('M'.code.toByte(), 'Z'.code.toByte(), 9))
            val same = write("b/reminedog.dll", dll)
            val missing = dir.resolve("c/reminedog.dll")
            var downloads = 0

            val results = Download.replaceAll(release, listOf(stale, same, missing)) { downloads++ }
            assertIs<Download.Replaced.Updated>(results[0])
            assertIs<Download.Replaced.AlreadyLatest>(results[1])
            assertIs<Download.Replaced.Updated>(results[2])
            assertEquals(listOf(stale, same, missing), results.map { it.path })
            assertEquals(1, downloads)
            assertEquals(1, server.requests.get())
            for (file in listOf(stale, same, missing)) assertContentEquals(dll, Files.readAllBytes(file))
            assertEquals(listOf("reminedog.dll"), Files.list(stale.parent).use { files -> files.map { it.fileName.toString() }.toList() })

            // Every file is the same as GitHub's hash: nothing is downloaded.
            val again = Download.replaceAll(release, listOf(stale, same, missing)) { downloads++ }
            assertTrue(again.all { it is Download.Replaced.AlreadyLatest })
            assertEquals(1, downloads)
            assertEquals(1, server.requests.get())

            // Another file there is told apart from the release's.
            assertFalse(Download.differs(release, stale))
            val test = write("d/reminedog.dll", byteArrayOf('M'.code.toByte(), 'Z'.code.toByte(), 7))
            assertTrue(Download.differs(release, test))
            assertFalse(Download.differs(release, dir.resolve("none/reminedog.dll")))
        }
    }

    @Test
    fun `a file that cannot be replaced fails alone`() {
        Server(mapOf("/dll" to dll)).use { server ->
            val release = Download.Release("v2", server.url("/dll"), dll.size.toLong(), sha256(dll))
            // The folder of this target is a file, so nothing can be written there.
            val blocked = write("blocked", byteArrayOf(0)).resolve("reminedog.dll")
            val fine = dir.resolve("fine/reminedog.dll")

            val results = Download.replaceAll(release, listOf(blocked, fine))
            val failed = assertIs<Download.Replaced.Failed>(results[0])
            assertTrue(failed.reason.contains("書き込めません"), failed.reason)
            assertIs<Download.Replaced.Updated>(results[1])
            assertContentEquals(dll, Files.readAllBytes(fine))
            assertFailsWith<Download.PlaceException> { Download.save(release, blocked) }
        }
    }
}
