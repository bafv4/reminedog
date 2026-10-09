package reminedog.installer

import com.formdev.flatlaf.FlatDarkLaf
import com.formdev.flatlaf.FlatLightLaf
import java.awt.GraphicsEnvironment
import java.io.IOException
import java.util.concurrent.TimeUnit
import javax.swing.SwingUtilities
import kotlin.system.exitProcess

/**
 * The reminedog installer: puts reminedog.dll on the PC and adds `-agentpath:<dll>` to the JVM
 * arguments of launcher instances.
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

/** Whether Windows shows apps in dark mode (AppsUseLightTheme is 0). */
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
