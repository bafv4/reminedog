package reminedog.installer

import com.formdev.flatlaf.FlatClientProperties
import java.awt.BorderLayout
import java.awt.Color
import java.awt.Component
import java.awt.Cursor
import java.awt.Dimension
import java.awt.FlowLayout
import java.awt.GridBagConstraints
import java.awt.GridBagLayout
import java.awt.Insets
import java.awt.event.MouseEvent
import java.io.File
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import java.nio.file.Files
import java.nio.file.InvalidPathException
import java.nio.file.Path
import java.nio.file.Paths
import javax.swing.BorderFactory
import javax.swing.Box
import javax.swing.BoxLayout
import javax.swing.ButtonGroup
import javax.swing.JButton
import javax.swing.JComponent
import javax.swing.JFileChooser
import javax.swing.JFrame
import javax.swing.JLabel
import javax.swing.JOptionPane
import javax.swing.JPanel
import javax.swing.JProgressBar
import javax.swing.JRadioButton
import javax.swing.JScrollPane
import javax.swing.JTable
import javax.swing.JTextArea
import javax.swing.JTextField
import javax.swing.SwingUtilities
import javax.swing.UIManager
import javax.swing.event.DocumentEvent
import javax.swing.event.DocumentListener
import javax.net.ssl.SSLException
import javax.swing.filechooser.FileNameExtensionFilter
import javax.swing.table.AbstractTableModel
import javax.swing.table.DefaultTableCellRenderer
import kotlin.concurrent.thread

/** The installer's window. */
class InstallerFrame : JFrame("reminedog インストーラー ${Download.INSTALLER_VERSION}") {
    private val downloadChoice = JRadioButton("GitHub から最新版をダウンロードする", true)
    private val folderField = JTextField(DEFAULT_FOLDER)
    private val folderButton = JButton("参照…")
    private val existingChoice = JRadioButton("PC にある DLL を使う")
    private val fileField = JTextField()
    private val fileButton = JButton("参照…")
    private val argumentLabel = JLabel(" ")
    private val warningLabel = JLabel(" ")

    private val launchers = mutableListOf<Launcher>()
    private val rows = RowModel()
    private val table = object : JTable(rows) {
        override fun getToolTipText(event: MouseEvent): String? {
            val row = rowAtPoint(event.point)
            if (row < 0) return null
            val r = rows.rows[convertRowIndexToModel(row)]
            return if (columnAtPoint(event.point) == 1) r.instance.launcher.root.toString() else status(r).text
        }
    }
    private val addButton = JButton("ランチャーのフォルダを追加…")
    private val reloadButton = JButton("読み直す")
    private val installButton = JButton("インストール")
    private val uninstallButton = JButton("アンインストール")
    private val updateButton = JButton("最新版に更新")
    private val progress = JProgressBar().apply {
        isIndeterminate = true
        isVisible = false
    }
    private val log = JTextArea(6, 60)

    init {
        defaultCloseOperation = EXIT_ON_CLOSE
        contentPane = JPanel(BorderLayout(0, 12)).apply {
            border = BorderFactory.createEmptyBorder(16, 20, 16, 20)
            add(
                vertical(header(), Box.createVerticalStrut(14), dllSection()),
                BorderLayout.NORTH,
            )
            add(instanceSection(), BorderLayout.CENTER)
            add(bottomSection(), BorderLayout.SOUTH)
        }
        // Looks like the default button, without becoming one: Enter in a text field must not install.
        installButton.putClientProperty(
            FlatClientProperties.STYLE,
            "background: \$Button.default.background; foreground: \$Button.default.foreground;" +
                " borderColor: \$Button.default.borderColor; hoverBackground: \$Button.default.hoverBackground;" +
                " pressedBackground: \$Button.default.pressedBackground",
        )

        // A DLL already on the PC (maybe a test build) is used as it is, unless the user picks
        // the download.
        existingDll()?.let {
            fileField.text = it.toString()
            existingChoice.isSelected = true
        }
        fileField.putClientProperty(FlatClientProperties.PLACEHOLDER_TEXT, "$DEFAULT_FOLDER\\${Download.ASSET}")
        val onChange = object : DocumentListener {
            override fun insertUpdate(e: DocumentEvent) = pathChanged()
            override fun removeUpdate(e: DocumentEvent) = pathChanged()
            override fun changedUpdate(e: DocumentEvent) = pathChanged()
        }
        folderField.document.addDocumentListener(onChange)
        fileField.document.addDocumentListener(onChange)
        downloadChoice.addActionListener { pathChanged() }
        existingChoice.addActionListener { pathChanged() }
        folderButton.addActionListener { chooseFolder() }
        fileButton.addActionListener { chooseFile() }
        addButton.addActionListener { addLauncher() }
        reloadButton.addActionListener { reload() }
        installButton.addActionListener { changeInstances(installing = true) }
        uninstallButton.addActionListener { changeInstances(installing = false) }
        updateButton.addActionListener { updateDlls() }
        updateButton.toolTipText = "チェックしたインスタンスが読み込んでいる DLL を、その場所のまま最新版にする"

        Launchers.detect().forEach { addIfNew(it) }
        if (launchers.isEmpty()) logLine("ランチャーが見つかりませんでした。「ランチャーのフォルダを追加…」で選んでください。")
        if (!System.getProperty("os.name", "").startsWith("Windows")) logLine("reminedog は今のところ Windows でだけ動きます。")
        pathChanged()
        reload()

        pack()
        minimumSize = Dimension(780, 620)
        setLocationRelativeTo(null)
    }

    private fun header() = vertical(
        JLabel("reminedog インストーラー ${Download.INSTALLER_VERSION}").apply {
            putClientProperty(FlatClientProperties.STYLE_CLASS, "h2")
        },
        Box.createVerticalStrut(4),
        JLabel("ランチャーのインスタンスの JVM 引数に -agentpath を追加して、reminedog を読み込ませます。").apply {
            putClientProperty(FlatClientProperties.STYLE_CLASS, "light")
        },
    )

    private fun dllSection(): JComponent {
        ButtonGroup().apply {
            add(downloadChoice)
            add(existingChoice)
        }
        val panel = JPanel(GridBagLayout())
        panel.addAt(sectionTitle("1. reminedog の DLL"), 0, 0, width = 3, bottom = 6)
        panel.addAt(downloadChoice, 0, 1, width = 3)
        panel.addAt(JLabel("保存するフォルダ"), 0, 2, left = 26)
        panel.addAt(folderField, 1, 2, weightX = 1.0, fill = GridBagConstraints.HORIZONTAL)
        panel.addAt(folderButton, 2, 2)
        panel.addAt(existingChoice, 0, 3, width = 3, top = 6)
        panel.addAt(JLabel("DLL のファイル"), 0, 4, left = 26)
        panel.addAt(fileField, 1, 4, weightX = 1.0, fill = GridBagConstraints.HORIZONTAL)
        panel.addAt(fileButton, 2, 4)
        panel.addAt(argumentLabel, 0, 5, width = 3, top = 8)
        panel.addAt(warningLabel, 0, 6, width = 3)
        return panel
    }

    private fun instanceSection(): JComponent {
        table.autoCreateRowSorter = true
        table.fillsViewportHeight = true
        table.showHorizontalLines = true
        table.tableHeader.reorderingAllowed = false
        listOf(36, 200, 170, 80, 250).forEachIndexed { i, width -> table.columnModel.getColumn(i).preferredWidth = width }
        table.columnModel.getColumn(0).maxWidth = 40
        table.columnModel.getColumn(4).cellRenderer = StatusRenderer()

        val buttons = JPanel(FlowLayout(FlowLayout.LEFT, 0, 0)).apply {
            add(addButton)
            add(Box.createHorizontalStrut(6))
            add(reloadButton)
            add(Box.createHorizontalStrut(12))
            add(JLabel("作業の前にランチャーを閉じてください").apply { putClientProperty(FlatClientProperties.STYLE_CLASS, "small") })
        }
        return JPanel(BorderLayout(0, 6)).apply {
            add(sectionTitle("2. reminedog を入れるインスタンス"), BorderLayout.NORTH)
            add(JScrollPane(table).apply { preferredSize = Dimension(740, 220) }, BorderLayout.CENTER)
            add(buttons, BorderLayout.SOUTH)
        }
    }

    private fun bottomSection(): JComponent {
        log.isEditable = false
        log.lineWrap = true
        log.putClientProperty(FlatClientProperties.STYLE_CLASS, "small")
        val buttons = JPanel(FlowLayout(FlowLayout.RIGHT, 0, 0)).apply {
            add(progress)
            add(Box.createHorizontalStrut(12))
            add(uninstallButton)
            add(Box.createHorizontalStrut(8))
            add(updateButton)
            add(Box.createHorizontalStrut(8))
            add(installButton)
        }
        return JPanel(BorderLayout(0, 10)).apply {
            add(JScrollPane(log), BorderLayout.CENTER)
            add(buttons, BorderLayout.SOUTH)
        }
    }

    /** The DLL the arguments point at: the downloaded one or the chosen file. */
    private fun dllPath(): String {
        if (existingChoice.isSelected) return fileField.text.trim()
        val folder = folderField.text.trim().let { if (it.length > 3) it.trimEnd('\\', '/') else it }
        if (folder.isEmpty()) return ""
        return if (folder.endsWith("\\")) folder + Download.ASSET else "$folder\\${Download.ASSET}"
    }

    /** Why the DLL cannot be used for installing, or null. */
    private fun dllProblem(path: String): String? {
        AgentArg.problem(path)?.let { return it }
        if (existingChoice.isSelected) {
            try {
                if (!Files.isRegularFile(Paths.get(path))) return "ファイルがありません"
            } catch (e: InvalidPathException) {
                return "パスが正しくありません"
            }
        }
        return null
    }

    private fun pathChanged() {
        folderField.isEnabled = downloadChoice.isSelected
        folderButton.isEnabled = downloadChoice.isSelected
        fileField.isEnabled = existingChoice.isSelected
        fileButton.isEnabled = existingChoice.isSelected
        val path = dllPath()
        val problem = dllProblem(path)
        if (problem != null) {
            argumentLabel.foreground = color("Actions.Red")
            argumentLabel.text = problem
        } else {
            argumentLabel.foreground = UIManager.getColor("Label.foreground")
            argumentLabel.text = "追加する JVM 引数：${AgentArg.token(path)}"
        }
        warningLabel.foreground = color("Actions.Yellow")
        warningLabel.text = (if (problem == null) othersCanChange(path) else null) ?: " "
        rows.fireTableDataChanged()
    }

    /** A warning when other users of the PC could replace the DLL at [path] (or its folder). */
    private fun othersCanChange(path: String): String? {
        val file = try {
            Paths.get(path)
        } catch (e: InvalidPathException) {
            return null
        }
        // A folder still to be made is made for this user only.
        val exposed = listOfNotNull(file, file.parent).filter { Files.exists(it) }.any { Acl.othersCanWrite(it) }
        return if (exposed) {
            "注意：この場所は PC のほかのユーザーも書き換えられます。共用の PC なら、自分だけの場所（例：%LOCALAPPDATA%\\reminedog）にしてください"
        } else {
            null
        }
    }

    private fun chooseFolder() {
        val chooser = JFileChooser(folderField.text.trim()).apply {
            fileSelectionMode = JFileChooser.DIRECTORIES_ONLY
            dialogTitle = "DLL を保存するフォルダ"
        }
        if (chooser.showOpenDialog(this) == JFileChooser.APPROVE_OPTION) folderField.text = chooser.selectedFile.absolutePath
    }

    private fun chooseFile() {
        val chooser = JFileChooser().apply {
            fileField.text.trim().takeIf { it.isNotEmpty() }?.let { selectedFile = File(it) }
            fileFilter = FileNameExtensionFilter("DLL（*.dll）", "dll")
            dialogTitle = "reminedog の DLL"
        }
        if (chooser.showOpenDialog(this) == JFileChooser.APPROVE_OPTION) fileField.text = chooser.selectedFile.absolutePath
    }

    private fun addLauncher() {
        val chooser = JFileChooser().apply {
            fileSelectionMode = JFileChooser.DIRECTORIES_ONLY
            dialogTitle = "ランチャーのフォルダ"
        }
        if (chooser.showOpenDialog(this) != JFileChooser.APPROVE_OPTION) return
        val found = Launchers.find(chooser.selectedFile.toPath())
        if (found.isEmpty()) {
            JOptionPane.showMessageDialog(
                this,
                """
                ランチャーのフォルダではありません。次のフォルダを選んでください。

                ・Prism Launcher：メニューの「フォルダー」→「ランチャーのルート」で開くフォルダ
                ・MultiMC：MultiMC.exe のあるフォルダ
                ・MCSR Launcher：MCSRLauncher.jar のあるフォルダ
                ・公式ランチャー：.minecraft フォルダ
                """.trimIndent(),
                "ランチャーが見つかりません",
                JOptionPane.WARNING_MESSAGE,
            )
            return
        }
        for (launcher in found) {
            if (addIfNew(launcher)) {
                logLine("${launcher.label} を追加しました（${launcher.root}）")
            } else {
                logLine("${launcher.label}（${launcher.root}）はすでに一覧にあります")
            }
        }
        reload()
    }

    private fun addIfNew(launcher: Launcher): Boolean {
        if (launchers.any { it.key == launcher.key }) return false
        launchers += launcher
        return true
    }

    /** Reads the instances and their arguments again, keeping the checked ones checked. */
    private fun reload() {
        val checked = rows.rows.filter { it.checked }.map { it.key }.toSet()
        val list = mutableListOf<Row>()
        for (launcher in launchers) {
            val instances = try {
                launcher.instances()
            } catch (e: Exception) {
                logLine("${launcher.label}（${launcher.root}）を読めません：${message(e)}")
                continue
            }
            for (instance in instances) {
                val row = Row(instance)
                try {
                    row.args = instance.read()
                } catch (e: Exception) {
                    row.error = message(e)
                }
                row.checked = row.key in checked
                list += row
            }
        }
        // Instances of one launcher with the same name are told apart by their folder.
        list.groupBy { it.instance.launcher.key to it.instance.name }.values.filter { it.size > 1 }.flatten()
            .forEach { it.name = "${it.instance.name}（${it.instance.place}）" }
        rows.rows = list
        rows.fireTableDataChanged()
    }

    /** The checked rows, or null (after saying so) when there are none. */
    private fun checkedRows(): List<Row>? {
        val chosen = rows.rows.filter { it.checked }
        if (chosen.isEmpty()) {
            JOptionPane.showMessageDialog(this, "一覧でインスタンスにチェックを付けてください。", "インスタンスを選んでください", JOptionPane.INFORMATION_MESSAGE)
            return null
        }
        return chosen
    }

    private fun changeInstances(installing: Boolean) {
        val chosen = checkedRows() ?: return
        val dll = dllPath()
        if (installing) {
            val problem = dllProblem(dll)
            if (problem != null) {
                JOptionPane.showMessageDialog(
                    this,
                    "$problem。\n\nJVM 引数は空白で区切られるので、パスに空白も日本語も含まない場所（例：$DEFAULT_FOLDER）を選んでください。",
                    "DLL の場所を使えません",
                    JOptionPane.WARNING_MESSAGE,
                )
                return
            }
        }

        val download = installing && downloadChoice.isSelected
        runInBackground {
            if (!launchersClosed(chosen)) return@runInBackground null
            if (download) {
                val target = Paths.get(dll)
                val release = try {
                    logLine("最新のリリースを確認しています…")
                    Download.latest()
                } catch (e: Exception) {
                    logLine("最新のリリースを確認できませんでした：${message(e)}")
                    return@runInBackground Outcome.error(
                        "ダウンロードできません",
                        "最新のリリースを確認できませんでした（${message(e)}）。インスタンスは変えていません。\n" +
                            "DLL を自分で用意したなら「PC にある DLL を使う」を選んでください。",
                    )
                }
                if (Download.differs(release, target) &&
                    !ask(
                        "DLL を置き換えます",
                        "$dll にはすでに別の DLL があります（${release.tag} とは違うファイルです）。\n" +
                            "${release.tag} で置き換えますか？\n\n" +
                            "試験版や自分でビルドした DLL を使っているなら「いいえ」を押して、「PC にある DLL を使う」を選んでください。",
                    )
                ) {
                    return@runInBackground Outcome.done("やめました。DLL もインスタンスも変えていません。")
                }
                try {
                    val saved = Download.save(release, target) { logDownloading(release) }
                    logLine(if (saved) "$dll に ${release.tag} を保存しました" else "$dll はすでに ${release.tag} と同じです")
                } catch (e: Download.PlaceException) {
                    logLine("DLL を置けませんでした：${message(e)}")
                    return@runInBackground Outcome.error(
                        "DLL を置けません",
                        "ダウンロードした DLL を $dll に置けませんでした。インスタンスは変えていません。\n" +
                            "その DLL を読み込んでいるゲームを閉じてから、もう一度試してください。",
                    )
                } catch (e: Exception) {
                    logLine("ダウンロードできませんでした：${message(e)}")
                    return@runInBackground Outcome.error(
                        "ダウンロードできません",
                        "DLL をダウンロードできませんでした（${message(e)}）。インスタンスは変えていません。\n" +
                            "DLL を自分で用意したなら「PC にある DLL を使う」を選んでください。",
                    )
                }
            }
            var failures = 0
            for (row in chosen) {
                try {
                    val change = if (installing) row.instance.install(dll) else row.instance.remove()
                    logLine("${row.title}：${change.message}")
                } catch (e: Exception) {
                    failures++
                    logLine("${row.title}：失敗しました（${message(e)}）")
                }
            }
            if (!installing) logLine("DLL のファイルは消していません。いらなければ消してください。")
            when {
                failures > 0 -> Outcome.warning("失敗があります", "一部のインスタンスで失敗しました。下のログを見てください。")
                installing -> Outcome.done("完了しました。ランチャーからゲームを起動してください。")
                else -> Outcome.done("完了しました。")
            }
        }
    }

    /**
     * Replaces the DLLs the checked instances load with the latest release, where they are: the
     * instances' arguments stay as they are. A DLL that several instances load is replaced once.
     */
    private fun updateDlls() {
        val chosen = checkedRows() ?: return
        // The DLL files by pathKey, with the instances that load each.
        val dlls = LinkedHashMap<String, InstalledDll>()
        for (row in chosen) {
            val args = row.args
            if (args == null) {
                logLine("${row.title}：設定を読めないので飛ばします")
                continue
            }
            val agents = args.agents
            if (agents.isEmpty()) logLine("${row.title}：reminedog が入っていないので飛ばします")
            for (agent in agents) {
                if (AgentArg.isDrivePath(agent.path)) {
                    dlls.getOrPut(AgentArg.pathKey(agent.path)) { InstalledDll(agent.path) }.users += row.title
                } else {
                    logLine("${row.title}：DLL のパス（${agent.path}）がドライブから始まらないので、置き換えられません")
                }
            }
        }
        if (dlls.isEmpty()) {
            JOptionPane.showMessageDialog(
                this,
                "チェックしたインスタンスには、置き換えられる reminedog が入っていません。",
                "置き換える DLL がありません",
                JOptionPane.INFORMATION_MESSAGE,
            )
            return
        }
        val list = dlls.values.joinToString("\n") { "・${it.path}（${it.users.joinToString("、")}）" }
        runInBackground {
            val targets = dlls.values.map { Paths.get(it.path) }
            val release = try {
                logLine("最新のリリースを確認しています…")
                Download.latest()
            } catch (e: Exception) {
                logLine("最新のリリースを確認できませんでした：${message(e)}")
                return@runInBackground Outcome.error("ダウンロードできません", "最新のリリースを確認できませんでした（${message(e)}）。DLL は変えていません。")
            }
            val go = ask(
                "最新版に更新",
                "次の DLL を GitHub の最新版（${release.tag}）に置き換えます。JVM 引数は変えません。\n\n$list\n\n" +
                    "その DLL を読み込んでいるゲームは、閉じてから「はい」を押してください。",
            )
            if (!go) return@runInBackground null
            val results = try {
                Download.replaceAll(release, targets) { logDownloading(release) }
            } catch (e: Exception) {
                logLine("ダウンロードできませんでした：${message(e)}")
                return@runInBackground Outcome.error("ダウンロードできません", "最新版をダウンロードできませんでした（${message(e)}）。DLL は変えていません。")
            }
            for (result in results) {
                logLine(
                    when (result) {
                        is Download.Replaced.Updated -> "${result.path} を ${release.tag} に置き換えました"
                        is Download.Replaced.AlreadyLatest -> "${result.path} はすでに ${release.tag} と同じです"
                        is Download.Replaced.Failed -> "${result.path} を置き換えられませんでした（${result.reason}）"
                    },
                )
            }
            when {
                results.any { it is Download.Replaced.Failed } ->
                    Outcome.warning(
                        "失敗があります",
                        "置き換えられなかった DLL があります。その DLL を読み込んでいるゲームを閉じてから、もう一度試してください（下のログに詳しく出ています）。",
                    )
                results.all { it is Download.Replaced.AlreadyLatest } -> Outcome.done("どの DLL もすでに最新版（${release.tag}）です。")
                else -> Outcome.done("reminedog を最新版（${release.tag}）にしました。")
            }
        }
    }

    private fun logDownloading(release: Download.Release) {
        logLine("reminedog ${release.tag} をダウンロードしています（${megabytes(release.size)} MB）…")
    }

    /**
     * Runs [task] off the event thread with the window busy, then shows the dialog it returns
     * (none for null: the user stopped it). The window is usable again whatever happens.
     */
    private fun runInBackground(task: () -> Outcome?) {
        setBusy(true)
        thread(isDaemon = true, name = "reminedog-installer") {
            var outcome: Outcome? = null
            try {
                outcome = task()
            } catch (e: Throwable) {
                outcome = Outcome.error("失敗しました", message(e))
                runCatching { logLine("失敗しました：${message(e)}") }
            } finally {
                val shown = outcome
                SwingUtilities.invokeLater {
                    setBusy(false)
                    reload()
                    shown?.let { JOptionPane.showMessageDialog(this@InstallerFrame, it.message, it.title, it.type) }
                }
            }
        }
    }

    /** Asks a yes-or-no question from a background task (the dialog shows on the event thread). */
    private fun ask(title: String, message: String): Boolean {
        var answer = JOptionPane.NO_OPTION
        SwingUtilities.invokeAndWait {
            answer = JOptionPane.showConfirmDialog(this, message, title, JOptionPane.YES_NO_OPTION, JOptionPane.WARNING_MESSAGE)
        }
        return answer == JOptionPane.YES_OPTION
    }

    /**
     * Until the launchers of [chosen] are closed, asks the user to close them (from a background
     * task: listing the programs takes a moment). False when they stop.
     */
    private fun launchersClosed(chosen: List<Row>): Boolean {
        while (true) {
            val processes = Processes.running()
            val open = chosen.map { it.instance.launcher }
                .distinctBy { it.key }
                .filter { it.isRunning(processes) }
                .map { it.label }
                .distinct()
            if (open.isEmpty()) return true
            val options = arrayOf("閉じた", "このまま続ける", "やめる")
            var answer = JOptionPane.CLOSED_OPTION
            SwingUtilities.invokeAndWait {
                answer = JOptionPane.showOptionDialog(
                    this,
                    "次のランチャーが起動しています：${open.joinToString("、")}\n\n" +
                        "起動したままだと、ランチャーが設定を書き戻して変更が消えることがあります。\n" +
                        "ランチャーを閉じてから「閉じた」を押してください。",
                    "ランチャーが起動しています",
                    JOptionPane.DEFAULT_OPTION,
                    JOptionPane.WARNING_MESSAGE,
                    null,
                    options,
                    options[0],
                )
            }
            when (answer) {
                0 -> continue
                1 -> return true
                else -> return false
            }
        }
    }

    private fun setBusy(busy: Boolean) {
        listOf(installButton, uninstallButton, updateButton, addButton, reloadButton, table, downloadChoice, existingChoice)
            .forEach { it.isEnabled = !busy }
        progress.isVisible = busy
        if (busy) {
            listOf(folderField, folderButton, fileField, fileButton).forEach { it.isEnabled = false }
        } else {
            pathChanged()
        }
        cursor = if (busy) Cursor.getPredefinedCursor(Cursor.WAIT_CURSOR) else Cursor.getDefaultCursor()
        // Closing the window ends the program, so not in the middle of writing the files.
        defaultCloseOperation = if (busy) DO_NOTHING_ON_CLOSE else EXIT_ON_CLOSE
    }

    /** Adds a line to the log (from any thread). */
    private fun logLine(line: String) {
        val append = {
            log.append(line + "\n")
            log.caretPosition = log.document.length
        }
        if (SwingUtilities.isEventDispatchThread()) append() else SwingUtilities.invokeLater(append)
    }

    private fun status(row: Row): Status {
        row.error?.let { return Status(StatusKind.ERROR, "読めません：$it") }
        val args = row.args ?: return Status(StatusKind.ERROR, "読めません")
        val found = args.agents
        if (found.isEmpty()) return Status(StatusKind.ABSENT, "未導入")
        val shared = if (args.shared) "（ランチャー全体の設定）" else ""
        val dll = dllPath()
        val other = found.firstOrNull { !AgentArg.samePath(it.path, dll) }
        return if (other != null) {
            Status(StatusKind.OTHER, "別の DLL：${other.path}$shared")
        } else {
            Status(StatusKind.INSTALLED, "導入済み$shared")
        }
    }

    /** A DLL file that checked instances load, and the titles of those instances. */
    private class InstalledDll(val path: String) {
        val users = mutableListOf<String>()
    }

    /** The dialog a background task ends with. */
    private class Outcome(val title: String, val message: String, val type: Int) {
        companion object {
            fun done(message: String) = Outcome("reminedog インストーラー", message, JOptionPane.INFORMATION_MESSAGE)

            fun warning(title: String, message: String) = Outcome(title, message, JOptionPane.WARNING_MESSAGE)

            fun error(title: String, message: String) = Outcome(title, message, JOptionPane.ERROR_MESSAGE)
        }
    }

    /** A line of the instance list. */
    private class Row(val instance: Instance) {
        var checked = false
        var args: Args? = null
        var error: String? = null

        /** The instance's name, with its folder when another one has the same name. */
        var name = instance.name
        val key: String get() = instance.key
        val title: String get() = "${instance.launcher.label} / $name"
    }

    private enum class StatusKind { INSTALLED, OTHER, ABSENT, ERROR }

    private class Status(val kind: StatusKind, val text: String) {
        override fun toString() = text
    }

    private inner class RowModel : AbstractTableModel() {
        private val columns = arrayOf("", "ランチャー", "インスタンス", "バージョン", "状態")
        var rows: List<Row> = emptyList()

        override fun getRowCount() = rows.size

        override fun getColumnCount() = columns.size

        override fun getColumnName(column: Int) = columns[column]

        override fun getColumnClass(column: Int): Class<*> = if (column == 0) Boolean::class.javaObjectType else String::class.java

        override fun isCellEditable(row: Int, column: Int) = column == 0

        override fun getValueAt(index: Int, column: Int): Any {
            val row = rows[index]
            return when (column) {
                0 -> row.checked
                1 -> row.instance.launcher.label
                2 -> row.name
                3 -> row.instance.detail
                else -> status(row)
            }
        }

        override fun setValueAt(value: Any?, index: Int, column: Int) {
            if (column == 0) {
                rows[index].checked = value == true
                fireTableCellUpdated(index, column)
            }
        }
    }

    /** Colors the status: green when installed, red when the instance cannot be read, and so on. */
    private class StatusRenderer : DefaultTableCellRenderer() {
        override fun getTableCellRendererComponent(
            table: JTable,
            value: Any?,
            isSelected: Boolean,
            hasFocus: Boolean,
            row: Int,
            column: Int,
        ): Component {
            super.getTableCellRendererComponent(table, value, isSelected, hasFocus, row, column)
            val kind = (value as? Status)?.kind
            if (!isSelected && kind != null) {
                foreground = when (kind) {
                    StatusKind.INSTALLED -> color("Actions.Green")
                    StatusKind.OTHER -> color("Actions.Yellow")
                    StatusKind.ABSENT -> UIManager.getColor("Label.disabledForeground") ?: table.foreground
                    StatusKind.ERROR -> color("Actions.Red")
                }
            }
            return this
        }
    }

    companion object {
        /** Where the DLL went before %LOCALAPPDATA% (and still goes where that path cannot be used). */
        private const val FALLBACK_FOLDER = "C:\\reminedog"

        /**
         * `%LOCALAPPDATA%\reminedog`, where only the user can change the DLL, when its path can be
         * put in JVM arguments (no spaces, ASCII only); else [FALLBACK_FOLDER].
         */
        private val DEFAULT_FOLDER: String =
            System.getenv("LOCALAPPDATA")
                ?.takeIf { it.isNotEmpty() }
                ?.let { it.trimEnd('\\') + "\\reminedog" }
                ?.takeIf { AgentArg.problem("$it\\${Download.ASSET}") == null }
                ?: FALLBACK_FOLDER

        /** A DLL already in one of the usual places. */
        private fun existingDll(): Path? =
            listOf(DEFAULT_FOLDER, FALLBACK_FOLDER).distinct().firstNotNullOfOrNull { folder ->
                try {
                    Paths.get(folder, Download.ASSET).takeIf { Files.isRegularFile(it) }
                } catch (e: InvalidPathException) {
                    null
                }
            }

        /** One of FlatLaf's icon colors (Actions.Green, Actions.Red, ...). */
        private fun color(key: String): Color = UIManager.getColor(key) ?: UIManager.getColor("Label.foreground")

        private fun sectionTitle(text: String) =
            JLabel(text).apply { putClientProperty(FlatClientProperties.STYLE_CLASS, "h3") }

        private fun vertical(vararg components: Component) = JPanel().apply {
            layout = BoxLayout(this, BoxLayout.Y_AXIS)
            for (component in components) {
                (component as? JComponent)?.alignmentX = Component.LEFT_ALIGNMENT
                add(component)
            }
        }

        private fun JPanel.addAt(
            component: Component,
            x: Int,
            y: Int,
            width: Int = 1,
            weightX: Double = 0.0,
            fill: Int = GridBagConstraints.NONE,
            top: Int = 2,
            left: Int = 0,
            bottom: Int = 2,
        ) {
            val constraints = GridBagConstraints().apply {
                gridx = x
                gridy = y
                gridwidth = width
                weightx = weightX
                this.fill = fill
                anchor = GridBagConstraints.WEST
                insets = Insets(top, left, bottom, 8)
            }
            add(component, constraints)
        }

        /** What went wrong, in words for the user where Java's own are not. */
        private fun message(e: Throwable): String =
            when (e) {
                is UnknownHostException -> "${e.message} が見つかりません（インターネットにつながっているか確かめてください）"
                is ConnectException -> "接続できません（インターネットにつながっているか確かめてください）"
                is SocketTimeoutException -> "応答がありません（時間切れ。しばらくしてから試してください）"
                is SSLException ->
                    "安全な接続を確かめられません（PC の時刻と、セキュリティソフトが通信を検査していないかを確かめてください）"
                else -> e.message?.takeIf { it.isNotEmpty() } ?: e.javaClass.simpleName
            }

        private fun megabytes(bytes: Long): String = "%.1f".format(bytes / 1048576.0)
    }
}
