package reminedog.installer

import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.attribute.AclEntry
import java.nio.file.attribute.AclEntryFlag
import java.nio.file.attribute.AclEntryPermission
import java.nio.file.attribute.AclEntryType
import java.nio.file.attribute.AclFileAttributeView

/**
 * DLL を変えてよい人。ゲームは起動した人の権限で DLL を動かすので、PC のほかの利用者が置き換え
 * られる DLL（C:\ の直下のフォルダは、サインインしたどの利用者も中に置くものを変えられる）は、
 * その人のコードを動かす。この一覧があるのは Windows だけ。ほかでは何も確かめず、変えない。
 */
object Acl {
    /** PC のほかの利用者を含むグループ（ドメインの後の名前、小文字）。 */
    private val OTHERS = setOf("everyone", "users", "authenticated users")

    private val WRITE = setOf(
        AclEntryPermission.WRITE_DATA,
        AclEntryPermission.APPEND_DATA,
        AclEntryPermission.DELETE,
        AclEntryPermission.DELETE_CHILD,
        AclEntryPermission.WRITE_ACL,
        AclEntryPermission.WRITE_OWNER,
    )

    /** PC のほかの利用者が [path] を変えられるか（分からなければ false）。 */
    fun othersCanWrite(path: Path): Boolean {
        val view = Files.getFileAttributeView(path, AclFileAttributeView::class.java) ?: return false
        val acl = try {
            view.acl
        } catch (e: IOException) {
            return false
        } catch (e: SecurityException) {
            return false
        }
        return acl.any { entry ->
            entry.type() == AclEntryType.ALLOW &&
                entry.principal().name.substringAfterLast('\\').lowercase() in OTHERS &&
                entry.permissions().any { it in WRITE }
        }
    }

    /**
     * [dir] とその中のものに触れるのを、[dir] の持ち主（作った利用者）・SYSTEM・管理者だけにする。
     * 作ったばかりのフォルダのため。失敗したらそのまま。
     */
    fun restrict(dir: Path) {
        val view = Files.getFileAttributeView(dir, AclFileAttributeView::class.java) ?: return
        try {
            val lookup = dir.fileSystem.userPrincipalLookupService
            val principals = listOfNotNull(
                view.owner,
                runCatching { lookup.lookupPrincipalByName("NT AUTHORITY\\SYSTEM") }.getOrNull(),
                runCatching { lookup.lookupPrincipalByName("BUILTIN\\Administrators") }.getOrNull(),
            ).distinct()
            view.acl = principals.map { principal ->
                AclEntry.newBuilder()
                    .setType(AclEntryType.ALLOW)
                    .setPrincipal(principal)
                    .setPermissions(AclEntryPermission.values().toSet())
                    .setFlags(AclEntryFlag.FILE_INHERIT, AclEntryFlag.DIRECTORY_INHERIT)
                    .build()
            }
        } catch (e: IOException) {
            // 親のフォルダが与えたままにする。ウィンドウはそういうフォルダについて警告する。
        } catch (e: SecurityException) {
        }
    }
}
