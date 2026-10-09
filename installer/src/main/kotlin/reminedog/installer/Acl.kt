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
 * Who may change the DLL. The game runs the DLL with the rights of whoever starts it, so one
 * that other users of the PC can replace (a folder right under C:\ lets every signed-in user
 * change what is put in it) runs their code. Only Windows has these lists; elsewhere nothing
 * is checked or changed.
 */
object Acl {
    /** Groups that hold other users of the PC (the name after the domain, in lowercase). */
    private val OTHERS = setOf("everyone", "users", "authenticated users")

    private val WRITE = setOf(
        AclEntryPermission.WRITE_DATA,
        AclEntryPermission.APPEND_DATA,
        AclEntryPermission.DELETE,
        AclEntryPermission.DELETE_CHILD,
        AclEntryPermission.WRITE_ACL,
        AclEntryPermission.WRITE_OWNER,
    )

    /** Whether other users of the PC may change [path] (false when it cannot be told). */
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
     * Lets only the owner of [dir] (the user who made it), SYSTEM and the administrators at it and
     * at what goes in it. For a folder just made; a failure leaves it as it was.
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
            // Kept as the parent folder gave it; the window warns about such folders.
        } catch (e: SecurityException) {
        }
    }
}
