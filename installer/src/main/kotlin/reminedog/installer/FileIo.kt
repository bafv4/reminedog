package reminedog.installer

import java.nio.file.AtomicMoveNotSupportedException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption

fun readUtf8(file: Path): String = String(Files.readAllBytes(file), Charsets.UTF_8)

/**
 * Replaces a file with new text through a temporary file next to it, so that a failure never
 * leaves a half-written settings file.
 */
fun writeAtomically(file: Path, text: String) {
    val temp = file.resolveSibling("${file.fileName}.reminedog-tmp")
    try {
        Files.write(temp, text.toByteArray(Charsets.UTF_8))
        try {
            Files.move(temp, file, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
        } catch (e: AtomicMoveNotSupportedException) {
            Files.move(temp, file, StandardCopyOption.REPLACE_EXISTING)
        }
    } finally {
        Files.deleteIfExists(temp)
    }
}
