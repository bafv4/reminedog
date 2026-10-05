package reminedog.installer

import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.nio.file.AtomicMoveNotSupportedException
import java.nio.file.FileSystemException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.security.MessageDigest

/** Downloads reminedog.dll from the latest GitHub release. */
object Download {
    const val REPOSITORY = "bafv4/reminedog"
    const val ASSET = "reminedog.dll"
    private const val API = "https://api.github.com/repos/$REPOSITORY/releases/latest"

    /** The DLL of a release. [sha256] is lowercase hex, or null when GitHub did not give one. */
    class Release(val tag: String, val url: String, val size: Long, val sha256: String?)

    fun latest(): Release {
        val connection = open(API)
        connection.setRequestProperty("Accept", "application/vnd.github+json")
        when (val code = connection.responseCode) {
            200 -> {}
            404 -> throw IOException("GitHub（$REPOSITORY）に公開されているリリースがありません")
            403, 429 -> throw IOException("GitHub の API の利用回数の上限に達しました。しばらくしてから試してください")
            else -> throw IOException("GitHub から HTTP $code が返りました")
        }
        return parse(connection.inputStream.use { String(it.readBytes(), Charsets.UTF_8) })
    }

    /** Reads the release JSON of the GitHub API. */
    fun parse(text: String): Release {
        val release = try {
            Json.parse(text).asJsonObject()
        } catch (e: IllegalArgumentException) {
            throw IOException("GitHub の返事を読めません（${e.message}）", e)
        } ?: throw IOException("GitHub の返事を読めません")
        val tag = release["tag_name"] as? String ?: "?"
        val asset = release["assets"].asJsonArray()
            ?.mapNotNull { it.asJsonObject() }
            ?.firstOrNull { (it["name"] as? String).equals(ASSET, ignoreCase = true) }
            ?: throw IOException("リリース $tag に $ASSET がありません")
        val url = asset["browser_download_url"] as? String
        val size = (asset["size"] as? JsonNumber)?.text?.toLongOrNull()
        if (url == null || size == null) throw IOException("リリース $tag の $ASSET の情報を読めません")
        val sha256 = (asset["digest"] as? String)?.takeIf { it.startsWith("sha256:") }?.removePrefix("sha256:")?.lowercase()
        return Release(tag, url, size, sha256)
    }

    /**
     * Puts the release's DLL at [target] (through a temporary file, checking its size and hash).
     * Returns false when the file there is already the same.
     */
    fun save(release: Release, target: Path): Boolean {
        if (release.sha256 != null && Files.isRegularFile(target) && release.sha256 == sha256(target)) return false
        Files.createDirectories(target.parent)
        val temp = target.resolveSibling("${target.fileName}.download")
        try {
            val connection = open(release.url)
            connection.setRequestProperty("Accept", "application/octet-stream")
            val code = connection.responseCode
            if (code != 200) throw IOException("ダウンロードで HTTP $code が返りました")
            connection.inputStream.use { input -> Files.newOutputStream(temp).use { input.copyTo(it) } }
            check(release, temp)
            try {
                try {
                    Files.move(temp, target, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
                } catch (e: AtomicMoveNotSupportedException) {
                    Files.move(temp, target, StandardCopyOption.REPLACE_EXISTING)
                }
            } catch (e: FileSystemException) {
                throw IOException("$target を置き換えられません。reminedog を読み込んだゲームが起動していたら閉じてください", e)
            }
            return true
        } finally {
            Files.deleteIfExists(temp)
        }
    }

    private fun check(release: Release, file: Path) {
        val size = Files.size(file)
        if (size != release.size) throw IOException("ダウンロードが途中で切れました（$size / ${release.size} バイト）")
        if (release.sha256 != null && release.sha256 != sha256(file)) throw IOException("ダウンロードした DLL のハッシュが合いません")
        val head = Files.newInputStream(file).use { input -> ByteArray(2).also { if (input.read(it) != 2) it.fill(0) } }
        if (head[0] != 'M'.code.toByte() || head[1] != 'Z'.code.toByte()) throw IOException("ダウンロードしたファイルが DLL ではありません")
    }

    fun sha256(file: Path): String {
        val digest = MessageDigest.getInstance("SHA-256")
        Files.newInputStream(file).use { input ->
            val buffer = ByteArray(64 * 1024)
            while (true) {
                val n = input.read(buffer)
                if (n == -1) break
                digest.update(buffer, 0, n)
            }
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    private fun open(url: String): HttpURLConnection =
        (URL(url).openConnection() as HttpURLConnection).apply {
            setRequestProperty("User-Agent", "reminedog-installer")
            connectTimeout = 15_000
            readTimeout = 30_000
            instanceFollowRedirects = true
        }
}
