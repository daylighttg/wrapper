package com.worldobservationlog.wrapperlite

import android.content.Context
import android.os.Build
import android.util.Log
import java.io.File
import java.io.FileOutputStream

/**
 * Extracts the QEMU binary, firmware, libraries and guest files bundled in the
 * APK (assets/qemu) to filesDir/qemu, where `wrapper gui --assets-dir` finds
 * them. Runs once per APK version. The data disk is created by wrapper itself.
 */
class QemuAssetManager(private val context: Context) {

    companion object {
        private const val TAG = "QemuAssetManager"
        private const val PREFS = "wrapper_assets"
        private const val KEY_VERSION = "extracted_version_code"
    }

    val qemuDir: File = File(context.filesDir, "qemu")

    private fun versionCode(): Long {
        val info = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            context.packageManager.getPackageInfo(context.packageName, android.content.pm.PackageManager.PackageInfoFlags.of(0))
        } else {
            @Suppress("DEPRECATION")
            context.packageManager.getPackageInfo(context.packageName, 0)
        }
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            info.longVersionCode
        } else {
            @Suppress("DEPRECATION")
            info.versionCode.toLong()
        }
    }

    /** Returns false only when extraction was needed and failed. */
    fun extractIfNeeded(): Boolean {
        val bundled = context.assets.list("qemu") ?: return true
        if (bundled.isEmpty()) return true
        val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val version = versionCode()
        if (prefs.getLong(KEY_VERSION, -1L) == version && File(qemuDir, "vmlinuz-lite-qemu").exists()) {
            return true
        }
        return try {
            copyAsset("qemu", qemuDir)
            fixPermissionsAndLibraries(qemuDir)
            prefs.edit().putLong(KEY_VERSION, version).apply()
            true
        } catch (e: Exception) {
            Log.e(TAG, "failed to extract bundled assets", e)
            false
        }
    }

    private fun copyAsset(src: String, dst: File) {
        val children = context.assets.list(src) ?: emptyArray()
        if (children.isNotEmpty()) {
            dst.mkdirs()
            for (child in children) {
                // AAPT strips ".gz" from asset names; restore the canonical one.
                val name = if (child == "lite-initramfs.cpio") "lite-initramfs.cpio.gz" else child
                copyAsset("$src/$child", File(dst, name))
            }
            return
        }
        // Never replace an existing data disk (accounts live there).
        if (dst.name == "data.img" && dst.exists()) return
        dst.parentFile?.mkdirs()
        context.assets.open(src).use { input ->
            FileOutputStream(dst).use { output -> input.copyTo(output) }
        }
    }

    private fun isElf(f: File): Boolean = try {
        f.inputStream().use { s ->
            val h = ByteArray(4)
            s.read(h) == 4 && h[0] == 0x7f.toByte() && h[1] == 'E'.code.toByte() && h[2] == 'L'.code.toByte() && h[3] == 'F'.code.toByte()
        }
    } catch (e: Exception) {
        false
    }

    /**
     * Zip/asset round trips turn library symlinks into small text files that
     * contain the link target; replace them with the real ELF, then make every
     * binary and library executable.
     */
    private fun fixPermissionsAndLibraries(dir: File) {
        dir.walkTopDown().filter { it.isFile && it.name.contains(".so") && it.length() in 1..1024 && !isElf(it) }.forEach { f ->
            var target = File(f.parentFile, f.readText().trim())
            var hops = 0
            while (target.isFile && !isElf(target) && target.length() in 1..1024 && hops++ < 8) {
                target = File(f.parentFile, target.readText().trim())
            }
            if (target.isFile && isElf(target)) target.copyTo(f, overwrite = true)
        }
        dir.walkTopDown().filter { it.isFile }.forEach { f ->
            val parent = f.parentFile?.name ?: ""
            if (parent == "bin" || parent == "lib" || f.name.contains(".so") || f.name.startsWith("qemu-system-")) {
                f.setReadable(true, false)
                f.setExecutable(true, false)
            }
        }
    }
}
