package io.github.blendonl.amux

import java.io.IOException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class SymlinkListTest {
    @Test
    fun `reads the target before the arrow and the path after it`() {
        assertEquals(
            listOf(
                Symlink("dash", "bin/sh"),
                Symlink("../../applib/libu_bin_zsh.so", "bin/zsh"),
                Symlink("/system/bin/linker64", "bin/linker64"),
            ),
            SymlinkList.parse("dash←./bin/sh\n../../applib/libu_bin_zsh.so←./bin/zsh\n/system/bin/linker64←./bin/linker64\n"),
        )
    }

    @Test
    fun `accepts a path without the leading dot slash`() {
        assertEquals(listOf(Symlink("less", "bin/pager")), SymlinkList.parse("less←bin/pager"))
    }

    @Test
    fun `keeps unicode in targets and paths`() {
        assertEquals(
            listOf(Symlink("../données/café ☕", "share/ünïcode/日本語")),
            SymlinkList.parse("../données/café ☕←./share/ünïcode/日本語\n"),
        )
    }

    @Test
    fun `skips empty lines`() {
        assertEquals(listOf(Symlink("nano", "bin/editor")), SymlinkList.parse("\nnano←./bin/editor\n\n"))
    }

    @Test
    fun `rejects a line without exactly one arrow`() {
        assertThrows(IOException::class.java) { SymlinkList.parse("dash ./bin/sh\n") }
        assertThrows(IOException::class.java) { SymlinkList.parse("a←b←./bin/sh\n") }
    }

    @Test
    fun `rejects an empty target`() {
        assertThrows(IOException::class.java) { SymlinkList.parse("←./bin/sh\n") }
    }

    @Test
    fun `rejects paths that leave the prefix`() {
        listOf("../escape", "/data/local/tmp/x", "./bin/../../x", "bin//sh", "./", "").forEach { path ->
            assertThrows(path, IOException::class.java) { SymlinkList.parse("dash←$path\n") }
        }
    }
}
