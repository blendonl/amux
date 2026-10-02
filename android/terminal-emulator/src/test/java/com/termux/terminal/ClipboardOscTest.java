package com.termux.terminal;

import java.nio.charset.StandardCharsets;
import java.util.Base64;
import java.util.List;

public class ClipboardOscTest extends TerminalTestCase {

    private static final int MIB = 1024 * 1024;

    @Override
    protected void setUp() throws Exception {
        super.setUp();
        withTerminalSized(4, 2);
    }

    private static String copy(String text) {
        return "\033]52;c;" + Base64.getEncoder().encodeToString(text.getBytes(StandardCharsets.UTF_8));
    }

    private static String textOfBytes(int length) {
        String unit = "héllo, 世界\n";
        int unitLength = unit.getBytes(StandardCharsets.UTF_8).length;
        String text = unit.repeat(length / unitLength) + "a".repeat(length % unitLength);
        assertEquals(length, text.getBytes(StandardCharsets.UTF_8).length);
        return text;
    }

    public void testCopiesAMebibyteOfText() {
        String text = textOfBytes(MIB);
        enterString(copy(text) + "\007");
        assertEquals(List.of(text), mOutput.clipboardPuts);
        assertLinesAre("    ", "    ");
    }

    public void testCopiesWithAStringTerminator() {
        enterString(copy("hi") + "\033\\");
        assertEquals(List.of("hi"), mOutput.clipboardPuts);
    }

    public void testDropsAnOscTooLongToCopyWithoutPrintingIt() {
        String tooLong = textOfBytes(2 * MIB);
        enterString(copy(tooLong) + "\007ok");
        enterString(copy(tooLong) + "\033\\hi");
        assertEquals(List.of(), mOutput.clipboardPuts);
        assertLinesAre("okhi", "    ");

        enterString(copy("next") + "\007");
        assertEquals(List.of("next"), mOutput.clipboardPuts);
    }

}
