package reminedog.installer

import com.formdev.flatlaf.FlatDarkLaf
import com.formdev.flatlaf.FlatLightLaf
import java.awt.GraphicsEnvironment
import java.io.IOException
import java.util.concurrent.TimeUnit
import javax.swing.SwingUtilities
import kotlin.system.exitProcess

/**
 * reminedog のインストーラー：reminedog.dll を PC に置き、ランチャーのインスタンスの JVM 引数に
 * `-agentpath:<dll>` を足す。
 */
fun main() {
    if (GraphicsEnvironment.isHeadless()) {
        System.err.println("The reminedog installer needs a desktop; it has no command-line mode.")
        exitProcess(1)
    }
    val dark = windowsUsesDarkMode()
    SwingUtilities.invokeLater {
        if (dark) FlatDarkLaf.setup() else FlatLightLaf.setup()
        InstallerFrame().isVisible = true
    }
}

/** Windows がアプリをダークモードで表示するか（AppsUseLightTheme が 0）。 */
private fun windowsUsesDarkMode(): Boolean =
    try {
        val process = ProcessBuilder(
            system32("reg.exe"), "query", "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
            "/v", "AppsUseLightTheme",
        ).redirectErrorStream(true).start()
        val output = process.inputStream.bufferedReader().readText()
        process.waitFor(5, TimeUnit.SECONDS)
        // "    AppsUseLightTheme    REG_DWORD    0x0"
        Regex("""AppsUseLightTheme\s+REG_DWORD\s+0x([0-9a-fA-F]+)""").find(output)?.groupValues?.get(1)?.toLong(16) == 0L
    } catch (e: IOException) {
        false
    }
