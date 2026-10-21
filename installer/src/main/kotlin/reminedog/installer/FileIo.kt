package reminedog.installer

import java.nio.file.AtomicMoveNotSupportedException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption

fun readUtf8(file: Path): String = String(Files.readAllBytes(file), Charsets.UTF_8)

/**
 * 隣の一時ファイルを通してファイルを新しい文字で置き換える。失敗しても書きかけの設定ファイルを
 * 残さないように。
 */
fun writeAtomically(file: Path, text: String) {
    val temp = file.resolveSibling("${file.fileName}.reminedog-tmp")
    try {
        Files.write(temp, text.toByteArray(Charsets.UTF_8))
        moveReplacing(temp, file)
    } finally {
        Files.deleteIfExists(temp)
    }
}

/** [source] を [target] の上に移す。ファイルシステムができるなら 1 回の操作で。 */
fun moveReplacing(source: Path, target: Path) {
    try {
        Files.move(source, target, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
    } catch (e: AtomicMoveNotSupportedException) {
        Files.move(source, target, StandardCopyOption.REPLACE_EXISTING)
    }
}
