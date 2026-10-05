package reminedog.installer

import java.nio.file.Files
import java.nio.file.Path
import org.junit.jupiter.api.io.TempDir
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertTrue

class LauncherTest {
    @TempDir
    lateinit var dir: Path

    private val dll = "C:\\reminedog\\reminedog.dll"
    private val agent = "-agentpath:$dll"

    private fun write(relative: String, text: String): Path {
        val file = dir.resolve(relative)
        Files.createDirectories(file.parent)
        Files.write(file, text.toByteArray(Charsets.UTF_8))
        return file
    }

    private fun Launcher.only(name: String) = instances().single { it.name == name }

    @Test
    fun `Prism - an instance on the launcher's arguments gets its own, starting from them`() {
        write("prism/prismlauncher.cfg", "[General]\r\nConfigVersion=1.3\r\nJvmArgs=-Xmx4G\r\n")
        val cfg = write(
            "prism/instances/1.21.11/instance.cfg",
            "[General]\r\nConfigVersion=1.3\r\nInstanceType=OneSix\r\nname=1.21.11\r\n[UI]\r\nfoo=bar\r\n",
        )
        write("prism/instances/1.21.11/mmc-pack.json", """{"components": [{"uid": "net.minecraft", "version": "1.21.11"}]}""")
        write("prism/instances/_MMC_TEMP/instance.cfg", "name=temp\n")
        val launcher = Launchers.at(dir.resolve("prism")).single()
        assertEquals("Prism Launcher", launcher.label)

        val instance = launcher.instances().single()
        assertEquals("1.21.11", instance.detail)
        assertTrue(instance.read().shared)

        assertIs<Change.Installed>(instance.install(dll))
        val args = instance.read()
        assertFalse(args.shared)
        assertEquals("-Xmx4G $agent", args.text)
        val text = readUtf8(cfg)
        assertTrue(text.contains("OverrideJavaArgs=true\r\n[UI]\r\nfoo=bar\r\n"), text)
        assertTrue(text.contains("JvmArgs=-Xmx4G -agentpath:C:\\\\reminedog\\\\reminedog.dll\r\n"), text)

        assertEquals(Change.AlreadyInstalled, instance.install(dll))
        assertIs<Change.Removed>(instance.remove())
        assertEquals("-Xmx4G", instance.read().text)
        assertEquals(Change.NotInstalled, instance.remove())
    }

    @Test
    fun `Prism - an agent in the launcher's arguments is not removed from one instance`() {
        write("prism/prismlauncher.cfg", "[General]\nConfigVersion=1.3\nJvmArgs=$agent\n".replace("\\", "\\\\"))
        write("prism/instances/a/instance.cfg", "[General]\nConfigVersion=1.3\nname=a\n")
        val instance = Launchers.at(dir.resolve("prism")).single().only("a")
        assertEquals(agent, instance.read().text)
        assertEquals(Change.SharedOnly, instance.remove())
    }

    @Test
    fun `MultiMC - the old format and the OverrideJava switch`() {
        write("mmc/multimc.cfg", "InstanceDir=insts\nJvmArgs=-Xmx2G\n")
        val cfg = write("mmc/insts/rsg/instance.cfg", "OverrideJava=true\nJvmArgs=-XX:+UseZGC\nname=RSG \\#1\n")
        val launcher = Launchers.find(dir.resolve("mmc/insts/rsg")).single()
        assertEquals("MultiMC", launcher.label)
        val instance = launcher.only("RSG #1")
        assertEquals("-XX:+UseZGC", instance.read().text)

        instance.install(dll)
        assertEquals(
            "OverrideJava=true\nJvmArgs=-XX:+UseZGC -agentpath:C:\\\\reminedog\\\\reminedog.dll\nname=RSG \\#1\nOverrideJavaArgs=true\n",
            readUtf8(cfg),
        )
    }

    @Test
    fun `MCSR - an instance on the launcher's Java settings gets a copy of them`() {
        write(
            "MCSRLauncher/launcher/options.json",
            """
            {
                "javaPath": "C:\\java\\bin\\javaw.exe",
                "jvmArguments": "-XX:+UseZGC",
                "minMemory": 9728,
                "maxMemory": 9728
            }
            """.trimIndent(),
        )
        write("MCSRLauncher/MCSRLauncher.jar", "")
        val file = write(
            "MCSRLauncher/launcher/instances/Server/instance.json",
            """
            {
                "id": "Server",
                "displayName": "Server (1.21)",
                "minecraftVersion": "1.21.11",
                "playTime": 43768
            }
            """.trimIndent() + "\n",
        )
        val launcher = Launchers.find(dir.resolve("MCSRLauncher")).single()
        val instance = launcher.only("Server (1.21)")
        assertEquals("1.21.11", instance.detail)
        assertTrue(instance.read().shared)

        assertIs<Change.Installed>(instance.install(dll))
        val json = JsonDoc.load(file).root
        assertEquals(listOf("id", "displayName", "minecraftVersion", "playTime", "options"), json.keys.toList())
        val options = json["options"].asJsonObject()!!
        assertEquals(false, options["useLauncherJavaOption"])
        assertEquals("C:\\java\\bin\\javaw.exe", options["javaPath"])
        assertEquals("9728", options["minMemory"].toString())
        assertEquals("-XX:+UseZGC $agent", options["jvmArguments"])
        assertTrue(readUtf8(file).contains("\n    \"playTime\": 43768,\n"))
        assertTrue(readUtf8(file).endsWith("}\n"))

        assertEquals(Change.AlreadyInstalled, instance.install(dll))
        assertIs<Change.Removed>(instance.remove())
        assertEquals("-XX:+UseZGC", instance.read().text)
    }

    @Test
    fun `MCSR - an instance with its own Java settings keeps them`() {
        write("launcher/options.json", """{"jvmArguments": ""}""")
        write("launcher/instances/rsg/instance.json", """{"id": "rsg", "options": {"useLauncherJavaOption": false, "maxMemory": 10240}}""")
        val instance = Launchers.at(dir.resolve("launcher")).single().only("rsg")
        assertEquals("", instance.read().text)
        instance.install(dll)
        val options = JsonDoc.load(dir.resolve("launcher/instances/rsg/instance.json")).root["options"].asJsonObject()!!
        assertEquals(listOf("useLauncherJavaOption", "maxMemory", "jvmArguments"), options.keys.toList())
        assertEquals(agent, options["jvmArguments"])
    }

    @Test
    fun `official launcher - a profile without javaArgs keeps the default ones`() {
        val file = write(
            ".minecraft/launcher_profiles.json",
            """
            {
              "profiles" : {
                "abc" : { "type" : "latest-release", "name" : "", "lastVersionId" : "latest-release" },
                "def" : { "type" : "custom", "name" : "Fabric", "lastVersionId" : "fabric-loader-1.21.11", "javaArgs" : "-Xmx6G" }
              },
              "version" : 3
            }
            """.trimIndent(),
        )
        write(".minecraft/launcher_profiles_microsoft_store.json", """{"profiles": {}}""")
        val launchers = Launchers.at(dir.resolve(".minecraft"))
        assertEquals(listOf("Minecraft Launcher", "Minecraft Launcher（Microsoft Store）"), launchers.map { it.label })

        val latest = launchers[0].only("最新のリリース")
        assertEquals("", latest.detail)
        assertEquals(MojangLauncher.DEFAULT_ARGS, latest.read().text)
        latest.install(dll)
        assertEquals("${MojangLauncher.DEFAULT_ARGS} $agent", latest.read().text)

        val fabric = launchers[0].only("Fabric")
        assertEquals("fabric-loader-1.21.11", fabric.detail)
        fabric.install(dll)
        assertEquals("-Xmx6G $agent", fabric.read().text)
        assertTrue(readUtf8(file).startsWith("{\n  \"profiles\": {\n    \"abc\": {"))
        fabric.remove()
        assertEquals("-Xmx6G", fabric.read().text)
    }
}
