package io.github.blendonl.amux

import java.io.File
import java.io.IOException
import java.io.RandomAccessFile
import java.nio.ByteBuffer
import java.nio.ByteOrder

object ZipModes {
    private const val END_SIGNATURE = 0x06054b50
    private const val END_SIZE = 22
    private const val MAX_COMMENT = 0xffff
    private const val ENTRY_SIGNATURE = 0x02014b50
    private const val ENTRY_SIZE = 46
    private const val UNIX = 3
    private const val ZIP64_COUNT = 0xffff
    private const val ZIP64_OFFSET = 0xffffffffL
    private const val PERMISSION_BITS = 0b111_111_111

    private class CentralDirectory(val entries: Int, val size: Int, val offset: Long)

    fun read(file: File): Map<String, Int> =
        RandomAccessFile(file, "r").use { zip ->
            val directory = centralDirectory(zip) ?: throw IOException("${file.path} is not a zip archive")
            if (directory.entries == ZIP64_COUNT || directory.offset == ZIP64_OFFSET) {
                throw IOException("${file.path} is a zip64 archive")
            }
            val bytes = ByteArray(directory.size)
            zip.seek(directory.offset)
            zip.readFully(bytes)
            modes(bytes, directory.entries) ?: throw IOException("${file.path} has a damaged central directory")
        }

    private fun centralDirectory(zip: RandomAccessFile): CentralDirectory? {
        val tailSize = minOf(zip.length(), (END_SIZE + MAX_COMMENT).toLong()).toInt()
        val tail = ByteArray(tailSize)
        zip.seek(zip.length() - tailSize)
        zip.readFully(tail)
        val buffer = ByteBuffer.wrap(tail).order(ByteOrder.LITTLE_ENDIAN)
        val end = (tailSize - END_SIZE downTo 0).firstOrNull { buffer.getInt(it) == END_SIGNATURE } ?: return null
        return CentralDirectory(
            entries = buffer.getShort(end + 10).toInt() and 0xffff,
            size = buffer.getInt(end + 12),
            offset = buffer.getInt(end + 16).toLong() and 0xffffffffL,
        )
    }

    private fun modes(bytes: ByteArray, entries: Int): Map<String, Int>? {
        val buffer = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN)
        var position = 0
        val modes = mutableMapOf<String, Int>()
        repeat(entries) {
            if (position + ENTRY_SIZE > bytes.size || buffer.getInt(position) != ENTRY_SIGNATURE) return null
            val madeBy = buffer.getShort(position + 4).toInt() and 0xffff
            val nameLength = buffer.getShort(position + 28).toInt() and 0xffff
            val extraLength = buffer.getShort(position + 30).toInt() and 0xffff
            val commentLength = buffer.getShort(position + 32).toInt() and 0xffff
            val attributes = buffer.getInt(position + 38)
            if (position + ENTRY_SIZE + nameLength > bytes.size) return null
            val name = String(bytes, position + ENTRY_SIZE, nameLength, Charsets.UTF_8)
            val mode = (attributes ushr 16) and PERMISSION_BITS
            if (madeBy ushr 8 == UNIX && mode != 0) modes[name] = mode
            position += ENTRY_SIZE + nameLength + extraLength + commentLength
        }
        return modes
    }
}
