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
        moveReplacing(temp, file)
    } finally {
        Files.deleteIfExists(temp)
    }
}

/** Moves [source] over [target], in one step where the file system can. */
fun moveReplacing(source: Path, target: Path) {
    try {
        Files.move(source, target, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
    } catch (e: AtomicMoveNotSupportedException) {
        Files.move(source, target, StandardCopyOption.REPLACE_EXISTING)
    }
}
