package reminedog.installer

import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.nio.file.FileSystemException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption
import java.security.MessageDigest

/** GitHub の最新のリリースから reminedog.dll をダウンロードする。 */
object Download {
    const val REPOSITORY = "bafv4/reminedog"

    /** 版によらない PC での DLL の名前（JVM 引数が同じファイルを指し続けるように）。 */
    const val ASSET = "reminedog.dll"

    /** リリースの DLL：`reminedog-1.2.3.dll`（か `reminedog.dll`）。 */
    private val RELEASE_DLL = Regex("""reminedog(-[0-9A-Za-z.+-]+)?\.dll""", RegexOption.IGNORE_CASE)
    private const val API = "https://api.github.com/repos/$REPOSITORY/releases/latest"

    /** このインストーラーの版。jar の manifest から（なしでビルドしたら "dev"）。 */
    val INSTALLER_VERSION: String = Download::class.java.`package`?.implementationVersion ?: "dev"

    /**
     * リリースの DLL と、GitHub が答えた大きさと SHA-256（小文字の 16 進）。これで分かるのは
     * ダウンロードが欠けずに届いたことで、誰がファイルを作ったかではない：比べられるのは GitHub の
     * 答えだけ。
     */
    class Release(val tag: String, val url: String, val size: Long, val sha256: String)

    /** ダウンロードした DLL を置けなかった（ゲームが読み込んでいる、フォルダに書けない）。 */
    class PlaceException(message: String, cause: Throwable) : IOException(message, cause)

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

    /** GitHub の API のリリースの JSON を読む。 */
    fun parse(text: String): Release {
        val release = try {
            Json.parse(text).asJsonObject()
        } catch (e: IllegalArgumentException) {
            throw IOException("GitHub の返事を読めません（${e.message}）", e)
        } ?: throw IOException("GitHub の返事を読めません")
        val tag = release["tag_name"] as? String ?: "?"
        val asset = release["assets"].asJsonArray()
            ?.mapNotNull { it.asJsonObject() }
            ?.firstOrNull { (it["name"] as? String)?.let(RELEASE_DLL::matches) == true }
            ?: throw IOException("リリース $tag に reminedog の DLL がありません")
        val url = asset["browser_download_url"] as? String
        val size = (asset["size"] as? JsonNumber)?.text?.toLongOrNull()
        if (url == null || size == null) throw IOException("リリース $tag の DLL の情報を読めません")
        val sha256 = (asset["digest"] as? String)?.takeIf { it.startsWith("sha256:") }?.removePrefix("sha256:")?.lowercase()
            ?: throw IOException("リリース $tag の DLL にハッシュ（SHA-256）がないので、ダウンロードを確かめられません")
        return Release(tag, url, size, sha256)
    }

    /**
     * リリースの DLL を [target] に置く（一時ファイルを通し、大きさとハッシュを確かめる）。
     * そこのファイルがもう同じなら false を返す。[onDownload] はダウンロードの直前に動く。
     * 置けないファイルは [PlaceException] を投げる。
     */
    fun save(release: Release, target: Path, onDownload: () -> Unit = {}): Boolean =
        when (val result = replaceAll(release, listOf(target), onDownload).single()) {
            is Replaced.Updated -> true
            is Replaced.AlreadyLatest -> false
            is Replaced.Failed -> throw PlaceException(result.reason, result.cause)
        }

    /** [target] がリリースの DLL と違うファイル（テスト用のビルド、古い版）か。 */
    fun differs(release: Release, target: Path): Boolean = hashOrNull(target)?.let { it != release.sha256 } ?: false

    /** [replaceAll] が 1 つの DLL のファイルにしたこと。 */
    sealed class Replaced(val path: Path) {
        class Updated(path: Path) : Replaced(path)

        class AlreadyLatest(path: Path) : Replaced(path)

        class Failed(path: Path, val reason: String, val cause: Throwable) : Replaced(path)
    }

    /**
     * リリースの DLL を [targets]（インスタンスが読み込む DLL。それぞれ 1 回）のそれぞれに置く。
     * ダウンロードは 1 回で、GitHub のハッシュからどのファイルももう同じと分かれば、ダウンロード
     * しない。[onDownload] はダウンロードの直前に動く。置き換えられない置き先（ゲームが読み込んで
     * いる）はそれだけが失敗する。ダウンロードの失敗は投げる。
     */
    fun replaceAll(release: Release, targets: List<Path>, onDownload: () -> Unit = {}): List<Replaced> {
        val hashes = targets.associateWith { hashOrNull(it) }
        if (hashes.values.all { it == release.sha256 }) return targets.map { Replaced.AlreadyLatest(it) }
        onDownload()
        val (dll, hash) = fetch(release)
        try {
            return targets.map { target ->
                if (hashes[target] == hash) {
                    Replaced.AlreadyLatest(target)
                } else {
                    try {
                        place(dll, target)
                        Replaced.Updated(target)
                    } catch (e: IOException) {
                        Replaced.Failed(target, e.message ?: e.javaClass.simpleName, e)
                    }
                }
            }
        } finally {
            Files.deleteIfExists(dll)
        }
    }

    /**
     * リリースの DLL を一時ファイルにダウンロードして確かめる。ファイルとその SHA-256。
     * ファイルは呼び出し側が消す。
     */
    private fun fetch(release: Release): Pair<Path, String> {
        val file = Files.createTempFile("reminedog-", ".dll")
        try {
            val connection = open(release.url)
            connection.setRequestProperty("Accept", "application/octet-stream")
            val code = connection.responseCode
            if (code != 200) throw IOException("ダウンロードで HTTP $code が返りました")
            connection.inputStream.use { input -> Files.newOutputStream(file).use { input.copyTo(it) } }
            return file to check(release, file)
        } catch (e: Exception) {
            Files.deleteIfExists(file)
            throw e
        }
    }

    /**
     * [target] を、隣の一時ファイルを通して [dll] で置き換える。そのために作ったフォルダは、利用者
     * （と SYSTEM と管理者）だけが書けるようにする。
     */
    private fun place(dll: Path, target: Path) {
        val temp = target.resolveSibling("${target.fileName}.download")
        try {
            try {
                if (!Files.isDirectory(target.parent)) {
                    Files.createDirectories(target.parent)
                    Acl.restrict(target.parent)
                }
                Files.copy(dll, temp, StandardCopyOption.REPLACE_EXISTING)
            } catch (e: FileSystemException) {
                throw IOException("${target.parent} に書き込めません（${e.reason ?: e.javaClass.simpleName}）", e)
            }
            try {
                moveReplacing(temp, target)
            } catch (e: FileSystemException) {
                throw IOException("$target を置き換えられません。reminedog を読み込んだゲームが起動していたら閉じてください", e)
            }
        } finally {
            // フォルダに書けないところでは、一時ファイルの削除も失敗しうる（Linux で、ファイルである
            // パスの下）。理由は上のエラーが言うので、こちらは捨てる。
            runCatching { Files.deleteIfExists(temp) }
        }
    }

    /** ファイルの SHA-256。ないか読めなければ null。 */
    private fun hashOrNull(file: Path): String? =
        try {
            if (Files.isRegularFile(file)) sha256(file) else null
        } catch (e: IOException) {
            null
        }

    /** ダウンロードした DLL を確かめる。その SHA-256。 */
    private fun check(release: Release, file: Path): String {
        val size = Files.size(file)
        if (size != release.size) throw IOException("ダウンロードが途中で切れました（$size / ${release.size} バイト）")
        val hash = sha256(file)
        if (release.sha256 != hash) throw IOException("ダウンロードした DLL のハッシュが合いません")
        val head = Files.newInputStream(file).use { input -> ByteArray(2).also { if (input.read(it) != 2) it.fill(0) } }
        if (head[0] != 'M'.code.toByte() || head[1] != 'Z'.code.toByte()) throw IOException("ダウンロードしたファイルが DLL ではありません")
        return hash
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
